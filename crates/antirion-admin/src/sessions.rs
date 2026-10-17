//! Session lists and revocation: a principal's own sessions, and an administrator's view of a
//! user's. A list shows a public id derived from the stored digest, never the cookie value.

use antirion_core::audit::Outcome;
use antirion_identity::rbac::Scope;
use antirion_runtime::admin_auth::SessionInfo;
use antirion_runtime::geoip::GeoDb;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;
use crate::pagination;

fn session_json(s: &SessionInfo, current: Option<&str>, geo: Option<&GeoDb>) -> Value {
    let location = geo
        .zip(s.facts.client_ip.as_deref())
        .and_then(|(db, ip)| db.locate(ip.parse().ok()?))
        .unwrap_or(Value::Null);
    json!({
        "id": s.id,
        "created_at": s.created_at.to_rfc3339_seconds(),
        "last_seen_at": s.last_seen_at.to_rfc3339_seconds(),
        "expires_at": s.expires_at.to_rfc3339_seconds(),
        "client_ip": s.facts.client_ip,
        "user_agent": s.facts.user_agent,
        "method": s.facts.method,
        "mfa": s.facts.mfa,
        "trusted": s.facts.trusted,
        "organization": s.facts.organization,
        "current": current == Some(s.digest.as_str()),
        "location": location,
    })
}

/// The caller's live sessions: those of its user, or those started with its own credential.
async fn own(call: &Call) -> Result<Vec<SessionInfo>, AdminError> {
    let (user, kid) = match &call.principal.user {
        Some(u) => (Some(u.as_str()), None),
        None => (None, Some(call.principal.id.as_str())),
    };
    call.rt
        .sessions_of(user, kid)
        .await
        .map_err(|_| AdminError::store_unavailable())
}

fn page(call: &Call, scope: &str, items: &[SessionInfo]) -> Result<Response, AdminError> {
    let (limit, after) = pagination::page_params(call, scope)?;
    let current = call.session.as_ref().map(|(_, s)| s.digest.as_str());
    let snap = call.rt.gens.load();
    let geo = snap.geoip.as_deref();
    let start = after
        .as_deref()
        .and_then(|a| items.iter().position(|s| s.id == a).map(|i| i + 1))
        .unwrap_or(0);
    let rest = items.get(start..).unwrap_or_default();
    let shown: Vec<Value> = rest
        .iter()
        .take(limit)
        .map(|s| session_json(s, current, geo))
        .collect();
    let next = (rest.len() > limit)
        .then(|| {
            rest.get(limit - 1)
                .map(|s| pagination::encode(&call.rt.cursor_key, scope, &s.id))
        })
        .flatten();
    Ok(call.json(StatusCode::OK, &json!({"items": shown, "next_cursor": next})))
}

/// `GET /auth/sessions`, `DELETE /auth/sessions/{id}`, `POST /auth/sessions:revoke_others`.
pub(crate) async fn mine(call: &Call, rest: Option<&str>) -> Result<Response, AdminError> {
    let resource = format!(
        "users/{}/sessions",
        call.principal.user.as_deref().unwrap_or(&call.principal.id)
    );
    match (rest, &call.method) {
        (None, &Method::GET) => {
            call.allow_params(&["limit", "cursor"])?;
            page(call, "sessions", &own(call).await?)
        }
        (None, _) => Err(AdminError::method_not_allowed("GET")),
        (Some(":revoke_others"), &Method::POST) => {
            call.allow_params(&[])?;
            let current = call.session.as_ref().map(|(_, s)| s.digest.clone());
            let digests = own(call)
                .await?
                .into_iter()
                .map(|s| s.digest)
                .filter(|d| Some(d) != current.as_ref())
                .collect();
            let n = call
                .rt
                .end_sessions(digests)
                .await
                .map_err(|_| AdminError::store_unavailable())?;
            call.audit(
                "auth.sessions.revoke_others",
                &resource,
                Outcome::Success,
                StatusCode::OK,
            )
            .await;
            Ok(call.json(StatusCode::OK, &json!({"revoked": n})))
        }
        (Some(id), &Method::DELETE) if !id.starts_with(':') => {
            call.allow_params(&[])?;
            let Some(s) = own(call).await?.into_iter().find(|s| s.id == id) else {
                return Err(AdminError::not_found());
            };
            call.rt
                .end_sessions(vec![s.digest])
                .await
                .map_err(|_| AdminError::store_unavailable())?;
            call.audit(
                "auth.sessions.revoke",
                &format!("{resource}/{id}"),
                Outcome::Success,
                StatusCode::NO_CONTENT,
            )
            .await;
            Ok(call.no_content())
        }
        (Some(id), _) if !id.starts_with(':') => Err(AdminError::method_not_allowed("DELETE")),
        _ => Err(AdminError::not_found()),
    }
}

/// `GET` and `DELETE /users/{name}/sessions` (permission `users:update`).
pub(crate) async fn of_user(call: &Call, user: &str) -> Result<Response, AdminError> {
    let resource = format!("users/{user}/sessions");
    call.require("users:update", &[Scope::Gateway])
        .map_err(|e| e.with_resource(resource.clone()))?;
    if !call.snap.identity.users.contains_key(user) {
        return Err(AdminError::not_found());
    }
    let sessions = call
        .rt
        .sessions_of(Some(user), None)
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    match call.method {
        Method::GET => {
            call.allow_params(&["limit", "cursor"])?;
            page(call, &resource, &sessions)
        }
        Method::DELETE => {
            call.allow_params(&[])?;
            let n = call
                .rt
                .end_sessions(sessions.into_iter().map(|s| s.digest).collect())
                .await
                .map_err(|_| AdminError::store_unavailable())?;
            call.audit(
                "users.sessions.revoke",
                &resource,
                Outcome::Success,
                StatusCode::OK,
            )
            .await;
            Ok(call.json(StatusCode::OK, &json!({"revoked": n})))
        }
        _ => Err(AdminError::method_not_allowed("GET, DELETE")),
    }
}
