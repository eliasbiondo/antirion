//! The admin plane: the Admin API under `/admin/v1`, the probes and the metrics exposition, served on
//! the admin listener only. Every call except the probes is authenticated; denials are audited.

#![forbid(unsafe_code)]

mod access_requests;
mod access_routes;
mod admin_mcp;
mod alerts;
mod analytics;
mod anomalies;
mod applylock;
mod budget_events;
mod budget_routes;
mod budgets;
mod cache;
mod cache_entries;
mod cache_routes;
mod cache_share;
mod call;
mod catalog;
mod catalog_read;
mod certinfo;
mod change_control;
mod change_needs;
mod change_requests;
mod change_secrets;
mod client_addr;
mod commit;
mod danger_routes;
mod delivery;
mod device;
mod document;
mod encryption;
mod error;
mod export;
mod generations;
mod guardrail_stats;
mod guardtest;
mod health;
mod hints;
mod idempotency;
mod ingress;
mod invitations;
mod jobs;
mod keys;
mod limit_counters;
mod limitsim;
mod local;
mod logfeedback;
mod logquery;
mod logs;
mod mcp;
mod me;
mod mfa;
mod models;
mod nodes;
mod oauth;
mod offboard;
mod oidc;
mod ops;
mod org_deletion;
mod overage;
mod pagination;
mod people;
mod perms;
mod playground;
mod pressure;
mod pricing;
mod probes;
mod provider_ops;
mod push;
mod recost;
mod regions;
mod resources;
mod role_bindings;
mod rotate;
mod route;
mod routes;
mod runtime;
mod safety;
mod safety_routes;
mod safety_summary;
mod saml;
mod scim;
mod session;
mod sessions;
mod settings;
mod signin_facts;
mod simulate;
mod spend;
mod tail;
mod tenancy;
mod testmode;
mod token_checks;
mod users;
mod users_cap;
mod views;
mod zipfile;

use std::sync::Arc;

use antirion_core::audit::{AuditEntry, Outcome};
use antirion_core::{Peer, RequestId, headers as h};
use antirion_runtime::Runtime;
use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::response::Response;
use http::header::{
    ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
    ACCESS_CONTROL_MAX_AGE, ACCESS_CONTROL_REQUEST_HEADERS, AUTHORIZATION, CACHE_CONTROL, ORIGIN, VARY,
};
use http::{HeaderName, HeaderValue, Method, StatusCode};

use crate::error::AdminError;

pub use crate::probes::waiting;
pub(crate) use crate::routes::Handled;

/// The admin plane's router.
pub fn router(rt: Arc<Runtime>) -> Router {
    Router::new().fallback(dispatch).with_state(rt)
}

async fn dispatch(State(rt): State<Arc<Runtime>>, req: Request) -> Response {
    let request_id = RequestId::from_client(req.headers().get(h::X_REQUEST_ID).map(|v| v.as_bytes()));
    let origin = req
        .headers()
        .get(ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let secure = req.extensions().get::<Peer>().is_some_and(|p| p.tls);
    let mut resp = match route::route(&rt, req, &request_id).await {
        Ok(r) => r,
        Err(e) => e.into_response(&request_id),
    };
    finish(&rt, &mut resp, &request_id, origin.as_deref(), secure);
    resp
}

/// `Strict-Transport-Security` on a TLS listener when `security.hsts` is enabled.
fn hsts(secure: bool, configured: Option<&str>) -> Option<HeaderValue> {
    configured
        .filter(|_| secure)
        .and_then(|v| HeaderValue::from_str(v).ok())
}

/// Adds the headers every admin response carries; `secure` when the listener terminated TLS, the
/// only case `Strict-Transport-Security` is sent in.
fn finish(rt: &Runtime, resp: &mut Response, request_id: &RequestId, origin: Option<&str>, secure: bool) {
    let headers = resp.headers_mut();
    if let Some(v) = hsts(secure, rt.gens.load().security.hsts.as_deref()) {
        headers.insert(http::header::STRICT_TRANSPORT_SECURITY, v);
    }
    if let Ok(v) = HeaderValue::from_str(request_id.as_str()) {
        headers.insert(HeaderName::from_static(h::X_REQUEST_ID), v);
    }
    headers
        .entry(CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store"));
    headers.insert(
        HeaderName::from_static(h::X_CONTENT_TYPE_OPTIONS),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        HeaderName::from_static(h::VERSION),
        HeaderValue::from_static(h::BUILD_VERSION),
    );
    headers.insert(
        HeaderName::from_static(h::X_FRAME_OPTIONS),
        HeaderValue::from_static("DENY"),
    );
    headers.insert(
        HeaderName::from_static(h::REFERRER_POLICY),
        HeaderValue::from_static("no-referrer"),
    );
    headers
        .entry(HeaderName::from_static(h::CONTENT_SECURITY_POLICY))
        .or_insert(HeaderValue::from_static("frame-ancestors 'none'"));
    if let Some(origin) = origin {
        headers.append(VARY, HeaderValue::from_static("Origin"));
        let snap = rt.gens.load();
        if let Some(cors) = snap.security.cors_admin.as_ref().filter(|c| c.allows(origin))
            && let Ok(v) = HeaderValue::from_str(cors.allow_origin_value(origin))
        {
            headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, v);
        }
    }
}

pub(crate) fn preflight(
    rt: &Runtime,
    parts: &http::request::Parts,
    origin: &str,
) -> Result<Response, AdminError> {
    let snap = rt.gens.load();
    let Some(cors) = snap.security.cors_admin.as_ref().filter(|c| c.allows(origin)) else {
        return Err(AdminError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "The origin is not allowed.",
        ));
    };
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::NO_CONTENT;
    let h = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(cors.allow_origin_value(origin)) {
        h.insert(ACCESS_CONTROL_ALLOW_ORIGIN, v);
    }
    if let Ok(v) = HeaderValue::from_str(&cors.methods) {
        h.insert(ACCESS_CONTROL_ALLOW_METHODS, v);
    }
    if let Some(v) = parts.headers.get(ACCESS_CONTROL_REQUEST_HEADERS) {
        h.insert(ACCESS_CONTROL_ALLOW_HEADERS, v.clone());
    }
    h.insert(ACCESS_CONTROL_MAX_AGE, HeaderValue::from(cors.max_age_secs));
    Ok(resp)
}

pub(crate) fn bearer(parts: &http::request::Parts) -> Option<&str> {
    let value = parts.headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
}

/// Where a refused call came from, for its audit entry.
pub(crate) struct Origin<'a> {
    pub(crate) request_id: &'a RequestId,
    pub(crate) ip: Option<String>,
    pub(crate) reason: Option<String>,
}

pub(crate) async fn audit_denied(
    rt: &Runtime,
    call_actor: antirion_core::audit::Actor,
    method: &Method,
    resource: &str,
    status: StatusCode,
    origin: Origin<'_>,
) {
    let generation = rt.gens.load().generation;
    let entry = AuditEntry {
        id: RequestId::generate().as_str().to_owned(),
        time: rt.now().to_rfc3339(),
        actor: call_actor,
        action: format!("{} {resource}", method.as_str()),
        resource: resource.to_owned(),
        outcome: Outcome::Denied,
        status_code: status.as_u16(),
        generation_before: Some(generation),
        generation_after: Some(generation),
        request_id: origin.request_id.as_str().to_owned(),
        source_ip: origin.ip,
        kind: antirion_core::audit::AuditKind::Write,
        reason: origin.reason,
        changes: serde_json::Value::Null,
    };
    if let Err(e) = rt.store.append_audit(entry).await {
        tracing::warn!(error = %e, "an audit entry could not be written");
    }
}

pub(crate) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| char::from(b).to_digit(16);
        match (
            bytes.get(i),
            bytes.get(i + 1).copied().and_then(hex),
            bytes.get(i + 2).copied().and_then(hex),
        ) {
            (Some(b'%'), Some(hi), Some(lo)) => {
                out.push(u8::try_from(hi * 16 + lo).unwrap_or(b'?'));
                i += 3;
            }
            (Some(b), _, _) => {
                out.push(*b);
                i += 1;
            }
            (None, _, _) => break,
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The path with repeated slashes collapsed; `None` for a dot segment (literal or encoded), a
/// backslash, an encoded slash or an encoded control character.
pub(crate) fn clean_path(raw: &str) -> Option<String> {
    let lower = raw.to_ascii_lowercase();
    if raw.contains('\\')
        || lower.contains("%2f")
        || lower.contains("%5c")
        || raw.bytes().any(|b| b.is_ascii_control())
    {
        return None;
    }
    let segments: Vec<&str> = raw.split('/').filter(|s| !s.is_empty()).collect();
    let bad = segments.iter().any(|s| {
        let d = s.to_ascii_lowercase().replace("%2e", ".");
        d == "." || d == ".." || d.contains("%0") || d.contains("%1") || d.contains("%7f")
    });
    if bad {
        return None;
    }
    let mut out = format!("/{}", segments.join("/"));
    if raw.ends_with('/') && out.len() > 1 {
        out.push('/');
    }
    Some(out)
}

pub(crate) fn query_pairs(query: Option<&str>) -> Vec<(String, String)> {
    query
        .into_iter()
        .flat_map(|q| q.split('&'))
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (
                percent_decode(&k.replace('+', " ")),
                percent_decode(&v.replace('+', " ")),
            )
        })
        .collect()
}

/// The console's static assets, served without credentials: they hold no data.
pub(crate) fn console(rt: &Runtime, method: &Method, path: &str) -> Result<Response, AdminError> {
    let rel = rt.statics.ui.relative(path).ok_or_else(AdminError::not_found)?;
    Ok(antirion_console::serve(method, &percent_decode(rel)).map(Body::from))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsts_only_on_tls_when_enabled() {
        let on = Some("max-age=86400");
        assert_eq!(hsts(true, on).unwrap(), "max-age=86400");
        assert!(hsts(false, on).is_none());
        assert!(hsts(true, None).is_none());
    }

    #[test]
    fn decoding() {
        assert_eq!(percent_decode("a%2Fb"), "a/b");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(
            query_pairs(Some("limit=10&cursor=a%3D&x")),
            vec![
                ("limit".to_owned(), "10".to_owned()),
                ("cursor".to_owned(), "a=".to_owned()),
                ("x".to_owned(), String::new())
            ]
        );
    }
}
