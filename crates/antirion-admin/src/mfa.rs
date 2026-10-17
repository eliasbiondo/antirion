//! A user's own second factors: list, enrol (TOTP), confirm, remove, and regenerate the recovery
//! codes. Secrets and codes are in the response that creates them, and never again.

use antirion_core::audit::Outcome;
use antirion_runtime::Runtime;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Map, Value, json};

use crate::call::Call;
use crate::error::AdminError;
use crate::local::{json_response, proof};

/// The human user a call acts for; service accounts and the bootstrap credential have no factors.
pub(crate) fn own_user(call: &Call) -> Result<String, AdminError> {
    call.principal
        .user
        .clone()
        .filter(|u| call.snap.identity.users.get(u).is_some_and(|u| u.human))
        .ok_or_else(AdminError::needs_human)
}

pub(crate) async fn start_totp(rt: &Runtime, user: &str) -> Result<Response, AdminError> {
    let (id, secret, uri) = rt.enrol_totp(user).await.map_err(AdminError::identity)?;
    Ok(json_response(
        StatusCode::CREATED,
        &json!({"factor_id": id, "secret": secret, "otpauth_uri": uri}),
    ))
}

/// Confirms a TOTP factor from an `MfaConfirm` body; the recovery codes of a first factor.
pub(crate) async fn confirm_totp(
    rt: &Runtime,
    user: &str,
    m: &Map<String, Value>,
) -> Result<Vec<String>, AdminError> {
    let id = m
        .get("factor_id")
        .and_then(Value::as_str)
        .ok_or_else(|| AdminError::invalid_parameter("factor_id is required"))?;
    let code = m
        .get("totp")
        .and_then(Value::as_str)
        .ok_or_else(|| AdminError::invalid_parameter("totp is required"))?;
    let name = m.get("name").and_then(Value::as_str).map(str::to_owned);
    rt.confirm_totp(user, id, code, name)
        .await
        .map_err(AdminError::identity)
}

pub(crate) async fn start_webauthn(rt: &Runtime, user: &str, origin: &str) -> Result<Response, AdminError> {
    let (id, options) = rt
        .enrol_webauthn(user, origin)
        .await
        .map_err(AdminError::identity)?;
    Ok(json_response(
        StatusCode::CREATED,
        &json!({"factor_id": id, "webauthn_options": options}),
    ))
}

/// Confirms a WebAuthn factor from an `MfaConfirm` body holding the browser's registration.
pub(crate) async fn confirm_webauthn(
    rt: &Runtime,
    user: &str,
    m: &Map<String, Value>,
    origin: &str,
) -> Result<Vec<String>, AdminError> {
    let id = m
        .get("factor_id")
        .and_then(Value::as_str)
        .ok_or_else(|| AdminError::invalid_parameter("factor_id is required"))?;
    let response = m
        .get("webauthn")
        .filter(|v| v.is_object())
        .ok_or_else(|| AdminError::invalid_parameter("webauthn is required"))?;
    let name = m.get("name").and_then(Value::as_str).map(str::to_owned);
    rt.confirm_webauthn(user, id, response, origin, name)
        .await
        .map_err(AdminError::identity)
}

fn body(call: &Call) -> Result<Map<String, Value>, AdminError> {
    if call.body.is_empty() {
        Ok(Map::new())
    } else {
        call.json_body()
    }
}

/// `/auth/me/mfa…` with a session or token.
pub(crate) async fn handle(call: &Call, rest: &[&str]) -> Result<Response, AdminError> {
    call.allow_params(&[])?;
    let user = own_user(call)?;
    let resource = format!("users/{user}/mfa");
    match (rest, &call.method) {
        ([], &Method::GET) => {
            let (factors, remaining) = call.rt.factors(&user).await.map_err(AdminError::identity)?;
            let items: Vec<Value> = factors
                .iter()
                .map(|f| {
                    json!({
                        "id": f.id,
                        "type": f.kind.as_str(),
                        "name": f.name,
                        "created_at": f.created_at.to_rfc3339_seconds(),
                        "last_used_at": f.last_used_at.map(|t| t.to_rfc3339_seconds()),
                    })
                })
                .collect();
            Ok(call.json(
                StatusCode::OK,
                &json!({"items": items, "recovery_codes_remaining": remaining}),
            ))
        }
        ([], _) => Err(AdminError::method_not_allowed("GET")),
        (["totp"], &Method::POST) => {
            let resp = start_totp(&call.rt, &user).await?;
            call.audit("auth.mfa.enrol", &resource, Outcome::Success, StatusCode::CREATED)
                .await;
            Ok(resp)
        }
        (["totp:confirm"], &Method::POST) => {
            let m = body(call)?;
            match confirm_totp(&call.rt, &user, &m).await {
                Ok(codes) => {
                    call.audit("auth.mfa.confirm", &resource, Outcome::Success, StatusCode::OK)
                        .await;
                    call.audit_read("auth.mfa.recovery_codes_revealed", &resource)
                        .await;
                    Ok(call.json(StatusCode::OK, &json!({"recovery_codes": codes})))
                }
                Err(e) => {
                    call.audit("auth.mfa.confirm", &resource, Outcome::Failed, e.status)
                        .await;
                    Err(e)
                }
            }
        }
        (["webauthn" | "webauthn:confirm"], &Method::POST) => {
            Err(AdminError::invalid_parameter("webauthn is not an allowed factor"))
        }
        ([":regenerate_recovery_codes"], &Method::POST) => {
            let m = body(call)?;
            let proof =
                proof(&m)?.ok_or_else(|| AdminError::invalid_parameter("a second factor is required"))?;
            match call.rt.regenerate_recovery_codes(&user, &proof).await {
                Ok(codes) => {
                    call.audit(
                        "auth.mfa.regenerate_recovery_codes",
                        &resource,
                        Outcome::Success,
                        StatusCode::OK,
                    )
                    .await;
                    call.audit_read("auth.mfa.recovery_codes_revealed", &resource)
                        .await;
                    Ok(call.json(StatusCode::OK, &json!({"recovery_codes": codes})))
                }
                Err(e) => {
                    let e = AdminError::identity(e);
                    call.audit(
                        "auth.mfa.regenerate_recovery_codes",
                        &resource,
                        Outcome::Failed,
                        e.status,
                    )
                    .await;
                    Err(e)
                }
            }
        }
        ([id], &Method::DELETE) if !id.contains(':') => {
            call.rt
                .delete_factor(&user, id)
                .await
                .map_err(AdminError::identity)?;
            call.audit(
                "auth.mfa.delete",
                &format!("{resource}/{id}"),
                Outcome::Success,
                StatusCode::NO_CONTENT,
            )
            .await;
            Ok(call.no_content())
        }
        ([id], _) if !id.contains(':') => Err(AdminError::method_not_allowed("DELETE")),
        (_, &Method::POST) => Err(AdminError::not_found()),
        _ => Err(AdminError::method_not_allowed("POST")),
    }
}
