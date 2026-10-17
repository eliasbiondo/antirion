//! The durable record of use: each key's last authentication, written to the store in batches every
//! few seconds (a key at most once a minute of use), and each user's last sign-in, written when the
//! session starts. Reads merge the store with what this node has not written yet.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use antirion_core::{ResourceUid, UtcMicros};
use antirion_store::IdentityRecord;
use serde::{Deserialize, Serialize};

use crate::Runtime;
use crate::admin_auth::SessionFacts;

const FLUSH_EVERY: Duration = Duration::from_secs(5);
/// The store kind of key marks.
const KEY: &str = "key";
/// The identity record that holds a user's last sign-in.
const LAST_LOGIN: &str = "last_login";
/// Sign-in methods whose second factor is the identity provider's.
const IDP_METHODS: [&str; 3] = ["oidc", "oauth", "saml"];

/// A user's last sign-in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastLogin {
    pub at: String,
    /// `token`, `oidc`, `oauth`, `saml` or `local`.
    pub method: String,
    /// Whether the sign-in proved a second factor.
    #[serde(default)]
    pub mfa: bool,
    /// The identity provider of a single sign-on, by its name in the configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

impl LastLogin {
    /// Whether an identity provider asserted the second factor.
    #[must_use]
    pub fn idp_mfa(&self) -> bool {
        self.mfa && IDP_METHODS.contains(&self.method.as_str())
    }
}

impl Runtime {
    /// Writes the key uses the store has not heard about.
    pub async fn flush_use_marks(&self) {
        let due = self.state.key_usage.unwritten();
        if due.is_empty() {
            return;
        }
        let rows = due.iter().map(|(k, at)| (k.to_hex(), at.0)).collect();
        match self.store.put_use_marks(KEY, rows).await {
            Ok(()) => self.state.key_usage.written(&due),
            Err(e) => tracing::warn!(error = %e, "key use times could not be written"),
        }
    }

    /// Every key's last use, by incarnation: the store's, or this node's when later.
    pub async fn keys_last_used(&self) -> HashMap<ResourceUid, UtcMicros> {
        let mut out: HashMap<ResourceUid, UtcMicros> = self
            .store
            .use_marks(KEY)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(id, at)| Some((ResourceUid::from_hex(&id)?, UtcMicros(at))))
            .collect();
        let snap = self.gens.load();
        for uid in snap.config.live_uids() {
            if let Some(mine) = self.state.key_usage.last_used(uid) {
                let entry = out.entry(uid).or_insert(mine);
                *entry = (*entry).max(mine);
            }
        }
        out
    }

    /// Records a user's sign-in.
    pub async fn record_login(&self, user: &str, facts: &SessionFacts) {
        let login = LastLogin {
            at: self.now().to_rfc3339_seconds(),
            method: facts.method.clone(),
            mfa: facts.mfa,
            provider: facts.provider.clone(),
        };
        let record = IdentityRecord {
            kind: LAST_LOGIN.to_owned(),
            id: user.to_owned(),
            user: user.to_owned(),
            expires_us: None,
            body: serde_json::to_string(&login).unwrap_or_default(),
        };
        if let Err(e) = self.store.put_record(record).await {
            tracing::warn!(error = %e, "a sign-in time could not be written");
        }
    }

    /// Every user's last sign-in.
    pub async fn last_logins(&self) -> HashMap<String, LastLogin> {
        self.store
            .records(LAST_LOGIN, None)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(|r| Some((r.user, serde_json::from_str(&r.body).ok()?)))
            .collect()
    }

    /// The kinds of every user's confirmed second factors (`totp`, `webauthn`).
    pub async fn factor_kinds(&self) -> HashMap<String, BTreeSet<String>> {
        let mut out: HashMap<String, BTreeSet<String>> = HashMap::new();
        for (user, kind) in self.confirmed_factor_kinds().await {
            out.entry(user).or_default().insert(kind);
        }
        out
    }
}

/// Writes key uses every few seconds, and once more when the node stops.
pub(crate) async fn run(rt: Arc<Runtime>) {
    loop {
        let stop = tokio::select! {
            () = rt.shutdown.cancelled() => true,
            () = tokio::time::sleep(FLUSH_EVERY) => false,
        };
        rt.flush_use_marks().await;
        if stop {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_provider_asserts_the_second_factor() {
        let login = |method: &str, mfa| LastLogin {
            at: String::new(),
            method: method.into(),
            mfa,
            provider: None,
        };
        assert!(login("oidc", true).idp_mfa());
        assert!(!login("oidc", false).idp_mfa());
        assert!(!login("local", true).idp_mfa());
    }
}
