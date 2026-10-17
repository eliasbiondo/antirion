//! Admin sessions: starting one with an admin credential, ending it, and `GET /auth/me`. A session
//! is an opaque id in the `antirion_session` cookie, stored server-side by its digest.

use std::collections::BTreeMap;

use antirion_identity::rbac::{AdminPrincipal, CATALOGUE};
use antirion_runtime::admin_auth::{BOOTSTRAP, Client, Session, SessionError};
use antirion_runtime::{Runtime, Snapshot};
use axum::body::Body;
use axum::response::Response;
use http::header::{CONTENT_TYPE, COOKIE, SET_COOKIE};
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;
use crate::role_bindings::binding_id;

pub(crate) const COOKIE_NAME: &str = "antirion_session";

/// The session id carried in the request's cookies.
pub(crate) fn cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (name == COOKIE_NAME && !value.is_empty()).then(|| value.to_owned())
        })
}

/// `GET /auth/me`'s document for a principal.
pub(crate) async fn me(rt: &Runtime, principal: &AdminPrincipal, session: Option<&Session>) -> Value {
    let snap = rt.gens.load();
    me_with(rt, &snap, principal, session).await
}

/// The signed-in user's own fields, with the second factors it has.
async fn user_json(rt: &Runtime, snap: &Snapshot, name: &str) -> Value {
    let Some(u) = snap.identity.users.get(name) else {
        return Value::Null;
    };
    let methods: Vec<&str> = match rt.factors(name).await {
        Ok((factors, _)) => {
            let mut m: Vec<&str> = factors.iter().map(|f| f.kind.as_str()).collect();
            m.sort_unstable();
            m.dedup();
            m
        }
        Err(_) => Vec::new(),
    };
    json!({
        "name": name,
        "display_name": u.display_name,
        "email": u.email,
        "title": u.title,
        "primary_team": u.primary_team,
        "mfa_methods": methods,
    })
}

pub(crate) async fn me_with(
    rt: &Runtime,
    snap: &Snapshot,
    principal: &AdminPrincipal,
    session: Option<&Session>,
) -> Value {
    let user = match &principal.user {
        Some(u) if principal.id != BOOTSTRAP => user_json(rt, snap, u).await,
        _ => Value::Null,
    };
    let mut doc = me_doc(snap, principal, session);
    if let Some(o) = doc.as_object_mut() {
        o.insert("user".into(), user);
    }
    doc
}

fn me_doc(snap: &Snapshot, principal: &AdminPrincipal, session: Option<&Session>) -> Value {
    let (kind, id, human) = match &principal.user {
        _ if principal.id == BOOTSTRAP => ("bootstrap", BOOTSTRAP.to_owned(), None),
        Some(user) => (
            "user",
            user.clone(),
            snap.identity.users.get(user).map(|u| u.human),
        ),
        None => ("token", principal.id.clone(), None),
    };
    let mut p = json!({
        "type": kind,
        "id": id,
        "authenticated_by": if session.is_some() { "session" } else { "bearer" },
    });
    if let (Some(human), Some(obj)) = (human, p.as_object_mut()) {
        obj.insert(
            "kind".into(),
            Value::from(if human { "human" } else { "service" }),
        );
    }
    let bindings: Vec<Value> = principal
        .bindings
        .iter()
        .map(|b| {
            let scope = b.scope.to_string();
            let holder = if kind == "token" { "token" } else { "user" };
            json!({
                "id": binding_id(holder, &id, b.role.as_str(), &scope),
                "principal": {"type": holder, "id": id},
                "role": b.role.as_str(),
                "scope": scope,
            })
        })
        .collect();
    let mut permissions: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for b in &principal.bindings {
        let entry = permissions.entry(b.scope.to_string()).or_default();
        entry.extend(CATALOGUE.iter().copied().filter(|p| {
            b.role.allows(p)
                && (!principal.read_only || p.split_once(':').is_some_and(|(_, a)| a.starts_with("read")))
        }));
        entry.sort_unstable();
        entry.dedup();
    }
    let session = session.map(|s| {
        json!({"idle_expires_at": s.idle_expires_at.to_rfc3339_seconds(), "expires_at": s.expires_at.to_rfc3339_seconds()})
    });
    json!({"principal": p, "bindings": bindings, "permissions": permissions, "session": session})
}

fn json_response(status: StatusCode, body: &Value) -> Response {
    let mut resp = Response::new(Body::from(serde_json::to_vec(body).unwrap_or_default()));
    *resp.status_mut() = status;
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    resp
}

/// Sets the session cookie on a response that starts a session.
pub(crate) fn set_cookie(resp: &mut Response, id: &str, max_age: u64) {
    let cookie = format!("{COOKIE_NAME}={id}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age={max_age}");
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        resp.headers_mut().insert(SET_COOKIE, v);
    }
}

/// `POST /admin/v1/auth/session` (unauthenticated): `{"token": "<admin credential>"}`.
pub(crate) async fn start(
    rt: &Runtime,
    method: &Method,
    body: &[u8],
    client: Client,
) -> Result<Response, AdminError> {
    if method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    let parsed: Value = serde_json::from_slice(body).map_err(|e| {
        AdminError::malformed(vec![antirion_config::ConfigError::new(
            &antirion_config::DocPath::root(),
            antirion_config::DetailCode::MalformedDocument,
            e.to_string(),
        )])
    })?;
    let token = parsed
        .get("token")
        .and_then(Value::as_str)
        .ok_or_else(|| AdminError::invalid_parameter("token is required"))?;
    let ip = client.ip.clone();
    rt.throttle_bearer(ip.as_deref()).map_err(AdminError::identity)?;
    let (id, session) = match rt.start_session(token, client).await {
        Ok(started) => started,
        Err(SessionError::InvalidCredential) => {
            rt.count_bearer_failure(ip.as_deref());
            return Err(AdminError::unauthenticated(
                "A valid admin credential is required.",
            ));
        }
        Err(SessionError::Store(_)) => return Err(AdminError::store_unavailable()),
        Err(SessionError::Random) => return Err(AdminError::internal()),
    };
    let max_age = rt.session_max_age(&session.facts).as_secs();
    let mut resp = json_response(StatusCode::OK, &me(rt, &session.principal, Some(&session)).await);
    set_cookie(&mut resp, &id, max_age);
    Ok(resp)
}

/// `POST /admin/v1/auth/logout`: deletes the session server-side and clears the cookie.
pub(crate) async fn logout(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    if let Some((id, _)) = &call.session {
        call.rt
            .end_session(id)
            .await
            .map_err(|_| AdminError::store_unavailable())?;
    }
    let mut resp = call.no_content();
    resp.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_static("antirion_session=; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=0"),
    );
    Ok(resp)
}

/// `GET` and `PATCH /admin/v1/auth/me`.
pub(crate) async fn whoami(call: &Call) -> Result<Response, AdminError> {
    match call.method {
        Method::GET => {
            call.allow_params(&[])?;
            Ok(json_response(
                StatusCode::OK,
                &me_with(
                    &call.rt,
                    &call.snap,
                    &call.principal,
                    call.session.as_ref().map(|(_, s)| s),
                )
                .await,
            ))
        }
        Method::PATCH => crate::me::patch(call).await,
        _ => Err(AdminError::method_not_allowed("GET, PATCH")),
    }
}

/// Whether a cookie-authenticated write comes from the admin plane's public origin: the configured
/// one, or the origin the client addressed.
pub(crate) fn origin_allowed(public_origin: Option<&str>, headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(http::header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    match public_origin {
        Some(expected) => origin == expected.trim_end_matches('/'),
        None => headers
            .get(http::header::HOST)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|host| origin == format!("http://{host}") || origin == format!("https://{host}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_and_origin() {
        let mut h = HeaderMap::new();
        h.insert(COOKIE, HeaderValue::from_static("a=b; antirion_session=xyz"));
        assert_eq!(cookie(&h).as_deref(), Some("xyz"));
        h.insert(http::header::HOST, HeaderValue::from_static("127.0.0.1:18081"));
        assert!(!origin_allowed(None, &h));
        h.insert(
            http::header::ORIGIN,
            HeaderValue::from_static("http://127.0.0.1:18081"),
        );
        assert!(origin_allowed(None, &h));
        assert!(!origin_allowed(Some("https://gw.example"), &h));
        h.insert(
            http::header::ORIGIN,
            HeaderValue::from_static("https://evil.example"),
        );
        assert!(!origin_allowed(None, &h));
    }
}
