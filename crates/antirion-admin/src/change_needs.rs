//! The write permissions deciding a change request needs: recorded when it is opened, checked
//! again against what its change set stages to at every decision and at the apply.

use antirion_identity::rbac::Scope;
use serde_json::{Value, json};

use crate::error::AdminError;

/// What deciding a change set needs: the permissions recorded when it was opened and those it
/// stages to now. A change set that does not stage is refused, never taken as needing nothing.
pub(crate) fn needed_from(
    stored: Option<Vec<(String, Vec<Scope>)>>,
    staged: Result<Vec<(String, Vec<Scope>)>, AdminError>,
) -> Result<Vec<(String, Vec<Scope>)>, AdminError> {
    let staged = staged.map_err(|e| no_longer_applies(&e))?;
    Ok(merge_needed(stored.unwrap_or_default(), staged))
}

/// The refusal of a decision on a change set that no longer stages against the configuration.
fn no_longer_applies(cause: &AdminError) -> AdminError {
    AdminError::conflict(
        "conflict",
        format!(
            "The change set no longer applies to the configuration ({}); it cannot be approved until it does.",
            cause.message
        ),
        cause.details.clone().unwrap_or_default(),
    )
}

/// The permissions of `stored` and every one of `staged` it lacks.
fn merge_needed(
    mut stored: Vec<(String, Vec<Scope>)>,
    staged: Vec<(String, Vec<Scope>)>,
) -> Vec<(String, Vec<Scope>)> {
    for p in staged {
        if !stored.contains(&p) {
            stored.push(p);
        }
    }
    stored
}

pub(crate) fn encode_needed(needed: &[(String, Vec<Scope>)]) -> Value {
    Value::Array(
        needed
            .iter()
            .map(|(p, scopes)| json!([p, scopes.iter().map(ToString::to_string).collect::<Vec<_>>()]))
            .collect(),
    )
}

/// The permissions recorded when the request was opened; `None` for a request stored before they
/// were, or a record that does not read.
pub(crate) fn stored_needed(body: &Value) -> Option<Vec<(String, Vec<Scope>)>> {
    body.get("_needed")?
        .as_array()?
        .iter()
        .map(|entry| {
            let permission = entry.get(0)?.as_str()?.to_owned();
            let scopes = entry
                .get(1)?
                .as_array()?
                .iter()
                .map(|s| s.as_str().and_then(Scope::parse))
                .collect::<Option<Vec<_>>>()?;
            Some((permission, scopes))
        })
        .collect()
}

/// Whether the permissions recorded when the request was opened cover what it stages now.
pub(crate) fn covered(stored: Option<&[(String, Vec<Scope>)]>, staged: &[(String, Vec<Scope>)]) -> bool {
    stored.is_none_or(|stored| staged.iter().all(|p| stored.contains(p)))
}

#[cfg(test)]
mod tests {
    use http::StatusCode;

    use super::*;

    fn write(p: &str, scopes: &[Scope]) -> (String, Vec<Scope>) {
        (p.to_owned(), scopes.to_vec())
    }

    #[test]
    fn a_change_set_that_no_longer_stages_is_refused_not_free() {
        let stage_error = AdminError::conflict("already_exists", "teams/x exists.", Vec::new());
        let refused = needed_from(None, Err(stage_error)).err();
        assert_eq!(refused.as_ref().map(|e| e.status), Some(StatusCode::CONFLICT));
        assert!(refused.is_some_and(|e| e.message.contains("teams/x exists.")));
        let stored = vec![write("teams:create", &[Scope::Gateway])];
        let stage_error = AdminError::not_found();
        assert!(needed_from(Some(stored), Err(stage_error)).is_err());
    }

    #[test]
    fn deciding_needs_the_recorded_and_the_staged_permissions() {
        let stored = vec![write("teams:create", &[Scope::Gateway])];
        let staged = vec![
            write("teams:create", &[Scope::Gateway]),
            write("routes:update", &[Scope::Gateway]),
        ];
        let needed = needed_from(Some(stored), Ok(staged.clone())).unwrap_or_default();
        assert_eq!(needed, staged);
        assert_eq!(needed_from(None, Ok(Vec::new())).unwrap_or_default(), Vec::new());
    }

    #[test]
    fn recorded_permissions_round_trip_and_cover_only_what_they_name() {
        let needed = vec![
            write(
                "teams:create",
                &[
                    Scope::Team("t".into()),
                    Scope::Organization("o".into()),
                    Scope::Gateway,
                ],
            ),
            write("config:apply", &[Scope::Gateway]),
        ];
        let body = json!({"_needed": encode_needed(&needed)});
        let read = stored_needed(&body);
        assert_eq!(read.as_deref(), Some(needed.as_slice()));
        assert!(covered(read.as_deref(), &needed[..1]));
        assert!(!covered(
            read.as_deref(),
            &[write("keys:create", &[Scope::Gateway])]
        ));
        assert!(covered(None, &[write("keys:create", &[Scope::Gateway])]));
        assert_eq!(stored_needed(&json!({})), None);
        assert_eq!(stored_needed(&json!({"_needed": [["x", ["cluster/y"]]]})), None);
    }
}
