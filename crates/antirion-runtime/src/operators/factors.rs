//! Second factors of local accounts: TOTP authenticators (secrets sealed at rest, each code accepted
//! once), recovery codes (kept as peppered verifiers, each usable once), enrolment, listing, removal
//! and the administrator's reset.

use antirion_core::UtcMicros;
use antirion_identity::otp;
use antirion_identity::verifier::random_id;
use serde::{Deserialize, Serialize};

use super::{FactorKind, IdentityError, kind};
use crate::Runtime;

/// A second factor presented at sign-in or to confirm a sensitive change.
#[derive(Clone, Debug)]
pub enum Proof {
    Totp(String),
    RecoveryCode(String),
    Webauthn(serde_json::Value),
}

/// A factor as its list shows it.
#[derive(Clone, Debug)]
pub struct Factor {
    pub id: String,
    pub kind: FactorKind,
    pub name: Option<String>,
    pub created_at: UtcMicros,
    pub last_used_at: Option<UtcMicros>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct FactorRecord {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) confirmed: bool,
    pub(crate) created_us: i64,
    #[serde(default)]
    pub(crate) last_used_us: Option<i64>,
    /// TOTP: the sealed secret, and the last step a code was accepted for.
    #[serde(default)]
    pub(crate) secret: Option<String>,
    #[serde(default)]
    pub(crate) last_step: Option<i64>,
    /// WebAuthn: the registered credential, or the registration in progress.
    #[serde(default)]
    pub(crate) webauthn: Option<serde_json::Value>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RecoveryRecord {
    #[serde(default)]
    verifiers: Vec<String>,
}

fn seal_location(id: &str) -> String {
    format!("factor/{id}")
}

fn unix_seconds(t: UtcMicros) -> i64 {
    t.0.div_euclid(1_000_000)
}

impl Runtime {
    pub(crate) async fn factor_records(
        &self,
        user: &str,
    ) -> Result<Vec<(String, FactorRecord)>, IdentityError> {
        let mut out = Vec::new();
        for rec in self.store.records(kind::FACTOR, Some(user)).await? {
            if let Ok(f) = serde_json::from_str::<FactorRecord>(&rec.body) {
                out.push((rec.id, f));
            }
        }
        Ok(out)
    }

    pub(crate) async fn confirmed_factors(
        &self,
        user: &str,
    ) -> Result<Vec<(String, FactorRecord)>, IdentityError> {
        Ok(self
            .factor_records(user)
            .await?
            .into_iter()
            .filter(|(_, f)| f.confirmed)
            .collect())
    }

    /// `(user, kind)` of every confirmed factor of every user.
    pub(crate) async fn confirmed_factor_kinds(&self) -> Vec<(String, String)> {
        self.store
            .records(kind::FACTOR, None)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(|r| {
                let f: FactorRecord = serde_json::from_str(&r.body).ok()?;
                f.confirmed.then_some((r.user, f.kind))
            })
            .collect()
    }

    /// The user's confirmed factors, and how many recovery codes remain.
    pub async fn factors(&self, user: &str) -> Result<(Vec<Factor>, usize), IdentityError> {
        let factors = self
            .confirmed_factors(user)
            .await?
            .into_iter()
            .filter_map(|(id, f)| {
                Some(Factor {
                    kind: FactorKind::parse(&f.kind)?,
                    id,
                    name: f.name,
                    created_at: UtcMicros(f.created_us),
                    last_used_at: f.last_used_us.map(UtcMicros),
                })
            })
            .collect();
        let remaining = self
            .read_record::<RecoveryRecord>(kind::RECOVERY, user)
            .await?
            .map_or(0, |(_, r)| r.verifiers.len());
        Ok((factors, remaining))
    }

    /// Starts enrolling a TOTP authenticator: the factor id, the base32 secret and its `otpauth://`
    /// URI, shown once.
    pub async fn enrol_totp(&self, user: &str) -> Result<(String, String, String), IdentityError> {
        let snap = self.gens.load();
        if !snap.identity.console.allows(FactorKind::Totp) {
            return Err(IdentityError::Invalid("totp is not an allowed factor"));
        }
        let secret = otp::new_secret().map_err(|_| IdentityError::Internal)?;
        let id = new_factor_id()?;
        let sealed = self
            .sealer
            .seal(&seal_location(&id), &secret)
            .map_err(|_| IdentityError::Internal)?;
        let record = FactorRecord {
            kind: "totp".to_owned(),
            name: None,
            confirmed: false,
            created_us: self.now().0,
            last_used_us: None,
            secret: Some(sealed),
            last_step: None,
            webauthn: None,
        };
        self.write_record(kind::FACTOR, &id, user, None, &record).await?;
        let account = snap
            .identity
            .users
            .get(user)
            .and_then(|u| u.email.clone())
            .unwrap_or_else(|| user.to_owned());
        let issuer = snap.identity.console.rp_name.clone();
        Ok((
            id.clone(),
            otp::base32(&secret),
            otp::otpauth_uri(&issuer, &account, &secret),
        ))
    }

    fn totp_secret(&self, id: &str, f: &FactorRecord) -> Option<Vec<u8>> {
        f.secret
            .as_deref()
            .and_then(|s| self.sealer.open(&seal_location(id), s).ok())
    }

    /// Accepts a TOTP code for factor `id` once: the step it matched is recorded with a
    /// compare-and-set, so the same code never works twice, on any node.
    async fn accept_totp(&self, user: &str, id: &str, code: &str) -> Result<bool, IdentityError> {
        let Some(rec) = self.store.record(kind::FACTOR, id).await? else {
            return Ok(false);
        };
        if rec.user != user {
            return Ok(false);
        }
        let Ok(f) = serde_json::from_str::<FactorRecord>(&rec.body) else {
            return Ok(false);
        };
        if f.kind != "totp" {
            return Ok(false);
        }
        let Some(secret) = self.totp_secret(id, &f) else {
            return Ok(false);
        };
        let now = self.now();
        let Some(step) = otp::verify(&secret, code.trim(), otp::step_at(unix_seconds(now)), f.last_step)
        else {
            return Ok(false);
        };
        let updated = FactorRecord {
            last_step: Some(step),
            last_used_us: Some(now.0),
            ..f
        };
        let body = serde_json::to_string(&updated).map_err(|_| IdentityError::Internal)?;
        Ok(self.store.swap_record(kind::FACTOR, id, &rec.body, &body).await?)
    }

    /// Confirms a factor being enrolled with its first code; returns the recovery codes when it is
    /// the user's first factor (shown once), else an empty list.
    pub async fn confirm_totp(
        &self,
        user: &str,
        id: &str,
        code: &str,
        name: Option<String>,
    ) -> Result<Vec<String>, IdentityError> {
        let Some((rec, f)) = self.read_record::<FactorRecord>(kind::FACTOR, id).await? else {
            return Err(IdentityError::NotFound);
        };
        if rec.user != user || f.kind != "totp" {
            return Err(IdentityError::NotFound);
        }
        if f.confirmed {
            return Err(IdentityError::Conflict("the factor is already confirmed"));
        }
        if !self.accept_totp(user, id, code).await? {
            return Err(IdentityError::Unauthenticated);
        }
        self.mark_confirmed(user, id, name).await
    }

    /// Marks an enrolled factor confirmed and issues recovery codes for a first factor.
    pub(crate) async fn mark_confirmed(
        &self,
        user: &str,
        id: &str,
        name: Option<String>,
    ) -> Result<Vec<String>, IdentityError> {
        let first = self.confirmed_factors(user).await?.is_empty();
        let Some((_, mut f)) = self.read_record::<FactorRecord>(kind::FACTOR, id).await? else {
            return Err(IdentityError::NotFound);
        };
        f.confirmed = true;
        f.name = name.or(f.name);
        self.write_record(kind::FACTOR, id, user, None, &f).await?;
        if first {
            self.issue_recovery_codes(user).await
        } else {
            Ok(Vec::new())
        }
    }

    /// Replaces the user's recovery codes; returns the new ones (shown once).
    pub(crate) async fn issue_recovery_codes(&self, user: &str) -> Result<Vec<String>, IdentityError> {
        let n = self.gens.load().identity.console.recovery_codes;
        let codes = otp::recovery_codes(n).map_err(|_| IdentityError::Internal)?;
        let record = RecoveryRecord {
            verifiers: codes
                .iter()
                .map(|c| self.pepper.verifier(&format!("recovery:{c}")))
                .collect(),
        };
        self.write_record(kind::RECOVERY, user, user, None, &record)
            .await?;
        Ok(codes)
    }

    /// Uses one recovery code: it is removed with a compare-and-set, so it works once.
    async fn accept_recovery_code(&self, user: &str, code: &str) -> Result<bool, IdentityError> {
        let Some(rec) = self.store.record(kind::RECOVERY, user).await? else {
            return Ok(false);
        };
        let Ok(mut r) = serde_json::from_str::<RecoveryRecord>(&rec.body) else {
            return Ok(false);
        };
        let presented = format!("recovery:{}", otp::normalize_recovery_code(code));
        let Some(i) = r.verifiers.iter().position(|v| self.pepper.verify(&presented, v)) else {
            return Ok(false);
        };
        r.verifiers.remove(i);
        let body = serde_json::to_string(&r).map_err(|_| IdentityError::Internal)?;
        Ok(self
            .store
            .swap_record(kind::RECOVERY, user, &rec.body, &body)
            .await?)
    }

    /// Whether `proof` is a valid second factor of `user` now; a TOTP code or recovery code is used
    /// up by succeeding.
    pub async fn verify_proof(&self, user: &str, proof: &Proof) -> Result<bool, IdentityError> {
        let snap = self.gens.load();
        match proof {
            Proof::Totp(code) => {
                if !snap.identity.console.allows(FactorKind::Totp) {
                    return Err(IdentityError::Invalid("totp is not an allowed factor"));
                }
                for (id, f) in self.confirmed_factors(user).await? {
                    if f.kind == "totp" && self.accept_totp(user, &id, code).await? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Proof::RecoveryCode(code) => self.accept_recovery_code(user, code).await,
            Proof::Webauthn(_) => Err(IdentityError::Invalid(
                "a WebAuthn assertion is accepted at sign-in only; give a TOTP or recovery code",
            )),
        }
    }

    /// Removes one of the user's factors; the last confirmed one cannot be removed.
    pub async fn delete_factor(&self, user: &str, id: &str) -> Result<(), IdentityError> {
        let factors = self.factor_records(user).await?;
        let Some((_, f)) = factors.iter().find(|(fid, _)| fid == id) else {
            return Err(IdentityError::NotFound);
        };
        if f.confirmed && factors.iter().filter(|(_, f)| f.confirmed).count() == 1 {
            return Err(IdentityError::Conflict(
                "the last second factor cannot be removed",
            ));
        }
        self.store.delete_record(kind::FACTOR, id).await?;
        Ok(())
    }

    /// Replaces the recovery codes after a fresh second factor.
    pub async fn regenerate_recovery_codes(
        &self,
        user: &str,
        proof: &Proof,
    ) -> Result<Vec<String>, IdentityError> {
        if self.confirmed_factors(user).await?.is_empty() {
            return Err(IdentityError::Conflict("no second factor is enrolled"));
        }
        if !self.verify_proof(user, proof).await? {
            return Err(IdentityError::Unauthenticated);
        }
        self.issue_recovery_codes(user).await
    }

    /// Removes every factor and recovery code of a user, ends its sessions and pending sign-ins:
    /// the next sign-in enrols a factor.
    pub async fn reset_factors(&self, user: &str) -> Result<(), IdentityError> {
        self.store
            .delete_records_of(user, &[kind::FACTOR, kind::RECOVERY, kind::CHALLENGE])
            .await?;
        self.end_user_sessions(user, None).await?;
        Ok(())
    }
}

pub(crate) fn new_factor_id() -> Result<String, IdentityError> {
    let id = random_id().map_err(|_| IdentityError::Internal)?;
    Ok(format!("f_{}", id.get(..16).unwrap_or(&id)))
}
