//! Operator identity for the console: local accounts with a mandatory second factor, password
//! policy, change and reset by email, recovery codes, invitations, preferences, and the outgoing mail
//! these need. The state is per user and lives in the store (`antirion_store::IdentityRecord`), never
//! in the configuration; passwords and recovery codes are kept only as hashes, TOTP secrets sealed.

mod checks;
mod factors;
pub mod mail;
mod password;
mod records;
mod settings;
mod signin;
mod webauthn;

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use antirion_core::UtcMicros;
use antirion_identity::rbac::Scope;
use antirion_identity::verifier::{digest_hex, random_id};
use antirion_store::{IdentityRecord, StoreError};
use parking_lot::Mutex;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

pub use checks::{InvitationCheck, ResetCheck, TokenState};
pub use factors::{Factor, Proof};
pub use mail::{Mailer, Outgoing};
pub use records::Invitation;
pub use settings::{ConsoleSettings, DeviceAuthorization, FactorKind, MailSettings, TrustedDevice};
pub use signin::LoginStep;

use crate::Runtime;
use crate::identity::IdentitySnapshot;

/// Record kinds of the identity store.
pub(crate) mod kind {
    pub(crate) const PASSWORD: &str = "password";
    pub(crate) const FACTOR: &str = "factor";
    pub(crate) const RECOVERY: &str = "recovery";
    pub(crate) const CHALLENGE: &str = "challenge";
    pub(crate) const LOCKOUT: &str = "lockout";
    pub(crate) const RESET: &str = "reset";
    pub(crate) const EMAIL: &str = "email";
    pub(crate) const PREFERENCES: &str = "preferences";
    pub(crate) const INVITATION: &str = "invitation";
    pub(crate) const INVITE_TOKEN: &str = "invite_token";
    pub(crate) const WEBAUTHN_HANDLE: &str = "webauthn_handle";
    pub(crate) const USED_TOKEN: &str = "used_token";
}

/// Why an operator-identity action was refused.
#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    /// Wrong credentials, factor or challenge: one answer for every cause.
    #[error("the credentials are not valid")]
    Unauthenticated,
    /// Local sign-in (or the feature asked for) is not enabled.
    #[error("not enabled")]
    NotEnabled,
    #[error("password sign-in is refused for this user")]
    LocalLoginRefused,
    #[error("too many attempts")]
    Throttled { retry_after: Duration },
    #[error("{}", .0.as_str())]
    Policy(antirion_identity::password::Rule),
    /// A token, factor or field the request named is not usable.
    #[error("{0}")]
    Invalid(&'static str),
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Conflict(&'static str),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("internal failure")]
    Internal,
}

/// Node-local state of the console's identity features.
#[derive(Debug, Default)]
pub struct ConsoleState {
    pub mailer: Mailer,
    pub push: crate::push::Pusher,
    /// Recent failed or rate-limited attempts per client IP or email, for the sign-in throttle.
    attempts: Mutex<HashMap<String, VecDeque<UtcMicros>>>,
    /// The login-page ticker and when it was computed.
    pub ticker: Mutex<Option<(UtcMicros, Value)>>,
}

impl ConsoleState {
    /// Records one attempt under `key` and says how long to wait when more than `max` fall within
    /// `window`.
    pub fn throttle(&self, key: &str, now: UtcMicros, max: u32, window: Duration) -> Option<Duration> {
        let mut map = self.attempts.lock();
        let list = map.entry(key.to_owned()).or_default();
        let start = now.0.saturating_sub(micros(window));
        while list.front().is_some_and(|t| t.0 <= start) {
            list.pop_front();
        }
        if list.len() >= usize::try_from(max).unwrap_or(usize::MAX) {
            let oldest = list.front().map_or(now.0, |t| t.0);
            let ends = oldest.saturating_add(micros(window));
            return Some(Duration::from_micros(
                u64::try_from(ends.saturating_sub(now.0))
                    .unwrap_or(0)
                    .max(1_000_000),
            ));
        }
        list.push_back(now);
        None
    }

    /// Whether `key` is over its limit, without counting an attempt.
    pub fn over(&self, key: &str, now: UtcMicros, max: u32, window: Duration) -> Option<Duration> {
        let map = self.attempts.lock();
        let list = map.get(key)?;
        let start = now.0.saturating_sub(micros(window));
        let recent: Vec<i64> = list.iter().map(|t| t.0).filter(|t| *t > start).collect();
        (recent.len() >= usize::try_from(max).unwrap_or(usize::MAX)).then(|| {
            let ends = recent
                .first()
                .copied()
                .unwrap_or(now.0)
                .saturating_add(micros(window));
            Duration::from_micros(
                u64::try_from(ends.saturating_sub(now.0))
                    .unwrap_or(0)
                    .max(1_000_000),
            )
        })
    }

    /// Forgets every throttle (test mode reset).
    pub fn reset(&self) {
        self.attempts.lock().clear();
        *self.ticker.lock() = None;
    }
}

/// `n` rounded to two significant figures.
fn two_figures(n: usize) -> usize {
    let mut scale = 1;
    while n / scale >= 100 {
        scale *= 10;
    }
    (n + scale / 2) / scale * scale
}

pub(crate) fn micros(d: Duration) -> i64 {
    i64::try_from(d.as_micros()).unwrap_or(i64::MAX)
}

/// A fresh single-use token and the digest it is stored under.
pub(crate) fn token() -> Result<(String, String), IdentityError> {
    let t = random_id().map_err(|_| IdentityError::Internal)?;
    let d = digest_hex(&t);
    Ok((t, d))
}

/// The organization a binding scope belongs to, through its team and project.
fn organization_of(id: &IdentitySnapshot, scope: &Scope) -> Option<String> {
    match scope {
        Scope::Gateway => None,
        Scope::Organization(o) => Some(o.clone()),
        Scope::Team(t) => id.teams.get(t).and_then(|t| t.parent.clone()),
        Scope::Project(p) => {
            let team = id.projects.get(p).and_then(|p| p.parent.clone())?;
            id.teams.get(&team).and_then(|t| t.parent.clone())
        }
    }
}

impl Runtime {
    /// A user's identity record of one kind, decoded.
    pub(crate) async fn read_record<T: DeserializeOwned>(
        &self,
        kind: &str,
        id: &str,
    ) -> Result<Option<(IdentityRecord, T)>, IdentityError> {
        let Some(rec) = self.store.record(kind, id).await? else {
            return Ok(None);
        };
        let body = serde_json::from_str(&rec.body).map_err(|_| IdentityError::Internal)?;
        Ok(Some((rec, body)))
    }

    pub(crate) async fn write_record<T: Serialize>(
        &self,
        kind: &str,
        id: &str,
        user: &str,
        expires: Option<UtcMicros>,
        body: &T,
    ) -> Result<(), IdentityError> {
        let body = serde_json::to_string(body).map_err(|_| IdentityError::Internal)?;
        self.store
            .put_record(IdentityRecord {
                kind: kind.to_owned(),
                id: id.to_owned(),
                user: user.to_owned(),
                expires_us: expires.map(|t| t.0),
                body,
            })
            .await?;
        Ok(())
    }

    /// Takes a single-use record, refusing it once expired.
    pub(crate) async fn take_live(
        &self,
        kind: &str,
        id: &str,
    ) -> Result<Option<IdentityRecord>, IdentityError> {
        let rec = self.store.take_record(kind, id).await?;
        let now = self.now();
        Ok(rec.filter(|r| r.expires_us.is_none_or(|e| now.0 < e)))
    }

    /// Reads a record unless it expired.
    pub(crate) async fn live_record(
        &self,
        kind: &str,
        id: &str,
    ) -> Result<Option<IdentityRecord>, IdentityError> {
        let rec = self.store.record(kind, id).await?;
        let now = self.now();
        Ok(rec.filter(|r| r.expires_us.is_none_or(|e| now.0 < e)))
    }

    /// The enabled human user with this email (case-insensitive).
    #[must_use]
    pub fn user_by_email(&self, email: &str) -> Option<String> {
        let snap = self.gens.load();
        let wanted = email.trim().to_lowercase();
        let mut names: Vec<&String> = snap
            .identity
            .users
            .iter()
            .filter(|(_, u)| u.human && u.email.as_deref().is_some_and(|e| e.to_lowercase() == wanted))
            .map(|(n, _)| n)
            .collect();
        names.sort();
        names.first().map(|n| (*n).clone())
    }

    /// Why `user` may not sign in with a password now, or `None` when it may: local accounts are
    /// off, the user is gone, disabled or a service account, its email domain must use single
    /// sign-on, or it is bound in an organization that refuses password sign-in.
    #[must_use]
    pub fn local_refusal(&self, user: &str) -> Option<IdentityError> {
        let snap = self.gens.load();
        let id = &snap.identity;
        if !id.console.local {
            return Some(IdentityError::NotEnabled);
        }
        let Some(u) = id.users.get(user).filter(|u| u.enabled && u.human) else {
            return Some(IdentityError::Unauthenticated);
        };
        let domain = u
            .email
            .as_deref()
            .and_then(|e| e.rsplit_once('@'))
            .map(|(_, d)| d.to_ascii_lowercase());
        if domain.is_some_and(|d| id.console.sso_required_domains.contains(&d)) {
            return Some(IdentityError::LocalLoginRefused);
        }
        let refused = u.bindings.iter().any(|b| {
            organization_of(id, &b.scope)
                .and_then(|o| id.organizations.get(&o))
                .is_some_and(|o| !o.local_login)
        });
        refused.then_some(IdentityError::LocalLoginRefused)
    }

    /// Queues a mail with the current settings; false when mail is off or the queue is full.
    pub fn send_mail(&self, to: &str, subject: &str, body: String) -> bool {
        let snap = self.gens.load();
        let Some(settings) = snap.identity.console.mail.clone() else {
            return false;
        };
        let message = antirion_mail::Message {
            from: settings.from.clone(),
            reply_to: settings.reply_to.clone(),
            to: to.to_owned(),
            subject: subject.to_owned(),
            body,
            date: self.now().to_rfc2822(),
        };
        self.state.console.mailer.enqueue(
            Outgoing {
                settings,
                message,
                policy: std::sync::Arc::clone(&self.gens.load().security.egress),
            },
            &self.metrics,
            &self.tasks,
            &self.shutdown,
        )
    }

    /// The login page's coarse, gateway-wide figures over the last 24 hours: requests and active keys
    /// to two significant figures, the error rate to a whole percent, p95 to 100 ms; recomputed at
    /// most once a minute.
    pub async fn login_ticker(&self) -> Value {
        let now = self.now();
        if let Some((at, v)) = self.state.console.ticker.lock().as_ref()
            && now.0.saturating_sub(at.0) < 60_000_000
        {
            return v.clone();
        }
        let from = now.0.saturating_sub(86_400_000_000);
        let q = crate::reqlog::analytics::AggQuery {
            from_us: from,
            to_us: now.0.saturating_add(1),
            starts: vec![from],
            group_by: vec![crate::reqlog::analytics::Dim::StatusClass],
            rollup: Some(crate::reqlog::analytics::RollupFilter::default()),
            ..crate::reqlog::analytics::AggQuery::default()
        };
        let groups = self.aggregate(&q).await.unwrap_or_default();
        let mut latency = antirion_obs::sketch::Sketch::new();
        let (mut total, mut errors) = (0i64, 0i64);
        for g in &groups {
            total += g.agg.counts[0];
            if g.key.first().cloned().flatten().as_deref() == Some("5xx") {
                errors += g.agg.counts[0];
            }
            latency.merge(&g.agg.latency);
        }
        let p95 = latency.quantile(0.95).unwrap_or(0.0);
        let total = usize::try_from(total).unwrap_or(0);
        let errors = usize::try_from(errors).unwrap_or(0);
        let keys = self
            .gens
            .load()
            .identity
            .keys_by_kid
            .values()
            .filter(|k| k.key.enabled && k.valid_until.is_none())
            .count();
        #[allow(clippy::cast_precision_loss)]
        let rate = if total == 0 {
            0.0
        } else {
            errors as f64 / total as f64
        };
        let v = serde_json::json!({
            "requests_24h": two_figures(total),
            "error_rate": (rate * 100.0).round() / 100.0,
            "p95_ms": (p95 / 100.0).round() * 100.0,
            "active_keys": two_figures(keys),
            "as_of": now.to_rfc3339_seconds(),
        });
        *self.state.console.ticker.lock() = Some((now, v.clone()));
        v
    }

    /// Forgets every identity record of a deleted user.
    pub async fn forget_identity(&self, user: &str) -> Result<(), StoreError> {
        self.store.delete_records_of(user, &[]).await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_significant_figures() {
        assert_eq!(two_figures(12_345), 12_000);
        assert_eq!(two_figures(12_500), 13_000);
        assert_eq!(two_figures(99), 99);
        assert_eq!(two_figures(0), 0);
        assert_eq!(two_figures(995), 1_000);
    }

    #[test]
    fn throttle_counts_within_the_window() {
        let s = ConsoleState::default();
        let w = Duration::from_secs(60);
        let t0 = UtcMicros(1_000_000_000);
        for i in 0..3 {
            assert!(s.throttle("ip", UtcMicros(t0.0 + i), 3, w).is_none());
        }
        let wait = s.throttle("ip", UtcMicros(t0.0 + 10), 3, w).unwrap();
        assert!(wait <= w);
        assert!(s.over("ip", UtcMicros(t0.0 + 10), 3, w).is_some());
        assert!(s.throttle("ip", UtcMicros(t0.0 + 61_000_000), 3, w).is_none());
        s.reset();
        assert!(s.over("ip", t0, 3, w).is_none());
    }
}
