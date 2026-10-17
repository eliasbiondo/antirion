//! The sign-in facts the console's public pages show before anyone is authenticated: whether a
//! device may be trusted and for how long, the password policy and the reset link's lifetime, and
//! whether the gateway is still waiting for its first account.

use antirion_runtime::Runtime;
use antirion_runtime::identity::IdentitySnapshot;
use serde_json::{Map, Value, json};

const DAY_SECS: u64 = 86_400;

/// The members `GET /auth/login_options` adds to its answer.
pub(crate) fn facts(rt: &Runtime, id: &IdentitySnapshot) -> Map<String, Value> {
    let c = &id.console;
    let trusted = c.trusted.map_or_else(
        || json!({"enabled": false, "max_age_days": null}),
        |t| json!({"enabled": true, "max_age_days": (t.max_age.as_secs() / DAY_SECS).max(1)}),
    );
    let password = if c.local {
        json!({
            "min_length": c.policy.min_length,
            "max_length": c.policy.max_length,
            "reset_ttl_seconds": rt.resets_enabled().then_some(c.reset_ttl.as_secs()),
        })
    } else {
        Value::Null
    };
    let mut out = Map::new();
    out.insert("trusted_device".into(), trusted);
    out.insert("password".into(), password);
    out.insert("setup_required".into(), Value::Bool(setup_required(id)));
    out.insert(
        "device_authorization".into(),
        Value::Bool(c.device.is_some() && c.personal_tokens),
    );
    out
}

/// No enabled user exists, so no admin token or session can authenticate: only the bootstrap
/// credential opens the Admin API.
fn setup_required(id: &IdentitySnapshot) -> bool {
    !id.users.values().any(|u| u.enabled)
}
