//! `/auth/me/push`: the signed-in user's browser push subscriptions and the gateway's VAPID public
//! key; `:test` sends a test notification to each of their browsers.

use antirion_core::audit::Outcome;
use antirion_runtime::push::PushError;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;
use crate::mfa::own_user;

fn refused(e: PushError) -> AdminError {
    match e {
        PushError::Endpoint(m) => AdminError::invalid_parameter(format!("endpoint: {m}")),
        PushError::Keys(field) => AdminError::invalid_parameter(field.to_owned()),
        PushError::TooMany => AdminError::conflict("limit_reached", e.to_string(), Vec::new()),
        PushError::Store(_) => AdminError::store_unavailable(),
        PushError::Key => AdminError::internal(),
    }
}

/// `GET /auth/me/push`.
pub(crate) async fn overview(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&[])?;
    let user = own_user(call)?;
    let public_key = call.rt.push_public_key().await.map_err(refused)?;
    let subscriptions = call.rt.push_subscriptions(&user).await.map_err(refused)?;
    Ok(call.json(
        StatusCode::OK,
        &json!({"public_key": public_key, "subscriptions": subscriptions}),
    ))
}

fn text<'a>(body: &'a serde_json::Map<String, Value>, pointer: &str) -> Option<&'a str> {
    let mut parts = pointer.split('/');
    let first = body.get(parts.next()?)?;
    parts.try_fold(first, |v, p| v.get(p))?.as_str()
}

/// `GET` and `POST /auth/me/push/subscriptions`.
pub(crate) async fn subscriptions(call: &Call) -> Result<Response, AdminError> {
    call.allow_params(&[])?;
    let user = own_user(call)?;
    match call.method {
        Method::GET => {
            let list = call.rt.push_subscriptions(&user).await.map_err(refused)?;
            Ok(call.json(StatusCode::OK, &json!({"items": list})))
        }
        Method::POST => {
            let body = call.json_body()?;
            if let Some(k) = body
                .keys()
                .find(|k| !matches!(k.as_str(), "endpoint" | "keys" | "device" | "expirationTime"))
            {
                return Err(AdminError::invalid_parameter(format!(
                    "{k} is not a subscription member"
                )));
            }
            let required = |p: &str| {
                text(&body, p).ok_or_else(|| {
                    AdminError::invalid_parameter(format!("{} is required", p.replace('/', ".")))
                })
            };
            let (endpoint, p256dh, auth) = (
                required("endpoint")?,
                required("keys/p256dh")?,
                required("keys/auth")?,
            );
            let device = text(&body, "device");
            let created = call
                .rt
                .subscribe_push(&user, endpoint, p256dh, auth, device)
                .await
                .map_err(refused)?;
            call.audit_with(
                "auth.me.push.subscribe",
                &format!("users/{user}/push_subscriptions/{}", created.id),
                Outcome::Success,
                StatusCode::CREATED,
                json!({"origin": created.origin, "device": created.device}),
            )
            .await;
            let value = serde_json::to_value(&created).map_err(|_| AdminError::internal())?;
            Ok(call.json(StatusCode::CREATED, &value))
        }
        _ => Err(AdminError::method_not_allowed("GET, POST")),
    }
}

/// `DELETE /auth/me/push/subscriptions/{id}`.
pub(crate) async fn subscription(call: &Call, id: &str) -> Result<Response, AdminError> {
    if call.method != Method::DELETE {
        return Err(AdminError::method_not_allowed("DELETE"));
    }
    call.allow_params(&[])?;
    let user = own_user(call)?;
    if !call.rt.unsubscribe_push(&user, id).await.map_err(refused)? {
        return Err(AdminError::not_found());
    }
    call.audit(
        "auth.me.push.unsubscribe",
        &format!("users/{user}/push_subscriptions/{id}"),
        Outcome::Success,
        StatusCode::NO_CONTENT,
    )
    .await;
    Ok(call.no_content())
}

/// `POST /auth/me/push:test`.
pub(crate) async fn test(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&[])?;
    let user = own_user(call)?;
    let results = call.rt.push_test(&user).await.map_err(refused)?;
    Ok(call.json(StatusCode::OK, &json!({"results": results})))
}
