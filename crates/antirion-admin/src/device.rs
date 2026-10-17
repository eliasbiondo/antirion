//! Sign-in from the command line through the device authorization grant (RFC 8628): a client
//! starts an authorization and polls for its token, and a signed-in person approves or denies it
//! in the console. Codes are kept only as digests in the store, so any node serves any step.

use std::sync::Arc;
use std::time::Duration;

use antirion_core::audit::{Actor, ActorKind, AuditEntry, AuditKind, Outcome};
use antirion_core::{Peer, RequestId, UtcMicros};
use antirion_identity::device;
use antirion_identity::keyfmt::{Credential, Plane};
use antirion_runtime::Runtime;
use antirion_runtime::operators::DeviceAuthorization;
use antirion_runtime::operators::IdentityError;
use antirion_store::AdminTokenRecord;
use antirion_store::IdentityRecord;
use axum::response::Response;
use bytes::Bytes;
use http::header::{CONTENT_TYPE, HOST};
use http::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::Handled;
use crate::call::Call;
use crate::error::AdminError;
use crate::local::json_response;

const GRANT: &str = "device_grant";
const USER_CODE: &str = "device_user_code";
const GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// How much a `slow_down` adds to the interval (RFC 8628 §3.5).
const SLOW_DOWN: u64 = 5;

/// The public paths of the flow, matched before authentication.
pub(crate) fn is_public(path: &str) -> bool {
    matches!(path, "/admin/v1/auth/device" | "/admin/v1/auth/device/token")
}

/// One authorization, as stored under the digest of its device code.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Grant {
    /// The user code, normalized.
    user_code: String,
    status: Status,
    requested_us: i64,
    expires_us: i64,
    client_id: Option<String>,
    client_ip: Option<String>,
    organization: Option<String>,
    sso: bool,
    interval_s: u64,
    last_poll_us: Option<i64>,
    /// The person who decided it.
    user: Option<String>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Pending,
    Approved,
    Denied,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Approved => "approved",
            Status::Denied => "denied",
        }
    }
}

/// The flow's settings, while it is enabled; its tokens are personal tokens, so it needs them too.
fn settings(rt: &Runtime) -> Option<DeviceAuthorization> {
    let snap = rt.gens.load();
    let c = &snap.identity.console;
    c.device.filter(|_| c.personal_tokens)
}

fn micros(d: Duration) -> i64 {
    i64::try_from(d.as_micros()).unwrap_or(i64::MAX)
}

/// An RFC 6749 §5.2 error answer.
fn oauth_error(code: &'static str, description: &str) -> Response {
    json_response(
        StatusCode::BAD_REQUEST,
        &json!({"error": code, "error_description": description}),
    )
}

/// The members of a JSON object or an `application/x-www-form-urlencoded` body.
fn fields(headers: &http::HeaderMap, body: &[u8]) -> Result<Map<String, Value>, AdminError> {
    let form = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.trim_start().starts_with("application/x-www-form-urlencoded"));
    if form {
        let text =
            std::str::from_utf8(body).map_err(|_| AdminError::invalid_parameter("the form is not UTF-8"))?;
        return Ok(text
            .split('&')
            .filter(|p| !p.is_empty())
            .map(|p| {
                let (k, v) = p.split_once('=').unwrap_or((p, ""));
                (
                    crate::percent_decode(&k.replace('+', " ")),
                    Value::from(crate::percent_decode(&v.replace('+', " "))),
                )
            })
            .collect());
    }
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Map::new());
    }
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(m)) => Ok(m),
        _ => Err(AdminError::invalid_parameter(
            "the body must be a JSON object or a form",
        )),
    }
}

fn text(m: &Map<String, Value>, name: &str, max: usize) -> Option<String> {
    m.get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(max).collect())
}

/// One request to a public step.
struct Public<'a> {
    rt: &'a Arc<Runtime>,
    parts: &'a http::request::Parts,
    body: Bytes,
    request_id: &'a RequestId,
}

impl Public<'_> {
    fn peer(&self) -> Option<String> {
        crate::client_addr::client_addr(self.rt, self.parts)
    }

    /// The console's origin: `auth.admin.public_origin`, else the request's own.
    fn origin(&self) -> String {
        if let Some(o) = self.rt.gens.load().identity.settings.public_origin.clone() {
            return o.trim_end_matches('/').to_owned();
        }
        let tls = self.parts.extensions.get::<Peer>().is_some_and(|p| p.tls);
        let host = self
            .parts
            .headers
            .get(HOST)
            .and_then(|v| v.to_str().ok())
            .or_else(|| self.parts.uri.authority().map(http::uri::Authority::as_str))
            .unwrap_or("localhost");
        format!("{}://{host}", if tls { "https" } else { "http" })
    }

    async fn audit(&self, action: &str, user: Option<&str>, status: StatusCode) {
        audit(self.rt, self.request_id, self.peer(), action, user, status).await;
    }
}

async fn audit(
    rt: &Runtime,
    request_id: &RequestId,
    ip: Option<String>,
    action: &str,
    user: Option<&str>,
    status: StatusCode,
) {
    let generation = rt.gens.load().generation;
    let entry = AuditEntry {
        id: RequestId::generate().as_str().to_owned(),
        time: rt.now().to_rfc3339(),
        actor: Actor {
            kind: ActorKind::Operator,
            id: user.map(str::to_owned),
        },
        action: action.to_owned(),
        resource: user.map_or_else(|| "auth/device".to_owned(), |u| format!("users/{u}")),
        outcome: Outcome::Success,
        status_code: status.as_u16(),
        generation_before: Some(generation),
        generation_after: Some(generation),
        request_id: request_id.as_str().to_owned(),
        source_ip: ip,
        kind: AuditKind::Write,
        reason: None,
        changes: Value::Null,
    };
    if let Err(e) = rt.store.append_audit(entry).await {
        tracing::warn!(error = %e, "an audit entry could not be written");
    }
}

/// `POST /auth/device` and `POST /auth/device/token`.
async fn public(p: &Public<'_>) -> Result<Response, AdminError> {
    if p.parts.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    let Some(s) = settings(p.rt) else {
        return Err(AdminError::not_found());
    };
    let m = fields(&p.parts.headers, &p.body)?;
    if p.parts.uri.path().ends_with("/token") {
        token(p, &s, &m).await
    } else {
        start(p, &s, &m).await
    }
}

async fn start(
    p: &Public<'_>,
    s: &DeviceAuthorization,
    m: &Map<String, Value>,
) -> Result<Response, AdminError> {
    let now = p.rt.now();
    if let Some(ip) = p.peer()
        && let Some(retry_after) = p.rt.state.console.throttle(
            &format!("device:{ip}"),
            now,
            s.max_starts_per_minute,
            Duration::from_secs(60),
        )
    {
        return Err(AdminError::identity(IdentityError::Throttled { retry_after }));
    }
    let device_code = device::device_code().map_err(|_| AdminError::internal())?;
    let store = &p.rt.store;
    let mut user_code = None;
    for _ in 0..5 {
        let code = device::user_code().map_err(|_| AdminError::internal())?;
        let n = device::normalize_user_code(&code).ok_or_else(AdminError::internal)?;
        let taken = store
            .record(USER_CODE, &n)
            .await
            .map_err(|_| AdminError::store_unavailable())?
            .is_some_and(|r| r.expires_us.is_none_or(|e| e > now.0));
        if !taken {
            user_code = Some(n);
            break;
        }
    }
    let user_code = user_code.ok_or_else(AdminError::internal)?;
    let expires_us = now.0.saturating_add(micros(s.code_ttl));
    let grant = Grant {
        user_code: user_code.clone(),
        status: Status::Pending,
        requested_us: now.0,
        expires_us,
        client_id: text(m, "client_id", 100),
        client_ip: p.peer(),
        organization: text(m, "organization", 100),
        sso: matches!(m.get("sso"), Some(Value::Bool(true)))
            || m.get("sso").and_then(Value::as_str) == Some("true"),
        interval_s: s.poll_interval.as_secs().max(1),
        last_poll_us: None,
        user: None,
    };
    let digest = device::digest(&device_code);
    let body = serde_json::to_string(&grant).map_err(|_| AdminError::internal())?;
    store
        .put_record(IdentityRecord {
            kind: GRANT.to_owned(),
            id: digest.clone(),
            user: String::new(),
            expires_us: Some(expires_us),
            body,
        })
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    store
        .put_record(IdentityRecord {
            kind: USER_CODE.to_owned(),
            id: user_code.clone(),
            user: String::new(),
            expires_us: Some(expires_us),
            body: json!({"grant": digest}).to_string(),
        })
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    let shown = device::display_user_code(&user_code);
    let base = format!(
        "{}{}/device",
        p.origin(),
        p.rt.statics.ui.path.trim_end_matches('/')
    );
    let mut complete = format!("{base}?code={shown}");
    if grant.sso {
        complete.push_str("&sso=1");
    }
    Ok(json_response(
        StatusCode::OK,
        &json!({
            "device_code": device_code,
            "user_code": shown,
            "verification_uri": base,
            "verification_uri_complete": complete,
            "expires_in": s.code_ttl.as_secs(),
            "interval": grant.interval_s,
        }),
    ))
}

async fn forget(rt: &Runtime, digest: &str, user_code: &str) {
    let _ = rt.store.delete_record(GRANT, digest).await;
    let _ = rt.store.delete_record(USER_CODE, user_code).await;
}

async fn token(
    p: &Public<'_>,
    s: &DeviceAuthorization,
    m: &Map<String, Value>,
) -> Result<Response, AdminError> {
    if m.get("grant_type")
        .and_then(Value::as_str)
        .is_some_and(|g| g != GRANT_TYPE)
    {
        return Ok(oauth_error(
            "unsupported_grant_type",
            "Only the device code grant is served here.",
        ));
    }
    let Some(code) = text(m, "device_code", 200) else {
        return Ok(oauth_error("invalid_request", "device_code is required."));
    };
    let digest = device::digest(&code);
    let store = &p.rt.store;
    let Some(record) = store
        .record(GRANT, &digest)
        .await
        .map_err(|_| AdminError::store_unavailable())?
    else {
        return Ok(oauth_error("invalid_grant", "The device code is not known."));
    };
    let Ok(mut grant) = serde_json::from_str::<Grant>(&record.body) else {
        return Ok(oauth_error("invalid_grant", "The device code is not known."));
    };
    let now = p.rt.now();
    if now.0 >= grant.expires_us {
        forget(p.rt, &digest, &grant.user_code).await;
        return Ok(oauth_error("expired_token", "The device code has expired."));
    }
    match grant.status {
        Status::Denied => {
            forget(p.rt, &digest, &grant.user_code).await;
            Ok(oauth_error("access_denied", "The sign-in was denied."))
        }
        Status::Pending => {
            let early = grant.last_poll_us.is_some_and(|last| {
                now.0.saturating_sub(last) < micros(Duration::from_secs(grant.interval_s))
            });
            if early {
                grant.interval_s = grant.interval_s.saturating_add(SLOW_DOWN);
            }
            grant.last_poll_us = Some(now.0);
            let next = serde_json::to_string(&grant).map_err(|_| AdminError::internal())?;
            let _ = store.swap_record(GRANT, &digest, &record.body, &next).await;
            Ok(if early {
                oauth_error("slow_down", "Poll less often.")
            } else {
                oauth_error("authorization_pending", "The sign-in has not been approved yet.")
            })
        }
        Status::Approved => issue(p, s, &digest, grant).await,
    }
}

/// The approved grant's token, issued once: the grant is taken from the store first, so two polls
/// can never both get one.
async fn issue(
    p: &Public<'_>,
    s: &DeviceAuthorization,
    digest: &str,
    grant: Grant,
) -> Result<Response, AdminError> {
    let store = &p.rt.store;
    let taken = store
        .take_record(GRANT, digest)
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    let _ = store.delete_record(USER_CODE, &grant.user_code).await;
    let Some(user) = taken.and(grant.user) else {
        return Ok(oauth_error(
            "expired_token",
            "The device code has already been used.",
        ));
    };
    if !p
        .rt
        .gens
        .load()
        .identity
        .users
        .get(&user)
        .is_some_and(|u| u.enabled && u.human)
    {
        return Ok(oauth_error(
            "access_denied",
            "The approving user can no longer sign in.",
        ));
    }
    let now = p.rt.now();
    let cred = Credential::generate(Plane::Admin).map_err(|_| AdminError::internal())?;
    let client = grant.client_id.as_deref().unwrap_or("command line");
    let record = AdminTokenRecord {
        kid: cred.kid().to_owned(),
        user: user.clone(),
        verifier: p.rt.pepper.verifier(cred.expose()),
        description: Some(format!("Device sign-in ({client})")),
        created_at: now.to_rfc3339_seconds(),
        expires_at: Some(UtcMicros(now.0.saturating_add(micros(s.token_ttl))).to_rfc3339_seconds()),
        access: None,
        last_used_us: None,
    };
    store
        .put_admin_token(record.clone())
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    p.audit("auth.device.token", Some(&user), StatusCode::OK).await;
    Ok(json_response(
        StatusCode::OK,
        &json!({
            "access_token": cred.expose(),
            "token_type": "Bearer",
            "expires_in": s.token_ttl.as_secs(),
            "kid": record.kid,
            "user": user,
        }),
    ))
}

/// [`decide`], built and boxed in a frame of its own so the router's frame stays the size it was.
pub(crate) fn decide_boxed<'a>(call: &'a Call, action: &'a str) -> Handled<'a> {
    Box::pin(decide(call, action))
}

/// [`public`] after reading the body, built and boxed in a frame of its own so the router's frame
/// stays the size it was.
pub(crate) fn public_boxed<'a>(
    rt: &'a Arc<Runtime>,
    parts: &'a http::request::Parts,
    body: axum::body::Body,
    limit: usize,
    request_id: &'a RequestId,
) -> Handled<'a> {
    Box::pin(async move {
        let body = axum::body::to_bytes(body, limit).await.map_err(|_| {
            AdminError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "The body is too large.",
            )
        })?;
        public(&Public {
            rt,
            parts,
            body,
            request_id,
        })
        .await
    })
}

/// `POST /auth/device:check`, `:approve` and `:deny`, by a signed-in person.
async fn decide(call: &Call, action: &str) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&[])?;
    if settings(&call.rt).is_none() {
        return Err(AdminError::not_found());
    }
    let user = crate::mfa::own_user(call)?;
    let ip = call.source_ip.clone();
    call.rt
        .throttle_client(ip.as_deref())
        .map_err(AdminError::identity)?;
    let m = call.json_body()?;
    let typed = m.get("user_code").and_then(Value::as_str).unwrap_or_default();
    let Some((digest, record, grant)) = find(call, typed).await? else {
        call.rt.count_failure(ip.as_deref());
        return Err(AdminError::not_found());
    };
    let shown = |g: &Grant| {
        json!({
            "user_code": device::display_user_code(&g.user_code),
            "status": g.status.as_str(),
            "requested_at": UtcMicros(g.requested_us).to_rfc3339_seconds(),
            "expires_at": UtcMicros(g.expires_us).to_rfc3339_seconds(),
            "client_id": g.client_id,
            "client_ip": g.client_ip,
            "organization": g.organization,
            "sso": g.sso,
        })
    };
    let status = match action {
        "check" => return Ok(call.json(StatusCode::OK, &shown(&grant))),
        "approve" => Status::Approved,
        "deny" => Status::Denied,
        _ => return Err(AdminError::not_found()),
    };
    if grant.status != Status::Pending {
        return Err(AdminError::conflict(
            "conflict",
            "This sign-in has already been decided.",
            Vec::new(),
        ));
    }
    let decided = Grant {
        status,
        user: Some(user.clone()),
        ..grant
    };
    let next = serde_json::to_string(&decided).map_err(|_| AdminError::internal())?;
    let swapped = call
        .rt
        .store
        .swap_record(GRANT, &digest, &record, &next)
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    if !swapped {
        return Err(AdminError::conflict(
            "conflict",
            "This sign-in has already been decided.",
            Vec::new(),
        ));
    }
    let what = if status == Status::Approved {
        "auth.device.approve"
    } else {
        "auth.device.deny"
    };
    audit(&call.rt, &call.request_id, ip, what, Some(&user), StatusCode::OK).await;
    Ok(call.json(StatusCode::OK, &shown(&decided)))
}

/// The pending or decided grant of a typed user code, unless it is unknown or expired.
async fn find(call: &Call, typed: &str) -> Result<Option<(String, String, Grant)>, AdminError> {
    let Some(code) = device::normalize_user_code(typed) else {
        return Ok(None);
    };
    let store = &call.rt.store;
    let now = call.rt.now();
    let Some(index) = store
        .record(USER_CODE, &code)
        .await
        .map_err(|_| AdminError::store_unavailable())?
    else {
        return Ok(None);
    };
    let Some(digest) = serde_json::from_str::<Value>(&index.body)
        .ok()
        .and_then(|v| v.get("grant").and_then(Value::as_str).map(str::to_owned))
    else {
        return Ok(None);
    };
    let Some(record) = store
        .record(GRANT, &digest)
        .await
        .map_err(|_| AdminError::store_unavailable())?
    else {
        return Ok(None);
    };
    let Ok(grant) = serde_json::from_str::<Grant>(&record.body) else {
        return Ok(None);
    };
    if now.0 >= grant.expires_us || grant.user_code != code {
        return Ok(None);
    }
    Ok(Some((digest, record.body, grant)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms_and_json_bodies() {
        let mut h = http::HeaderMap::new();
        h.insert(
            CONTENT_TYPE,
            http::HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        let m = fields(&h, b"device_code=a%2Bb&grant_type=urn%3Aietf").unwrap();
        assert_eq!(m["device_code"], "a+b");
        assert_eq!(m["grant_type"], "urn:ietf");
        let j = fields(&http::HeaderMap::new(), br#"{"client_id": "cli"}"#).unwrap();
        assert_eq!(text(&j, "client_id", 100).as_deref(), Some("cli"));
        assert!(fields(&http::HeaderMap::new(), b"").unwrap().is_empty());
        assert!(fields(&http::HeaderMap::new(), b"[1]").is_err());
    }

    #[test]
    fn grants_round_trip_through_their_record() {
        let g = Grant {
            user_code: "BCDFGHJK".into(),
            status: Status::Pending,
            requested_us: 1,
            expires_us: 2,
            client_id: None,
            client_ip: Some("10.0.0.1".into()),
            organization: None,
            sso: true,
            interval_s: 5,
            last_poll_us: None,
            user: None,
        };
        let back: Grant = serde_json::from_str(&serde_json::to_string(&g).unwrap()).unwrap();
        assert_eq!(back.status, Status::Pending);
        assert!(back.sso);
    }
}
