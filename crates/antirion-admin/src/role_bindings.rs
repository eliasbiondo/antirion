//! Role bindings and roles. A binding of a user is an entry of `users.<name>.role_bindings`, so it
//! commits a generation; a binding of a single admin token is kept in the store. Binding ids are
//! derived from the binding itself.

use antirion_config::model;
use antirion_core::audit::Outcome;
use antirion_identity::rbac::Scope;
use antirion_store::TokenBindingRecord;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::call::Call;
use crate::commit::{Proposal, api_write};
use crate::error::AdminError;
use crate::pagination;
use crate::perms::scope_chain;

/// One binding, whoever holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BindingRow {
    pub(crate) id: String,
    /// `user` or `token`.
    pub(crate) kind: &'static str,
    pub(crate) principal: String,
    pub(crate) role: String,
    pub(crate) scope: String,
}

pub(crate) fn binding_id(kind: &str, principal: &str, role: &str, scope: &str) -> String {
    let digest = Sha256::digest(format!("{kind}\n{principal}\n{role}\n{scope}").as_bytes());
    let hex: String = digest.iter().take(10).map(|b| format!("{b:02x}")).collect();
    format!("rb-{hex}")
}

impl BindingRow {
    fn new(kind: &'static str, principal: &str, role: &str, scope: &str) -> BindingRow {
        BindingRow {
            id: binding_id(kind, principal, role, scope),
            kind,
            principal: principal.to_owned(),
            role: role.to_owned(),
            scope: scope.to_owned(),
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "principal": {"type": self.kind, "id": self.principal},
            "role": self.role,
            "scope": self.scope,
        })
    }

    fn chain(&self, call: &Call) -> Vec<Scope> {
        Scope::parse(&self.scope).map_or_else(|| vec![Scope::Gateway], |s| scope_chain(&call.snap, &s))
    }

    /// Whether the caller holds this binding itself.
    fn is_own(&self, call: &Call) -> bool {
        match self.kind {
            "user" => call.principal.user.as_deref() == Some(self.principal.as_str()),
            _ => call.principal.id == self.principal,
        }
    }
}

/// The bindings of the users in the document.
pub(crate) fn user_bindings(doc: &Value) -> Vec<BindingRow> {
    let mut out = Vec::new();
    for (user, spec) in doc.get("users").and_then(Value::as_object).into_iter().flatten() {
        for b in spec
            .get("role_bindings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let (Some(role), Some(scope)) = (
                b.get("role").and_then(Value::as_str),
                b.get("scope").and_then(Value::as_str),
            ) {
                out.push(BindingRow::new("user", user, role, scope));
            }
        }
    }
    out
}

async fn all_bindings(call: &Call) -> Result<Vec<BindingRow>, AdminError> {
    let mut rows = user_bindings(&call.snap.config.doc);
    let tokens = call
        .rt
        .store
        .token_bindings(None)
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    rows.extend(
        tokens
            .iter()
            .map(|t| BindingRow::new("token", &t.kid, &t.role, &t.scope)),
    );
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    rows.dedup_by(|a, b| a.id == b.id);
    Ok(rows)
}

/// `GET` and `POST /role_bindings`.
pub(crate) async fn collection(call: &Call) -> Result<Response, AdminError> {
    match call.method {
        Method::GET => list(call).await,
        Method::POST => create(call).await,
        _ => Err(AdminError::method_not_allowed("GET, POST")),
    }
}

/// `GET` and `DELETE /role_bindings/{id}`.
pub(crate) async fn one(call: &Call, id: &str) -> Result<Response, AdminError> {
    let rows = all_bindings(call).await?;
    let Some(row) = rows
        .into_iter()
        .find(|r| r.id == id)
        .filter(|r| call.principal.sees(&r.chain(call)))
    else {
        return Err(AdminError::not_found());
    };
    match call.method {
        Method::GET => {
            call.allow_params(&[])?;
            call.require("role_bindings:read", &row.chain(call))?;
            Ok(call.json(StatusCode::OK, &row.to_json()))
        }
        Method::DELETE => delete(call, &row).await,
        _ => Err(AdminError::method_not_allowed("GET, DELETE")),
    }
}

async fn list(call: &Call) -> Result<Response, AdminError> {
    call.allow_params(&[
        "limit",
        "cursor",
        "principal_type",
        "principal_id",
        "role",
        "scope",
    ])?;
    let (limit, after) = pagination::page_params(call, "role_bindings")?;
    let filter = |name: &str, value: &str| call.param(name).is_none_or(|v| v == value);
    let rows: Vec<BindingRow> = all_bindings(call)
        .await?
        .into_iter()
        .filter(|r| after.as_ref().is_none_or(|a| r.id.as_str() > a.as_str()))
        .filter(|r| {
            filter("principal_type", r.kind)
                && filter("principal_id", &r.principal)
                && filter("role", &r.role)
                && filter("scope", &r.scope)
        })
        .filter(|r| call.principal.allows("role_bindings:read", &r.chain(call)))
        .collect();
    let page: Vec<Value> = rows.iter().take(limit).map(BindingRow::to_json).collect();
    let next = (rows.len() > limit)
        .then(|| rows.get(limit - 1))
        .flatten()
        .map(|r| pagination::encode(&call.rt.cursor_key, "role_bindings", &r.id));
    Ok(call.json(StatusCode::OK, &json!({"items": page, "next_cursor": next})))
}

fn bad(message: &str) -> AdminError {
    AdminError::invalid_parameter(message.to_owned())
}

async fn create(call: &Call) -> Result<Response, AdminError> {
    call.allow_params(&[])?;
    let body = Value::Object(call.json_body()?);
    let kind = match body.pointer("/principal/type").and_then(Value::as_str) {
        Some("user") => "user",
        Some("token") => "token",
        _ => return Err(bad("principal.type must be user or token")),
    };
    let principal = body
        .pointer("/principal/id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad("principal.id is required"))?;
    let role_name = body.get("role").and_then(Value::as_str).unwrap_or_default();
    let role = call
        .snap
        .identity
        .roles
        .resolve(role_name)
        .ok_or_else(|| bad("role names no role"))?;
    let scope_text = body.get("scope").and_then(Value::as_str).unwrap_or_default();
    let scope = Scope::parse(scope_text).ok_or_else(|| bad("scope must be gateway or <kind>/<name>"))?;
    let kind_of_scope = scope_text.split('/').next().unwrap_or_default();
    if !role.bindable_at().contains(&kind_of_scope) {
        return Err(bad("the role cannot be bound at this scope"));
    }
    let row = BindingRow::new(kind, principal, role_name, scope_text);
    let resource = format!("role_bindings/{}", row.id);
    let chain = scope_chain(&call.snap, &scope);
    call.require("role_bindings:create", &chain)
        .map_err(|e| e.with_resource(resource.clone()))?;
    if !call.principal.may_grant(&role, &chain) || row.is_own(call) {
        return Err(AdminError::forbidden("a role the caller holds itself").with_resource(resource));
    }
    if all_bindings(call).await?.iter().any(|r| r.id == row.id) {
        return Err(AdminError::conflict(
            "already_exists",
            "This binding already exists.",
            Vec::new(),
        ));
    }
    let status = StatusCode::CREATED;
    let generation = if kind == "user" {
        if call
            .snap
            .config
            .doc
            .pointer(&format!("/users/{principal}"))
            .is_none()
        {
            return Err(AdminError::not_found());
        }
        let entry = json!({"role": role_name, "scope": scope_text});
        let committed = api_write(call, "role_bindings.create", &resource, status, |state| {
            let mut doc = state.doc.clone();
            let user = model::collection("users")
                .and_then(|c| model::collection_map_mut(&mut doc, c))
                .and_then(|m| m.get_mut(principal))
                .and_then(Value::as_object_mut)
                .ok_or_else(AdminError::not_found)?;
            let list = user
                .entry("role_bindings")
                .or_insert_with(|| Value::Array(Vec::new()));
            if let Some(a) = list.as_array_mut() {
                a.push(entry.clone());
            }
            Ok(Proposal {
                candidate: doc,
                written: vec![antirion_runtime::changes::Target::Resource(
                    "users",
                    principal.to_owned(),
                )],
                credentials: Vec::new(),
                meta_edits: Vec::new(),
            })
        })
        .await?;
        Some(committed.generation)
    } else {
        let token = call
            .rt
            .store
            .admin_token(principal.to_owned())
            .await
            .map_err(|_| AdminError::store_unavailable())?;
        if token.is_none() {
            return Err(AdminError::not_found());
        }
        call.rt
            .store
            .put_token_binding(TokenBindingRecord {
                id: row.id.clone(),
                kid: principal.to_owned(),
                role: role_name.to_owned(),
                scope: scope_text.to_owned(),
                created_at: call.rt.now().to_rfc3339_seconds(),
            })
            .await
            .map_err(|_| AdminError::store_unavailable())?;
        call.audit("role_bindings.create", &resource, Outcome::Success, status)
            .await;
        None
    };
    let resp = call.json(status, &row.to_json());
    let resp = match generation {
        Some(g) => call.mutated(resp, g).await,
        None => resp,
    };
    Ok(call.created(resp, &format!("/admin/v1/role_bindings/{}", row.id)))
}

async fn delete(call: &Call, row: &BindingRow) -> Result<Response, AdminError> {
    call.allow_params(&[])?;
    let resource = format!("role_bindings/{}", row.id);
    let chain = row.chain(call);
    call.require("role_bindings:delete", &chain)
        .map_err(|e| e.with_resource(resource.clone()))?;
    let role = call.snap.identity.roles.resolve(&row.role);
    if row.is_own(call) || !role.is_some_and(|r| call.principal.may_grant(&r, &chain)) {
        return Err(AdminError::forbidden("a role the caller holds itself").with_resource(resource));
    }
    if row.kind == "user" {
        let committed = api_write(
            call,
            "role_bindings.delete",
            &resource,
            StatusCode::NO_CONTENT,
            |state| {
                let mut doc = state.doc.clone();
                if let Some(list) = model::collection("users")
                    .and_then(|c| model::collection_map_mut(&mut doc, c))
                    .and_then(|m| m.get_mut(&row.principal))
                    .and_then(|u| u.get_mut("role_bindings"))
                    .and_then(Value::as_array_mut)
                {
                    list.retain(|b| {
                        b.get("role").and_then(Value::as_str) != Some(row.role.as_str())
                            || b.get("scope").and_then(Value::as_str) != Some(row.scope.as_str())
                    });
                }
                Ok(Proposal {
                    candidate: doc,
                    written: vec![antirion_runtime::changes::Target::Resource(
                        "users",
                        row.principal.clone(),
                    )],
                    credentials: Vec::new(),
                    meta_edits: Vec::new(),
                })
            },
        )
        .await?;
        return Ok(call.mutated(call.no_content(), committed.generation).await);
    }
    call.rt
        .store
        .delete_token_binding(row.id.clone())
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    call.audit(
        "role_bindings.delete",
        &resource,
        Outcome::Success,
        StatusCode::NO_CONTENT,
    )
    .await;
    Ok(call.no_content())
}

/// `GET /roles`: the built-in roles, then the custom ones, with their permissions.
pub(crate) fn roles(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&["limit", "cursor"])?;
    let readable = call
        .principal
        .bindings
        .iter()
        .any(|b| b.role.allows("role_bindings:read"));
    if !readable {
        return Err(AdminError::forbidden("role_bindings:read"));
    }
    let items: Vec<Value> = call
        .snap
        .identity
        .roles
        .all()
        .iter()
        .map(|r| {
            json!({"name": r.as_str(), "builtin": r.is_builtin(), "description": r.description(),
                   "bindable_at": r.bindable_at(), "permissions": r.permissions()})
        })
        .collect();
    Ok(call.json(StatusCode::OK, &json!({"items": items, "next_cursor": null})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_follow_the_binding() {
        let a = binding_id("user", "alice", "viewer", "team/t");
        assert!(a.starts_with("rb-") && a.len() == 23);
        assert_eq!(a, binding_id("user", "alice", "viewer", "team/t"));
        assert_ne!(a, binding_id("token", "alice", "viewer", "team/t"));
        let doc = json!({"users": {"alice": {"role_bindings": [{"role": "viewer", "scope": "team/t"}]}}});
        let rows = user_bindings(&doc);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, a);
    }
}
