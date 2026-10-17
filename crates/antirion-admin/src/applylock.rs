//! `admin.config.lock_applied`: while it is on, an API write may not change or delete a resource or
//! setting that an apply manages; only an apply from its source (or turning the lock off) changes it.

use antirion_runtime::ConfigState;
use antirion_runtime::changes::{self, Op, Target};
use http::StatusCode;
use serde_json::Value;

use crate::call::Call;
use crate::error::AdminError;

/// Whether the startup document turns the lock on (a static setting).
fn locked(statics: &Value) -> bool {
    statics
        .pointer("/admin/config/lock_applied")
        .and_then(Value::as_bool)
        == Some(true)
}

/// The apply source that manages the first resource or setting `candidate` would change or delete.
fn first_locked(state: &ConfigState, candidate: &Value) -> Option<(String, String)> {
    changes::compute(&state.doc, candidate)
        .into_iter()
        .filter(|(_, op, _, _)| *op != Op::Create)
        .find_map(|(target, _, _, _)| {
            let meta = match &target {
                Target::Resource(c, name) => state.meta.resources.get(*c).and_then(|m| m.get(name)),
                Target::Setting(s) => state.meta.settings.get(*s),
            }?;
            let source = meta.managed_by.strip_prefix("apply:")?;
            Some((target.path(), source.to_owned()))
        })
}

/// Refuses an API write that would change an apply-managed resource while the lock is on.
pub(crate) fn check(call: &Call, state: &ConfigState, candidate: &Value) -> Result<(), AdminError> {
    if !locked(&call.rt.statics.doc) {
        return Ok(());
    }
    match first_locked(state, candidate) {
        None => Ok(()),
        Some((path, source)) => Err(AdminError::new(
            StatusCode::CONFLICT,
            "managed_by_apply",
            format!(
                "{path} is managed by apply source {source:?} and admin.config.lock_applied is on: change it \
                 through an apply from that source."
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use antirion_runtime::state::{Metadata, ResourceMeta};
    use serde_json::json;

    use super::*;

    fn meta(managed_by: &str) -> ResourceMeta {
        ResourceMeta {
            etag: String::new(),
            generation: 1,
            created_at: String::new(),
            updated_at: String::new(),
            managed_by: managed_by.into(),
            uid: antirion_core::ResourceUid::random(),
            credential: None,
            created_by: None,
            disabled_reason: None,
            lifecycle_at: None,
            credentials_rotated_at: std::collections::BTreeMap::new(),
            rotated_at: None,
        }
    }

    #[test]
    fn only_changes_to_applied_resources_are_named() {
        let mut m = Metadata::default();
        let routes = m.resources.entry("routes".into()).or_default();
        routes.insert("filed".into(), meta("apply:git"));
        routes.insert("manual".into(), meta("api"));
        let state = ConfigState {
            generation: 1,
            doc: json!({"version": 1, "routes": {"filed": {"tree": "a"}, "manual": {"tree": "a"}}}),
            meta: m,
        };
        let manual = json!({"version": 1, "routes": {"filed": {"tree": "a"}, "manual": {"tree": "b"}, "new": {"tree": "a"}}});
        assert_eq!(first_locked(&state, &manual), None);
        let filed = json!({"version": 1, "routes": {"filed": {"tree": "b"}, "manual": {"tree": "a"}}});
        assert_eq!(
            first_locked(&state, &filed),
            Some(("routes.filed".into(), "git".into()))
        );
        let deleted = json!({"version": 1, "routes": {"manual": {"tree": "a"}}});
        assert_eq!(first_locked(&state, &deleted).map(|(_, s)| s), Some("git".into()));
        assert!(locked(&json!({"admin": {"config": {"lock_applied": true}}})));
        assert!(!locked(&json!({})));
    }
}
