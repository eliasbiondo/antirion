//! Which future answers a request on the admin listener: the probes, the public sign-in steps, the
//! console's files, or an authenticated Admin API call.
//!
//! The choice is made by a synchronous function that builds and boxes only the future it picks, so
//! the frame polled for every request stays small in an unoptimised build, which lays out each
//! future a function builds in a stack slot of its own.

use std::sync::Arc;

use antirion_core::RequestId;
use antirion_runtime::Runtime;
use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::response::Response;
use http::header::{ACCESS_CONTROL_REQUEST_METHOD, ORIGIN};
use http::request::Parts;
use http::{Method, StatusCode};

use crate::call::{self, Call, anonymous};
use crate::error::AdminError;
use crate::routes::{self, Handled, done};
use crate::{
    Origin, admin_mcp, audit_denied, bearer, clean_path, client_addr, console, device, idempotency, local,
    oauth, oidc, ops, percent_decode, preflight, probes, query_pairs, saml, scim, session,
};

pub(crate) async fn route(
    rt: &Arc<Runtime>,
    req: Request,
    request_id: &RequestId,
) -> Result<Response, AdminError> {
    lane(rt, req, request_id)?.await
}

fn answered<'a>(result: Result<Response, AdminError>) -> Result<Handled<'a>, AdminError> {
    result.map(|r| done(Ok(r)))
}

async fn read(body: Body, limit: usize, what: &'static str) -> Result<Bytes, AdminError> {
    axum::body::to_bytes(body, limit)
        .await
        .map_err(|_| AdminError::new(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large", what))
}

const BODY_TOO_LARGE: &str = "The body is too large.";

/// `(provider, step)` of `/admin/v1/auth/<kind>/{provider}/{step}` when `step` is one of `steps`.
fn sign_in_step(rest: &str, steps: &[&str]) -> Option<(String, String)> {
    rest.split_once('/')
        .filter(|(_, step)| steps.contains(step))
        .map(|(name, step)| (name.to_owned(), step.to_owned()))
}

fn lane<'a>(
    rt: &'a Arc<Runtime>,
    req: Request,
    request_id: &'a RequestId,
) -> Result<Handled<'a>, AdminError> {
    let (parts, body) = req.into_parts();
    let peer = client_addr::client_addr(rt, &parts);
    let path = clean_path(parts.uri.path()).ok_or_else(|| {
        AdminError::new(
            StatusCode::BAD_REQUEST,
            "invalid_path",
            "The request path is not allowed.",
        )
    })?;
    match path.as_str() {
        "/-/livez" => return answered(probes::probe(rt, &parts.method, true)),
        "/-/readyz" => return answered(probes::probe(rt, &parts.method, false)),
        _ => {}
    }
    if let Some(origin) = parts.headers.get(ORIGIN).and_then(|v| v.to_str().ok())
        && parts.method == Method::OPTIONS
        && parts.headers.contains_key(ACCESS_CONTROL_REQUEST_METHOD)
    {
        return answered(preflight(rt, &parts, origin));
    }
    let limit = usize::try_from(rt.statics.max_document).unwrap_or(usize::MAX);
    if scim::is_scim(&path) {
        let bearer = bearer(&parts).map(str::to_owned);
        let query = query_pairs(parts.uri.query());
        return Ok(Box::pin(async move {
            let body = read(body, limit, BODY_TOO_LARGE).await?;
            Ok(scim::handle(rt, parts.method.clone(), &path, bearer.as_deref(), query, body).await)
        }));
    }
    let snap = rt.gens.load();
    let metrics = &snap.obs.metrics;
    let rest = if metrics.enabled && path == metrics.path {
        if !metrics.auth_required {
            if parts.method != Method::GET {
                return Err(AdminError::method_not_allowed("GET"));
            }
            let accept = parts
                .headers
                .get(http::header::ACCEPT)
                .and_then(|v| v.to_str().ok());
            return answered(Ok(ops::exposition(rt, accept)));
        }
        None
    } else if path == admin_mcp::PATH && admin_mcp::enabled(&snap) {
        Some(String::new())
    } else {
        match path.strip_prefix("/admin/v1/") {
            Some(r) => Some(r.to_owned()),
            None => return answered(console(rt, &parts.method, &path)),
        }
    };
    drop(snap);
    let params = query_pairs(parts.uri.query());
    let testing = params.iter().any(|(k, v)| k == "test" && v == "true");
    if let Some(rest) = path.strip_prefix("/admin/v1/auth/oidc/")
        && !(testing && rest.ends_with("/login"))
    {
        let (name, step) = sign_in_step(rest, &["login", "callback"]).ok_or_else(AdminError::not_found)?;
        if parts.method != Method::GET {
            return Err(AdminError::method_not_allowed("GET"));
        }
        return Ok(Box::pin(async move {
            if step == "login" {
                oidc::login(rt, &name, &params, &parts.headers, false).await
            } else {
                oidc::callback(rt, &name, &params, &parts.headers)
                    .await
                    .map(|mut resp| {
                        oidc::unbind(&mut resp);
                        resp
                    })
            }
        }));
    }
    if let Some(rest) = path.strip_prefix("/admin/v1/auth/saml/")
        && let Some((name, step)) = sign_in_step(rest, &["login", "acs", "metadata"])
        && !(testing && step == "login")
    {
        return Ok(Box::pin(async move {
            let body = read(body, limit, BODY_TOO_LARGE).await?;
            saml::public(rt, &name, &step, &parts.method, &params, &parts.headers, &body).await
        }));
    }
    if let Some(rest) = path.strip_prefix("/admin/v1/auth/oauth/") {
        let (name, step) = sign_in_step(rest, &["login", "callback"]).ok_or_else(AdminError::not_found)?;
        if parts.method != Method::GET {
            return Err(AdminError::method_not_allowed("GET"));
        }
        return Ok(Box::pin(async move {
            if step == "login" {
                oauth::login(rt, &name, &params, &parts.headers).await
            } else {
                oauth::callback(rt, &name, &params, &parts.headers)
                    .await
                    .map(|mut resp| {
                        oidc::unbind(&mut resp);
                        resp
                    })
            }
        }));
    }
    if path == "/admin/v1/auth/session" {
        let client = antirion_runtime::admin_auth::Client {
            ip: peer,
            user_agent: parts
                .headers
                .get(http::header::USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.chars().take(512).collect()),
            organization: None,
        };
        return Ok(Box::pin(async move {
            let body = read(body, limit, BODY_TOO_LARGE).await?;
            session::start(rt, &parts.method, &body, client).await
        }));
    }
    if device::is_public(&path) {
        return Ok(Box::pin(async move {
            device::public_boxed(rt, &parts, body, limit, request_id).await
        }));
    }
    let reason = if matches!(
        parts.method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    ) {
        call::change_reason(&parts.headers)?
    } else {
        None
    };
    let anonymous_call = bearer(&parts).is_none() && session::cookie(&parts.headers).is_none();
    if local::is_public(&path) || (local::is_enrolment(&path) && anonymous_call) {
        return Ok(Box::pin(async move {
            let body = read(body, limit, BODY_TOO_LARGE).await?;
            let public = local::Public::new(rt, &parts, params, body, request_id);
            if local::is_public(&path) {
                local::handle(&public, &path).await
            } else {
                local::enrol(&public, &path).await
            }
        }));
    }
    let request = Authenticated {
        parts,
        body,
        path,
        rest,
        reason,
        peer,
        limit,
    };
    Ok(Box::pin(authenticated(rt, request, request_id)))
}

/// A request that needs a credential, after the public paths were ruled out.
struct Authenticated {
    parts: Parts,
    body: Body,
    path: String,
    /// The path below `/admin/v1/`; `None` for a metrics scrape.
    rest: Option<String>,
    reason: Option<String>,
    peer: Option<String>,
    limit: usize,
}

async fn authenticated(
    rt: &Arc<Runtime>,
    request: Authenticated,
    request_id: &RequestId,
) -> Result<Response, AdminError> {
    let Authenticated {
        parts,
        body,
        path,
        rest,
        reason,
        peer,
        limit,
    } = request;
    let scrape = rest.is_none();
    let token = bearer(&parts).map(str::to_owned);
    let mut session = None;
    if token.is_some() {
        rt.throttle_bearer(peer.as_deref())
            .map_err(AdminError::identity)?;
    }
    let principal = match (token, session::cookie(&parts.headers)) {
        (Some(t), _) => {
            let found = rt.authenticate_admin(&t).await;
            if found.is_none() {
                rt.count_bearer_failure(peer.as_deref());
            }
            found.ok_or(Some(t))
        }
        (None, Some(id)) => match rt.session(&id).await {
            Ok(Some(s)) => {
                let p = s.principal.clone();
                session = Some((id, s));
                Ok(p)
            }
            Ok(None) => Err(None),
            Err(_) => return Err(AdminError::store_unavailable()),
        },
        (None, None) => Err(None),
    };
    let principal = match principal {
        Ok(p) => p,
        Err(token) => {
            let data_key = token.as_deref().is_some_and(|t| t.starts_with("sk-antirion-"));
            let message = if data_key {
                "Virtual keys are data-plane credentials; they are not valid on the admin plane."
            } else if scrape {
                "A scrape needs an admin token with metrics:read: create a token for a service account \
                 holding metrics:read and send it as a bearer token, or set observability.metrics.auth: none."
            } else {
                "A valid admin credential is required."
            };
            // The console asks who is signed in on every page load; that question without a bearer
            // token (no session, or one that ended) is not a refused call worth an audit entry.
            let session_probe = token.is_none() && parts.method == Method::GET && path == "/admin/v1/auth/me";
            if !session_probe {
                audit_denied(
                    rt,
                    anonymous(),
                    &parts.method,
                    &path,
                    StatusCode::UNAUTHORIZED,
                    Origin {
                        request_id,
                        ip: peer,
                        reason,
                    },
                )
                .await;
            }
            return Err(AdminError::unauthenticated(message));
        }
    };
    let unsafe_method = matches!(
        parts.method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    );
    if session.is_some()
        && unsafe_method
        && !session::origin_allowed(
            rt.gens.load().identity.settings.public_origin.as_deref(),
            &parts.headers,
        )
    {
        return Err(AdminError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "A cookie-authenticated write must come from the admin plane's origin.",
        ));
    }
    let body = read(body, limit, "The document is too large.").await?;
    let call = Call {
        rt: Arc::clone(rt),
        snap: rt.gens.load(),
        principal,
        session,
        request_id: request_id.clone(),
        source_ip: peer.clone(),
        method: parts.method.clone(),
        query: query_pairs(parts.uri.query()),
        headers: parts.headers,
        body,
        reason,
        path: path.clone(),
        raw_query: parts.uri.query().map(str::to_owned),
        approved: false,
    };
    let result = match rest {
        None => ops::metrics(&call),
        Some(_) if path == admin_mcp::PATH => admin_mcp::serve_boxed(&call).await,
        // Collapsing repeated slashes is the admin plane's only path normalisation: a trailing slash
        // names no operation, whatever the method, query or body.
        Some(rest) if rest.ends_with('/') => Err(AdminError::not_found()),
        Some(rest) => {
            let segments: Vec<String> = rest.split('/').map(percent_decode).collect();
            let segs: Vec<&str> = segments.iter().map(String::as_str).collect();
            match idempotency::key(&call) {
                Ok(Some(key)) => {
                    Box::pin(idempotency::run(&call, &path, &key, || {
                        routes::handle(&call, &segs)
                    }))
                    .await
                }
                Ok(None) => routes::handle(&call, &segs).await,
                Err(e) => Err(e),
            }
        }
    };
    if let Err(e) = &result
        && e.status == StatusCode::FORBIDDEN
    {
        let resource = e.resource.clone().unwrap_or_else(|| path.clone());
        audit_denied(
            rt,
            call.actor(),
            &call.method,
            &resource,
            e.status,
            Origin {
                request_id,
                ip: peer,
                reason: call.reason.clone(),
            },
        )
        .await;
    }
    result
}
