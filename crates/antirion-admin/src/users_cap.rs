//! The optional cap on active users (`auth.admin.max_active_users`): enabled and invited `human`
//! users count; service accounts do not. Creating, enabling, inviting or provisioning one more past
//! the cap is refused.

use antirion_runtime::Runtime;
use http::StatusCode;
use serde_json::{Value, json};

use crate::error::AdminError;
use crate::scim::ScimError;

/// The cap, when one is set.
pub(crate) fn cap(rt: &Runtime) -> Option<u64> {
    rt.gens
        .load()
        .config
        .doc
        .pointer("/auth/admin/max_active_users")
        .and_then(Value::as_u64)
}

fn human(entry: &Value) -> bool {
    entry.get("kind").and_then(Value::as_str) != Some("service")
}

fn enabled(entry: &Value) -> bool {
    entry.get("enabled") != Some(&Value::Bool(false))
}

/// Enabled human users, and disabled ones with an invitation pending.
pub(crate) async fn active(rt: &Runtime) -> u64 {
    let snap = rt.gens.load();
    let users = snap.config.doc.get("users").and_then(Value::as_object);
    let enabled_humans = users
        .into_iter()
        .flatten()
        .filter(|(_, e)| human(e) && enabled(e))
        .count();
    let invited = rt
        .invitations()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|i| {
            users
                .and_then(|u| u.get(&i.user))
                .is_some_and(|e| human(e) && !enabled(e))
        })
        .count();
    u64::try_from(enabled_humans + invited).unwrap_or(u64::MAX)
}

/// Whether one more active human user fits under the cap.
pub(crate) async fn room_for_one(rt: &Runtime) -> bool {
    match cap(rt) {
        None => true,
        Some(cap) => active(rt).await < cap,
    }
}

pub(crate) fn limit_reached() -> AdminError {
    AdminError::new(
        StatusCode::CONFLICT,
        "limit_reached",
        "The gateway has as many active users as auth.admin.max_active_users allows.",
    )
}

/// Refuses a change that makes one more user active past the cap.
pub(crate) async fn check(rt: &Runtime) -> Result<(), AdminError> {
    if room_for_one(rt).await {
        Ok(())
    } else {
        Err(limit_reached())
    }
}

/// As [`check`], for SCIM.
pub(crate) async fn check_scim(rt: &Runtime, becomes_active: bool) -> Result<(), ScimError> {
    if !becomes_active || room_for_one(rt).await {
        Ok(())
    } else {
        Err(ScimError::new(
            StatusCode::CONFLICT,
            Some("tooMany"),
            "The gateway has as many active users as auth.admin.max_active_users allows.",
        ))
    }
}

/// `users: {active, cap}` for `GET /status`.
pub(crate) async fn status(rt: &Runtime) -> Value {
    json!({"active": active(rt).await, "cap": cap(rt)})
}

/// Whether a user write makes a human user active that was not: a new enabled human, or an
/// existing human enabled again.
pub(crate) fn activates(old: Option<&Value>, new: &Value) -> bool {
    human(new) && enabled(new) && old.is_none_or(|o| !enabled(o) || !human(o))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_newly_active_humans_count() {
        assert!(activates(None, &json!({"kind": "human"})));
        assert!(activates(None, &json!({})));
        assert!(!activates(None, &json!({"kind": "service"})));
        assert!(!activates(None, &json!({"enabled": false})));
        assert!(activates(
            Some(&json!({"enabled": false})),
            &json!({"enabled": true})
        ));
        assert!(!activates(Some(&json!({})), &json!({"display_name": "x"})));
    }
}
