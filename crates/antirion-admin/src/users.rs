//! Admin tokens of users and service accounts: issued once, stored as verifiers, revoked by delete.

use antirion_core::UtcMicros;
use antirion_core::audit::Outcome;
use antirion_identity::rbac::Scope;
use antirion_identity::{Credential, Plane};
use antirion_store::AdminTokenRecord;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;

fn token_json(r: &AdminTokenRecord) -> Value {
    json!({"kid": r.kid, "user": r.user, "description": r.description, "created_at": r.created_at, "expires_at": r.expires_at})
}

fn user_exists(call: &Call, user: &str) -> bool {
    call.snap
        .config
        .doc
        .get("users")
        .and_then(|u| u.get(user))
        .is_some()
}

/// `GET` and `POST /users/{name}/tokens`.
pub(crate) async fn tokens(call: &Call, user: &str) -> Result<Response, AdminError> {
    let resource = format!("users/{user}/tokens");
    match call.method {
        Method::GET => {
            call.allow_params(&[])?;
            call.require("users:read", &[Scope::Gateway])?;
            if !user_exists(call, user) {
                return Err(AdminError::not_found());
            }
            let records = call
                .rt
                .store
                .admin_tokens_of(user.to_owned())
                .await
                .map_err(|_| AdminError::store_unavailable())?;
            let items: Vec<Value> = records.iter().map(token_json).collect();
            Ok(call.json(StatusCode::OK, &json!({"items": items, "next_cursor": null})))
        }
        Method::POST => {
            call.allow_params(&[])?;
            call.require("users:update", &[Scope::Gateway])
                .map_err(|e| e.with_resource(resource.clone()))?;
            if !user_exists(call, user) {
                return Err(AdminError::not_found());
            }
            let body: Value = if call.body.is_empty() {
                json!({})
            } else {
                Value::Object(call.json_body()?)
            };
            let expires_at = match body.get("expires_at") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) => Some(
                    UtcMicros::parse_rfc3339(s)
                        .map_err(|_| AdminError::invalid_parameter("expires_at must be an RFC 3339 time"))?
                        .to_rfc3339(),
                ),
                Some(_) => {
                    return Err(AdminError::invalid_parameter(
                        "expires_at must be an RFC 3339 time",
                    ));
                }
            };
            let cred = Credential::generate(Plane::Admin).map_err(|_| AdminError::internal())?;
            let record = AdminTokenRecord {
                kid: cred.kid().to_owned(),
                user: user.to_owned(),
                verifier: call.rt.pepper.verifier(cred.expose()),
                description: body.get("description").and_then(Value::as_str).map(str::to_owned),
                created_at: call.rt.now().to_rfc3339_seconds(),
                expires_at,
                access: None,
                last_used_us: None,
            };
            call.rt
                .store
                .put_admin_token(record.clone())
                .await
                .map_err(|_| AdminError::store_unavailable())?;
            call.audit(
                "users.tokens.create",
                &resource,
                Outcome::Success,
                StatusCode::CREATED,
            )
            .await;
            call.audit_read("users.tokens.revealed", &resource).await;
            let mut out = token_json(&record);
            if let Some(obj) = out.as_object_mut() {
                obj.insert("token".into(), Value::from(cred.expose()));
                obj.insert("hint".into(), Value::from(cred.hint()));
            }
            Ok(call.json(StatusCode::CREATED, &out))
        }
        _ => Err(AdminError::method_not_allowed("GET, POST")),
    }
}

/// `DELETE /users/{name}/tokens/{kid}`.
pub(crate) async fn token(call: &Call, user: &str, kid: &str) -> Result<Response, AdminError> {
    if call.method != Method::DELETE {
        return Err(AdminError::method_not_allowed("DELETE"));
    }
    call.allow_params(&[])?;
    let resource = format!("users/{user}/tokens/{kid}");
    call.require("users:update", &[Scope::Gateway])
        .map_err(|e| e.with_resource(resource.clone()))?;
    let record = call
        .rt
        .store
        .admin_token(kid.to_owned())
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    if record.is_none_or(|r| r.user != user) {
        return Err(AdminError::not_found());
    }
    call.rt
        .store
        .delete_admin_token(kid.to_owned())
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    call.rt
        .store
        .forget_token(kid.to_owned())
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    call.audit(
        "users.tokens.delete",
        &resource,
        Outcome::Success,
        StatusCode::NO_CONTENT,
    )
    .await;
    Ok(call.no_content())
}

/// `POST /users/{name}:reset_mfa`: removes every second factor and recovery code of another user
/// and ends its sessions; its next sign-in enrols a factor.
pub(crate) async fn reset_mfa(call: &Call, user: &str) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&[])?;
    let resource = format!("users/{user}");
    call.require("users:update", &[Scope::Gateway])
        .map_err(|e| e.with_resource(resource.clone()))?;
    if !user_exists(call, user) {
        return Err(AdminError::not_found());
    }
    if call.principal.user.as_deref() == Some(user) {
        return Err(AdminError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "A principal cannot reset its own second factors.",
        )
        .with_resource(resource));
    }
    let reason = if call.body.is_empty() {
        None
    } else {
        match call.json_body()?.get("reason") {
            None | Some(Value::Null) => None,
            Some(Value::String(r)) if r.chars().count() <= 500 => Some(r.clone()),
            Some(_) => {
                return Err(AdminError::invalid_parameter(
                    "reason must be at most 500 characters",
                ));
            }
        }
    };
    call.rt.reset_factors(user).await.map_err(AdminError::identity)?;
    call.audit_with(
        "users.reset_mfa",
        &resource,
        Outcome::Success,
        StatusCode::NO_CONTENT,
        json!({"reason": reason}),
    )
    .await;
    Ok(call.no_content())
}

/// Removes a deleted user's admin tokens with their sessions and bindings, its other sessions and
/// its identity state (password, factors, preferences).
pub(crate) async fn forget_user(rt: &antirion_runtime::Runtime, user: &str) {
    let store = &rt.store;
    let result = async {
        for token in store.admin_tokens_of(user.to_owned()).await? {
            store.forget_token(token.kid).await?;
        }
        store.delete_admin_tokens_of(user.to_owned()).await?;
        rt.end_user_sessions(user, None).await?;
        rt.forget_identity(user).await
    }
    .await;
    if let Err(e) = result {
        tracing::warn!(error = %e, "the tokens of a deleted user could not be removed");
    }
}
