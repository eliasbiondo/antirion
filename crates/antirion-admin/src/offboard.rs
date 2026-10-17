//! `POST /users/{name}:offboard`: in one configuration change the user is disabled, loses its role
//! bindings, and every key it owns is disabled, deleted or given to another user; then its sessions
//! are ended and its admin tokens deleted. One audit entry records the change and the reason.

use antirion_config::model;
use antirion_config::{ConfigError, DetailCode, DocPath};
use antirion_identity::rbac::Scope;
use antirion_runtime::changes::Target;
use antirion_runtime::{MetaEdit, Mutation};
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Map, Value, json};

use crate::call::Call;
use crate::error::AdminError;
use crate::perms::{chain, owner};

const MAX_REASON: usize = 500;

/// What happens to the keys the user owns.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum KeyAction {
    Disable,
    Delete,
    Transfer,
}

impl KeyAction {
    fn parse(s: &str) -> Option<KeyAction> {
        match s {
            "disable" => Some(KeyAction::Disable),
            "delete" => Some(KeyAction::Delete),
            "transfer" => Some(KeyAction::Transfer),
            _ => None,
        }
    }

    fn permission(self) -> &'static str {
        match self {
            KeyAction::Delete => "keys:delete",
            KeyAction::Disable | KeyAction::Transfer => "keys:update",
        }
    }

    fn past(self) -> &'static str {
        match self {
            KeyAction::Disable => "disabled",
            KeyAction::Delete => "deleted",
            KeyAction::Transfer => "transferred",
        }
    }
}

struct Request {
    keys: KeyAction,
    transfer_to: Option<String>,
    reason: Option<String>,
}

fn invalid(field: &str, code: DetailCode, message: &str) -> AdminError {
    AdminError::validation(vec![ConfigError::new(&DocPath::root().key(field), code, message)])
}

fn request(call: &Call, user: &str) -> Result<Request, AdminError> {
    let body = call.json_body()?;
    let keys = match body.get("keys").and_then(Value::as_str) {
        Some(k) => KeyAction::parse(k).ok_or_else(|| {
            invalid(
                "keys",
                DetailCode::InvalidEnum,
                "keys must be disable, delete or transfer",
            )
        })?,
        None => return Err(invalid("keys", DetailCode::Required, "keys is required")),
    };
    let transfer_to = match body.get("transfer_to") {
        None | Some(Value::Null) => None,
        Some(Value::String(t)) => Some(t.clone()),
        Some(_) => {
            return Err(invalid(
                "transfer_to",
                DetailCode::TypeMismatch,
                "transfer_to must be a user name",
            ));
        }
    };
    let reason = match body.get("reason") {
        None | Some(Value::Null) => None,
        Some(Value::String(r)) if r.chars().count() <= MAX_REASON => Some(r.clone()),
        Some(_) => {
            return Err(invalid(
                "reason",
                DetailCode::OutOfRange,
                "reason must be a string of at most 500 characters",
            ));
        }
    };
    match (keys, &transfer_to) {
        (KeyAction::Transfer, None) => {
            return Err(invalid(
                "transfer_to",
                DetailCode::Required,
                "transferring keys needs transfer_to",
            ));
        }
        (KeyAction::Transfer, Some(to)) => {
            let target = call.snap.config.doc.pointer(&format!("/users/{to}"));
            let Some(target) = target.filter(|_| to != user) else {
                return Err(invalid(
                    "transfer_to",
                    DetailCode::UnresolvedReference,
                    "transfer_to must name another existing user",
                ));
            };
            if target.get("enabled") == Some(&Value::Bool(false)) {
                return Err(invalid(
                    "transfer_to",
                    DetailCode::Conflict,
                    "transfer_to names a disabled user",
                ));
            }
        }
        (_, Some(_)) => {
            return Err(invalid(
                "transfer_to",
                DetailCode::Conflict,
                "transfer_to is only for keys: transfer",
            ));
        }
        (_, None) => {}
    }
    Ok(Request {
        keys,
        transfer_to,
        reason,
    })
}

/// The keys `user` owns in `doc`, by name.
fn owned(doc: &Value, user: &str) -> Vec<String> {
    doc.get("keys")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(_, k)| k.get("owner").and_then(Value::as_str) == Some(user))
        .map(|(name, _)| name.clone())
        .collect()
}

/// The document after offboarding `user`, and the meta edits and written targets it makes.
fn offboarded(doc: &Value, user: &str, req: &Request) -> (Value, Vec<MetaEdit>, Vec<Target>, Vec<String>) {
    let mut doc = doc.clone();
    let keys = owned(&doc, user);
    if let Some(spec) = doc
        .pointer_mut(&format!("/users/{user}"))
        .and_then(Value::as_object_mut)
    {
        spec.insert("enabled".into(), Value::Bool(false));
        spec.remove("role_bindings");
    }
    let mut edits = Vec::new();
    if let Some(map) = doc.get_mut("keys").and_then(Value::as_object_mut) {
        for name in &keys {
            match req.keys {
                KeyAction::Delete => {
                    map.remove(name);
                }
                KeyAction::Disable => {
                    if let Some(k) = map.get_mut(name).and_then(Value::as_object_mut) {
                        k.insert("enabled".into(), Value::Bool(false));
                    }
                    edits.push(MetaEdit::DisabledReason {
                        key: name.clone(),
                        reason: "owner_offboarded".to_owned(),
                    });
                }
                KeyAction::Transfer => {
                    if let (Some(k), Some(to)) =
                        (map.get_mut(name).and_then(Value::as_object_mut), &req.transfer_to)
                    {
                        k.insert("owner".into(), Value::from(to.clone()));
                    }
                }
            }
        }
    }
    let written = std::iter::once(Target::Resource("users", user.to_owned()))
        .chain(keys.iter().map(|k| Target::Resource("keys", k.clone())))
        .collect();
    (doc, edits, written, keys)
}

pub(crate) async fn offboard(call: &Call, user: &str) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&["wait"])?;
    call.check_wait()?;
    let resource = format!("users/{user}");
    call.require("users:update", &[Scope::Gateway])
        .map_err(|e| e.with_resource(resource.clone()))?;
    if call.snap.config.doc.pointer(&format!("/users/{user}")).is_none() {
        return Err(AdminError::not_found());
    }
    if call.principal.user.as_deref() == Some(user) {
        return Err(AdminError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "A principal cannot offboard itself.",
        )
        .with_resource(resource));
    }
    let req = request(call, user)?;
    let keys_def = model::collection("keys").ok_or_else(AdminError::internal)?;
    for name in owned(&call.snap.config.doc, user) {
        let spec = call.snap.config.resource(keys_def, &name).map(|(s, _)| s);
        call.require_owned(
            req.keys.permission(),
            &chain(&call.snap, "keys", &name, spec),
            owner("keys", spec),
        )
        .map_err(|e| e.with_resource(format!("keys/{name}")))?;
    }
    let action = "users.offboard";
    let mut audit = call.audit_context(action, &resource, StatusCode::OK);
    if audit.reason.is_none() {
        audit.reason.clone_from(&req.reason);
    }
    let mut touched = Vec::new();
    let result = call
        .rt
        .mutate(|state| {
            let (candidate, meta_edits, written, keys) = offboarded(&state.doc, user, &req);
            if !call.approved && crate::change_control::governed(&state.doc, &candidate) {
                return Err(crate::change_control::required());
            }
            touched = keys;
            Ok(Mutation {
                candidate,
                writer: "api".to_owned(),
                written,
                credentials: Vec::new(),
                meta_edits,
                dry_run: false,
                source: "api".to_owned(),
                audit: Some(audit),
            })
        })
        .await;
    let committed = crate::commit::finish(call, action, &resource, result).await?;
    let sessions = call
        .rt
        .end_user_sessions(user, None)
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    let store = &call.rt.store;
    let tokens = store
        .admin_tokens_of(user.to_owned())
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    for token in &tokens {
        store
            .forget_token(token.kid.clone())
            .await
            .map_err(|_| AdminError::store_unavailable())?;
    }
    store
        .delete_admin_tokens_of(user.to_owned())
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    let keys: Vec<Value> = touched
        .iter()
        .map(|k| json!({"name": k, "action": req.keys.past()}))
        .collect();
    let mut out = Map::new();
    out.insert("user".into(), Value::from(user));
    out.insert("keys".into(), Value::Array(keys));
    out.insert("sessions_revoked".into(), Value::from(sessions));
    out.insert("tokens_deleted".into(), Value::from(tokens.len()));
    let resp = call.json(StatusCode::OK, &Value::Object(out));
    Ok(call.mutated(resp, committed.generation).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(keys: KeyAction, to: Option<&str>) -> Request {
        Request {
            keys,
            transfer_to: to.map(str::to_owned),
            reason: None,
        }
    }

    #[test]
    fn the_user_is_disabled_and_its_keys_follow_the_choice() {
        let doc = json!({
            "users": {"a": {"kind": "human", "role_bindings": [{"role": "viewer", "scope": "gateway"}]}, "b": {}},
            "keys": {"k1": {"owner": "a"}, "k2": {"owner": "a"}, "k3": {"owner": "b"}}
        });
        let (out, edits, written, keys) = offboarded(&doc, "a", &req(KeyAction::Transfer, Some("b")));
        assert_eq!(keys, vec!["k1", "k2"]);
        assert_eq!(out.pointer("/users/a/enabled"), Some(&Value::Bool(false)));
        assert!(out.pointer("/users/a/role_bindings").is_none());
        assert_eq!(out.pointer("/keys/k1/owner"), Some(&json!("b")));
        assert!(edits.is_empty() && written.len() == 3);
        let (out, edits, ..) = offboarded(&doc, "a", &req(KeyAction::Disable, None));
        assert_eq!(out.pointer("/keys/k2/enabled"), Some(&Value::Bool(false)));
        assert_eq!(edits.len(), 2);
        let (out, ..) = offboarded(&doc, "a", &req(KeyAction::Delete, None));
        assert!(out.pointer("/keys/k1").is_none() && out.pointer("/keys/k3").is_some());
    }
}
