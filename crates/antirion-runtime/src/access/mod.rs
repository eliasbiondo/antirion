//! Data-plane access controls beyond a key's own checks: the failed-authentication throttle, access
//! rules and the external authorization hook, with the node state the throttle and the hook's answer
//! cache keep across generations.

mod authz;
mod rules;
mod throttle;

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

pub use self::authz::{AuthzCache, AuthzKey, ExternalAuthz, Verdict};
pub use self::rules::{AccessRule, RuleFacts, denies};
pub use self::throttle::{FailedAuth, ThrottleSettings};

/// The compiled `auth` settings of these controls.
#[derive(Clone, Debug, Default)]
pub struct AccessSettings {
    pub throttle: ThrottleSettings,
    pub rules: Vec<AccessRule>,
    pub external: Option<Arc<ExternalAuthz>>,
}

impl AccessSettings {
    /// Reads `auth.failed_auth_throttle`, `auth.access_rules` and `auth.external_authz` from a
    /// validated, secret-resolved document; `group:` patterns expand to their model groups.
    #[must_use]
    pub fn of(resolved: &Value, groups: &BTreeMap<String, Vec<String>>) -> AccessSettings {
        let auth = resolved.get("auth");
        AccessSettings {
            throttle: ThrottleSettings::of(auth.and_then(|a| a.get("failed_auth_throttle"))),
            rules: auth
                .and_then(|a| a.get("access_rules"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|r| AccessRule::of(r, groups))
                .collect(),
            external: auth
                .and_then(|a| a.get("external_authz"))
                .and_then(ExternalAuthz::of)
                .map(Arc::new),
        }
    }
}

/// Node state of these controls.
#[derive(Debug, Default)]
pub struct AccessState {
    pub failed_auth: FailedAuth,
    pub authz: AuthzCache,
}

impl AccessState {
    /// Forgets every throttle and cached authorization answer (test mode reset).
    pub fn reset(&self) {
        self.failed_auth.reset();
        self.authz.reset();
    }
}
