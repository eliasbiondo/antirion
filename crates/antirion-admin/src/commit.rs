//! Running a mutation for a handler: the audit context, and the mapping of commit failures to the
//! Admin API's errors.

use antirion_core::audit::Outcome;
use antirion_runtime::changes::Target;
use antirion_runtime::state::KeyCredential;
use antirion_runtime::{CommitError, Committed, ConfigState, MetaEdit, Mutation};
use http::StatusCode;
use serde_json::{Map, Value};

use crate::call::Call;
use crate::error::AdminError;

/// What a resource or settings write proposes.
pub(crate) struct Proposal {
    pub(crate) candidate: Value,
    pub(crate) written: Vec<Target>,
    pub(crate) credentials: Vec<(String, KeyCredential)>,
    pub(crate) meta_edits: Vec<MetaEdit>,
}

/// Commits `build`'s proposal as an API write, audited as `action` on `resource`.
pub(crate) async fn api_write(
    call: &Call,
    action: &str,
    resource: &str,
    success: StatusCode,
    build: impl FnOnce(&ConfigState) -> Result<Proposal, AdminError>,
) -> Result<Committed, AdminError> {
    let audit = call.audit_context(action, resource, success);
    let privileged = (action.to_owned(), resource.to_owned());
    let result = call
        .rt
        .mutate(|state| {
            let p = build(state)?;
            if !call.approved && crate::change_control::governed(&state.doc, &p.candidate) {
                return Err(crate::change_control::required());
            }
            crate::cache_share::check(&call.principal, &state.doc, &p.candidate)?;
            crate::applylock::check(call, state, &p.candidate)?;
            Ok(Mutation {
                candidate: p.candidate,
                writer: "api".to_owned(),
                written: p.written,
                credentials: p.credentials,
                meta_edits: p.meta_edits,
                dry_run: false,
                source: "api".to_owned(),
                audit: Some(audit),
            })
        })
        .await;
    let committed = finish(call, action, resource, result).await?;
    if committed.generation != call.snap.generation {
        call.rt
            .audit_privileged(&privileged.0, &privileged.1, Some(&call.principal.id));
    }
    Ok(committed)
}

/// Maps a commit result; a rejected or invalid change is audited as failed.
pub(crate) async fn finish(
    call: &Call,
    action: &str,
    resource: &str,
    result: Result<Committed, CommitError<AdminError>>,
) -> Result<Committed, AdminError> {
    let err = match result {
        Ok(c) => return Ok(c),
        Err(CommitError::Rejected(e)) => e,
        Err(CommitError::Invalid(details)) => AdminError::validation(details),
        Err(CommitError::Store(e)) => {
            tracing::error!(error = %e, "a configuration change could not be stored");
            AdminError::store_unavailable()
        }
        Err(CommitError::Builder) => AdminError::internal(),
    };
    if err.status != StatusCode::FORBIDDEN {
        call.audit(action, resource, Outcome::Failed, err.status).await;
    }
    Err(err.with_resource(resource))
}

/// RFC 7396: `null` removes a member, objects merge, anything else replaces.
pub(crate) fn merge_patch(target: &mut Value, patch: &Value) {
    let Value::Object(p) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    if let Value::Object(t) = target {
        for (k, v) in p {
            if v.is_null() {
                t.remove(k);
            } else {
                merge_patch(t.entry(k.clone()).or_insert(Value::Null), v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn merge_patch_rfc7396() {
        let mut t = json!({"a": "b", "c": {"d": "e", "f": "g"}});
        merge_patch(&mut t, &json!({"a": "z", "c": {"f": null}}));
        assert_eq!(t, json!({"a": "z", "c": {"d": "e"}}));
        let mut t = json!({"x": 1});
        merge_patch(&mut t, &json!({"x": {"y": 2}}));
        assert_eq!(t, json!({"x": {"y": 2}}));
    }
}
