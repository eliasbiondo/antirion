//! Local sign-in: the password step (with its account lockout and the per-client throttle), the
//! single-use challenge it hands out, and the second-factor step that starts the session.

use std::time::Duration;

use antirion_core::UtcMicros;
use antirion_identity::password;
use serde::{Deserialize, Serialize};

use super::{IdentityError, kind, micros, token};
use crate::Runtime;
use crate::admin_auth::{Client, Session, SessionFacts};

/// How long a sign-in challenge lives.
const CHALLENGE_TTL: Duration = Duration::from_secs(300);
/// Wrong second factors one challenge tolerates before it is void.
const CHALLENGE_ATTEMPTS: u32 = 5;

/// What the password step answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginStep {
    /// `mfa`, or `enroll_mfa` when the user has no second factor yet.
    pub next: &'static str,
    pub challenge: String,
    /// The WebAuthn request options for the user's credentials, when it has any.
    pub webauthn: Option<serde_json::Value>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct PasswordRecord {
    pub(crate) hash: String,
    /// Earlier hashes, newest first.
    #[serde(default)]
    pub(crate) history: Vec<String>,
    pub(crate) changed_us: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Lockout {
    #[serde(default)]
    failures: Vec<i64>,
    #[serde(default)]
    until: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Challenge {
    pub(crate) user: String,
    /// `mfa` or `enroll_mfa`.
    pub(crate) next: String,
    #[serde(default)]
    pub(crate) organization: Option<String>,
    #[serde(default)]
    pub(crate) attempts: u32,
    /// The WebAuthn assertion this sign-in started, when the user has a credential.
    #[serde(default)]
    pub(crate) webauthn_state: Option<String>,
}

impl Runtime {
    /// The throttle every public identity endpoint applies per client IP.
    pub fn throttle_client(&self, ip: Option<&str>) -> Result<(), IdentityError> {
        self.throttle_ip("ip", ip)
    }

    /// The same throttle for admin tokens: bearer credentials and token sign-ins.
    pub fn throttle_bearer(&self, ip: Option<&str>) -> Result<(), IdentityError> {
        self.throttle_ip("bearer", ip)
    }

    fn throttle_ip(&self, kind: &str, ip: Option<&str>) -> Result<(), IdentityError> {
        let snap = self.gens.load();
        let c = &snap.identity.console;
        match ip.and_then(|ip| {
            self.state.console.over(
                &format!("{kind}:{ip}"),
                self.now(),
                c.throttle_failures,
                c.throttle_window,
            )
        }) {
            Some(retry_after) => Err(IdentityError::Throttled { retry_after }),
            None => Ok(()),
        }
    }

    /// Counts a failed attempt from `ip` against the throttle.
    pub fn count_failure(&self, ip: Option<&str>) {
        self.count_ip("ip", ip);
    }

    /// Counts a failed admin token from `ip`.
    pub fn count_bearer_failure(&self, ip: Option<&str>) {
        self.count_ip("bearer", ip);
    }

    fn count_ip(&self, kind: &str, ip: Option<&str>) {
        let snap = self.gens.load();
        let c = &snap.identity.console;
        if let Some(ip) = ip {
            let _ = self.state.console.throttle(
                &format!("{kind}:{ip}"),
                self.now(),
                c.throttle_failures,
                c.throttle_window,
            );
        }
    }

    async fn locked_until(&self, user: &str) -> Result<Option<Duration>, IdentityError> {
        let snap = self.gens.load();
        let c = &snap.identity.console;
        let now = self.now();
        let Some((_, mut l)) = self.read_record::<Lockout>(kind::LOCKOUT, user).await? else {
            return Ok(None);
        };
        let start = now.0.saturating_sub(micros(c.lockout_window));
        l.failures.retain(|t| *t > start);
        if let Some(until) = l.until.filter(|u| *u > now.0) {
            return Ok(Some(Duration::from_micros(
                u64::try_from(until - now.0).unwrap_or(0).max(1_000_000),
            )));
        }
        if l.failures.len() >= usize::try_from(c.lockout_failures).unwrap_or(usize::MAX) {
            let until = now.0.saturating_add(micros(c.lockout_duration));
            l.until = Some(until);
            self.write_record(kind::LOCKOUT, user, user, None, &l).await?;
            return Ok(Some(c.lockout_duration));
        }
        Ok(None)
    }

    async fn record_failure(&self, user: &str) -> Result<(), IdentityError> {
        let snap = self.gens.load();
        let c = &snap.identity.console;
        let now = self.now();
        let mut l = self
            .read_record::<Lockout>(kind::LOCKOUT, user)
            .await?
            .map(|(_, l)| l)
            .unwrap_or_default();
        let start = now.0.saturating_sub(micros(c.lockout_window));
        l.failures.retain(|t| *t > start);
        l.failures.push(now.0);
        if l.failures.len() >= usize::try_from(c.lockout_failures).unwrap_or(usize::MAX) {
            l.until = Some(now.0.saturating_add(micros(c.lockout_duration)));
        }
        self.write_record(kind::LOCKOUT, user, user, None, &l).await
    }

    /// The user's stored password, when it has one.
    pub(crate) async fn password_of(&self, user: &str) -> Result<Option<PasswordRecord>, IdentityError> {
        Ok(self
            .read_record::<PasswordRecord>(kind::PASSWORD, user)
            .await?
            .map(|(_, p)| p))
    }

    /// Hands out a sign-in challenge for `user`.
    pub(crate) async fn new_challenge(
        &self,
        user: &str,
        organization: Option<String>,
        origin: Option<&str>,
    ) -> Result<LoginStep, IdentityError> {
        let next = if self.confirmed_factors(user).await?.is_empty() {
            "enroll_mfa"
        } else {
            "mfa"
        };
        let assertion = match origin {
            Some(o) if next == "mfa" => self.start_assertion(user, o).await?,
            _ => None,
        };
        let (webauthn_state, webauthn) = assertion.map_or((None, None), |(s, o)| (Some(s), Some(o)));
        let (challenge, digest) = token()?;
        let expires = self.now().saturating_add(CHALLENGE_TTL);
        let body = Challenge {
            user: user.to_owned(),
            next: next.to_owned(),
            organization,
            attempts: 0,
            webauthn_state,
        };
        self.write_record(kind::CHALLENGE, &digest, user, Some(expires), &body)
            .await?;
        Ok(LoginStep {
            next,
            challenge,
            webauthn,
        })
    }

    /// The live challenge with this value.
    pub(crate) async fn challenge(&self, challenge: &str) -> Result<(String, Challenge), IdentityError> {
        let digest = antirion_identity::verifier::digest_hex(challenge);
        let rec = self
            .live_record(kind::CHALLENGE, &digest)
            .await?
            .ok_or(IdentityError::Unauthenticated)?;
        let body: Challenge = serde_json::from_str(&rec.body).map_err(|_| IdentityError::Internal)?;
        if self.local_refusal(&body.user).is_some() {
            return Err(IdentityError::Unauthenticated);
        }
        Ok((digest, body))
    }

    /// Counts a wrong second factor against a challenge, voiding it after a few.
    async fn challenge_failed(&self, digest: &str, c: &Challenge) -> Result<(), IdentityError> {
        let Some(rec) = self.store.record(kind::CHALLENGE, digest).await? else {
            return Ok(());
        };
        let mut next = c.clone();
        next.attempts += 1;
        if next.attempts >= CHALLENGE_ATTEMPTS {
            self.store.delete_record(kind::CHALLENGE, digest).await?;
        } else {
            let body = serde_json::to_string(&next).map_err(|_| IdentityError::Internal)?;
            self.store
                .swap_record(kind::CHALLENGE, digest, &rec.body, &body)
                .await?;
        }
        Ok(())
    }

    /// The password step: one answer for every wrong credential, after the same work.
    pub async fn local_login(
        &self,
        email: &str,
        password_text: &str,
        organization: Option<String>,
        ip: Option<&str>,
        origin: &str,
    ) -> Result<LoginStep, IdentityError> {
        if !self.gens.load().identity.console.local {
            return Err(IdentityError::NotEnabled);
        }
        self.throttle_client(ip)?;
        let user = self.user_by_email(email);
        if let Some(u) = &user
            && let Some(retry_after) = self.locked_until(u).await?
        {
            return Err(IdentityError::Throttled { retry_after });
        }
        let stored = match &user {
            Some(u) => self.password_of(u).await?,
            None => None,
        };
        let presented = password_text.to_owned();
        let hash = stored.as_ref().map(|p| p.hash.clone());
        let matches = tokio::task::spawn_blocking(move || match hash {
            Some(h) => password::verify(&presented, &h),
            None => {
                password::verify_nothing(&presented);
                false
            }
        })
        .await
        .map_err(|_| IdentityError::Internal)?;
        let (Some(user), Some(stored), true) = (user, stored, matches) else {
            self.count_failure(ip);
            if let Some(u) = self.user_by_email(email) {
                self.record_failure(&u).await?;
            }
            return Err(IdentityError::Unauthenticated);
        };
        match self.local_refusal(&user) {
            None => {}
            Some(IdentityError::LocalLoginRefused) => return Err(IdentityError::LocalLoginRefused),
            Some(_) => {
                self.count_failure(ip);
                return Err(IdentityError::Unauthenticated);
            }
        }
        self.store.delete_record(kind::LOCKOUT, &user).await?;
        if password::needs_rehash(&stored.hash) {
            let presented = password_text.to_owned();
            if let Ok(Ok(h)) = tokio::task::spawn_blocking(move || password::hash(&presented)).await {
                let updated = PasswordRecord { hash: h, ..stored };
                self.write_record(kind::PASSWORD, &user, &user, None, &updated)
                    .await?;
            }
        }
        self.new_challenge(&user, organization, Some(origin)).await
    }

    /// The second-factor step: a TOTP code, a WebAuthn assertion or a recovery code completes the
    /// challenge and starts the session. `trust` is honoured when trusted devices are enabled.
    pub async fn local_mfa(
        &self,
        challenge: &str,
        proof: &super::Proof,
        trust: bool,
        client: Client,
        origin: &str,
    ) -> Result<(String, Session), IdentityError> {
        if !self.gens.load().identity.console.local {
            return Err(IdentityError::NotEnabled);
        }
        self.throttle_client(client.ip.as_deref())?;
        let (digest, c) = self.challenge(challenge).await?;
        let verified = match (proof, &c.webauthn_state) {
            (_, _) if c.next != "mfa" => false,
            (super::Proof::Webauthn(assertion), Some(state)) => {
                self.verify_assertion(&c.user, state, assertion, origin).await?
            }
            (super::Proof::Webauthn(_), None) => false,
            _ => self.verify_proof(&c.user, proof).await?,
        };
        if !verified {
            self.count_failure(client.ip.as_deref());
            self.challenge_failed(&digest, &c).await?;
            return Err(IdentityError::Unauthenticated);
        }
        if self.take_live(kind::CHALLENGE, &digest).await?.is_none() {
            return Err(IdentityError::Unauthenticated);
        }
        self.local_session(&c, trust, client).await
    }

    /// Starts the session a completed local sign-in earned.
    pub(crate) async fn local_session(
        &self,
        c: &Challenge,
        trust: bool,
        client: Client,
    ) -> Result<(String, Session), IdentityError> {
        let trusted = trust && self.gens.load().identity.console.trusted.is_some();
        let facts = SessionFacts::new(
            "local",
            true,
            trusted,
            Client {
                organization: c.organization.clone(),
                ..client
            },
        );
        self.start_user_session(&c.user, facts)
            .await
            .map_err(|e| match e {
                crate::admin_auth::SessionError::Store(s) => IdentityError::Store(s),
                _ => IdentityError::Unauthenticated,
            })
    }

    /// Voids every pending sign-in challenge of a user.
    pub(crate) async fn void_challenges(&self, user: &str) -> Result<(), IdentityError> {
        self.store.delete_records_of(user, &[kind::CHALLENGE]).await?;
        Ok(())
    }

    /// When `user` last set its password.
    pub async fn password_changed_at(&self, user: &str) -> Result<Option<UtcMicros>, IdentityError> {
        Ok(self.password_of(user).await?.map(|p| UtcMicros(p.changed_us)))
    }
}

impl Runtime {
    /// The user an enrolment challenge (`next: enroll_mfa`) lets enrol a factor.
    pub async fn enrolment_user(&self, challenge: &str) -> Result<String, IdentityError> {
        let (_, c) = self.challenge(challenge).await?;
        if c.next != "enroll_mfa" {
            return Err(IdentityError::Unauthenticated);
        }
        Ok(c.user)
    }

    /// Ends an enrolment sign-in: the challenge is used up and the session starts.
    pub async fn finish_enrolment(
        &self,
        challenge: &str,
        client: Client,
    ) -> Result<(String, Session), IdentityError> {
        let (digest, c) = self.challenge(challenge).await?;
        if c.next != "enroll_mfa" || self.take_live(kind::CHALLENGE, &digest).await?.is_none() {
            return Err(IdentityError::Unauthenticated);
        }
        self.local_session(&c, false, client).await
    }
}
