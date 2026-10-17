//! The public checks of an invitation or password-reset link: the console asks before it shows a
//! form, and the token stays usable. Answers other than `valid` count against the caller's
//! throttle, as a wrong token does on the endpoints that use it.

use antirion_runtime::operators::TokenState;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::error::AdminError;
use crate::local::{Public, json_response};

fn token(p: &Public) -> Result<String, AdminError> {
    if p.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    p.object()?
        .get("token")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| AdminError::invalid_parameter("token is required"))
}

fn counted(p: &Public, state: TokenState) {
    if state != TokenState::Valid {
        p.rt.count_failure(p.peer.as_deref());
    }
}

/// `POST /auth/invitations:check`.
pub(crate) async fn invitation(p: &Public) -> Result<Response, AdminError> {
    let token = token(p)?;
    p.rt.throttle_client(p.peer.as_deref())
        .map_err(AdminError::identity)?;
    let check =
        p.rt.check_invitation(&token)
            .await
            .map_err(AdminError::identity)?;
    counted(p, check.state);
    let snap = p.rt.gens.load();
    let organization = check.organization.as_ref().map(|name| {
        let display = snap
            .config
            .doc
            .get("organizations")
            .and_then(|o| o.get(name))
            .and_then(|o| o.get("display_name"))
            .and_then(Value::as_str)
            .unwrap_or(name);
        json!({"name": name, "display_name": display})
    });
    let inv = check.invitation.as_ref();
    let body = json!({
        "state": check.state.as_str(),
        "email": inv.map(|i| i.email.clone()),
        "organization": organization,
        "expires_at": inv.map(|i| i.expires_at.to_rfc3339_seconds()),
    });
    Ok(json_response(StatusCode::OK, &body))
}

/// `POST /auth/local/password_reset:check`: not found while resets are not offered, like the two
/// reset endpoints.
pub(crate) async fn reset(p: &Public) -> Result<Response, AdminError> {
    if !p.rt.resets_enabled() {
        return Err(AdminError::not_found());
    }
    let token = token(p)?;
    p.rt.throttle_client(p.peer.as_deref())
        .map_err(AdminError::identity)?;
    let check = p.rt.check_reset(&token).await.map_err(AdminError::identity)?;
    counted(p, check.state);
    let body = json!({
        "state": check.state.as_str(),
        "expires_at": check.expires_at.map(antirion_core::UtcMicros::to_rfc3339_seconds),
    });
    Ok(json_response(StatusCode::OK, &body))
}
