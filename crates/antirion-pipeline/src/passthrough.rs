//! Explicit passthrough mounts (`api.passthrough`): a request under a mount's prefix, authenticated,
//! authorized for the mount's provider and counted against the request limits, is forwarded to the
//! provider with its path below the prefix, its query and its body unchanged and the provider's own
//! credential. The answer comes back as the provider sent it; an event stream, AWS event-stream or
//! NDJSON answer is relayed as a stream.

use std::sync::Arc;

use antirion_core::Stage;
use antirion_dialects::Endpoint;
use antirion_egress::{Payload, TransportError};
use antirion_error::{Code, GatewayError, Message};
use antirion_identity::access::{ListPair, allowed};
use antirion_providers::{DeploymentRt, Forward};
use bytes::Bytes;
use http::header::{CACHE_CONTROL, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE};
use http::request::Parts;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Response, StatusCode};
use hyper::body::Incoming;
use serde_json::Value;

use crate::account::{self, Admission, End};
use crate::auth::Authenticated;
use crate::ctx::{Body, Ctx, streamed};
use crate::parse::Parsed;
use crate::reqlog::AttemptFact;
use antirion_obs::trace::SpanId;
use antirion_resilience::Class;

/// A mount the request path falls under: its name and the methods it allows.
pub(crate) struct Matched {
    pub(crate) name: String,
    pub(crate) methods: Vec<Method>,
}

const DEFAULT_METHODS: [&str; 3] = ["GET", "POST", "DELETE"];

fn prefix_of(m: &Value) -> Option<&str> {
    m.get("path_prefix")
        .and_then(Value::as_str)
        .map(|p| p.trim_end_matches('/'))
        .filter(|p| !p.is_empty())
}

/// The path below `prefix` when `path` is the prefix itself or a path under it.
fn below<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = path.strip_prefix(prefix)?;
    (rest.is_empty() || rest.starts_with('/')).then_some(rest)
}

/// The mount whose prefix `path` falls under.
pub(crate) fn matched(api: Option<&Value>, path: &str) -> Option<Matched> {
    let mounts = api?.get("passthrough")?.as_object()?;
    mounts.iter().find_map(|(name, m)| {
        below(path, prefix_of(m)?)?;
        let methods = m
            .get("methods")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>())
            .unwrap_or_else(|| DEFAULT_METHODS.to_vec());
        Some(Matched {
            name: name.clone(),
            methods: methods
                .into_iter()
                .filter_map(|m| Method::from_bytes(m.as_bytes()).ok())
                .collect(),
        })
    })
}

/// Hop-by-hop fields, which are never relayed (RFC 9110 §7.6.1).
const HOP_BY_HOP: [&str; 8] = [
    "connection",
    "keep-alive",
    "proxy-connection",
    "proxy-authenticate",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

fn streams(headers: &HeaderMap) -> bool {
    matches!(
        crate::attempt::essence(headers).as_deref(),
        Some("text/event-stream" | "application/vnd.amazon.eventstream" | "application/x-ndjson")
    )
}

fn unavailable(message: &'static str) -> GatewayError {
    GatewayError::new(Code::UpstreamUnavailable, Stage::Attempt, Message::Fixed(message))
}

fn transport(e: &TransportError) -> GatewayError {
    match e {
        TransportError::Timeout | TransportError::ConnectTimeout => GatewayError::new(
            Code::UpstreamTimeout,
            Stage::Attempt,
            Message::Fixed("The provider did not answer in time."),
        ),
        TransportError::EgressDenied(_) => GatewayError::with_reason(
            antirion_egress::reasons::EGRESS_DENIED,
            Stage::Attempt,
            Message::Fixed("The provider's address is denied by the egress rules."),
        ),
        _ => unavailable("The provider could not be reached."),
    }
}

/// Serves a request under a passthrough mount.
pub(crate) async fn run(
    cx: &mut Ctx,
    parts: &Parts,
    incoming: Incoming,
    endpoint: &Endpoint,
    auth: &Authenticated,
) -> Response<Body> {
    match forward(cx, parts, incoming, endpoint, auth).await {
        Ok(resp) => resp,
        Err(e) => {
            account::settle(&cx.account, &cx.log, End::Failed).await;
            cx.error(&e)
        }
    }
}

async fn forward(
    cx: &mut Ctx,
    parts: &Parts,
    incoming: Incoming,
    endpoint: &Endpoint,
    auth: &Authenticated,
) -> Result<Response<Body>, GatewayError> {
    crate::access::require_end_user(auth, auth.end_user.as_deref())?;
    let snap = Arc::clone(&cx.snap);
    let name = endpoint.path_param.clone().unwrap_or_default();
    let mount = snap
        .config
        .doc
        .pointer("/api/passthrough")
        .and_then(|m| m.get(&name))
        .cloned()
        .unwrap_or(Value::Null);
    let provider_name = mount.get("provider").and_then(Value::as_str).unwrap_or_default();
    let Some(provider) = snap.providers.providers.get(provider_name).filter(|p| p.enabled) else {
        return Err(unavailable("The mount's provider is not available."));
    };
    let key = &auth.key;
    let levels = snap.identity.access_levels(key);
    let lists: Vec<&ListPair> = levels.iter().map(|a| &a.providers).collect();
    if !allowed(&provider.name, &lists) {
        return Err(crate::authorize::no_allowed_deployment());
    }
    cx.log.update(|r| r.route = Some(format!("passthrough:{name}")));
    let empty = Bytes::from_static(b"{}");
    let skim = antirion_json::skim(&empty, &antirion_json::JsonLimits::default())
        .map_err(|_| unavailable("The request cannot be admitted."))?;
    let parsed = Parsed::json(empty, skim, Some(Value::Object(serde_json::Map::new())));
    account::admit(
        cx,
        &cx.account,
        &Admission {
            endpoint,
            parsed: &parsed,
            key,
            end_user: auth.end_user.as_deref(),
            model: "",
            route: None,
            tags: &key.tags,
            candidates: &[],
            stream: false,
            hedge: 1,
        },
    )
    .await?;
    let declared = parts
        .headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    let body = crate::body::read(
        incoming,
        snap.security.max_body,
        cx.rt.statics.http.body_read,
        declared,
    )
    .await
    .map_err(|_| crate::accept::too_large(Stage::Parse))?;
    let prefix = prefix_of(&mount).unwrap_or_default();
    let path = below(parts.uri.path(), prefix).unwrap_or_default();
    let caps = snap
        .config
        .doc
        .pointer(&format!("/providers/{}/capabilities", provider.name));
    let d = DeploymentRt::direct(provider, &format!("{}/passthrough", provider.name), "", caps);
    let mut headers = HeaderMap::new();
    let rules = crate::egress::rules(cx, key, None, "");
    crate::egress::outbound(cx, &parts.headers, &rules, false, &mut headers);
    for name in [CONTENT_TYPE, CONTENT_ENCODING] {
        if let Some(v) = parts.headers.get(&name) {
            headers.insert(name, v.clone());
        }
    }
    let mut request = d
        .passthrough_request(Forward {
            method: parts.method.clone(),
            path,
            query: parts.uri.query(),
            headers,
            body: Payload::from(body),
            connect_timeout: None,
        })
        .map_err(|_| unavailable("The request to the provider cannot be built."))?;
    let cred_tls = snap
        .security
        .egress_tls
        .clone()
        .unwrap_or_else(|| provider.tls.clone());
    d.authorize(&mut request, &cx.rt.creds, &cred_tls)
        .await
        .map_err(|_| {
            GatewayError::with_reason(
                crate::reasons::CREDENTIAL_ACQUISITION_FAILED,
                Stage::Attempt,
                Message::Fixed("The gateway could not obtain a credential for the provider."),
            )
        })?;
    let resilience = crate::policy::effective(&snap, "resilience", key, "", None);
    let timers = crate::mount_timers::MountTimers::new(
        &antirion_resilience::Policy::from_value(&resilience).timeouts,
        crate::controls::read_timeout(&parts.headers),
    );
    let head_at = timers.head.map_or_else(
        || tokio::time::Instant::now() + std::time::Duration::from_secs(86_400 * 365),
        |(d, _)| tokio::time::Instant::now() + d,
    );
    let started_at = cx.rt.now();
    let start = std::time::Instant::now();
    let server = request.uri.host().map(|h| {
        let port = request
            .uri
            .port_u16()
            .unwrap_or(if request.uri.scheme_str() == Some("https") {
                443
            } else {
                80
            });
        (h.trim_matches(['[', ']']).to_owned(), port)
    });
    let fact = |status: Option<u16>, class: Class| AttemptFact {
        reason: "primary",
        provider: provider.name.clone(),
        provider_type: provider.kind.as_str().to_owned(),
        deployment: d.name.clone(),
        upstream_model: String::new(),
        credential: None,
        key: None,
        started_at,
        start,
        end: std::time::Instant::now(),
        ttft: None,
        status,
        class: class.as_str(),
        retry_after_ms: None,
        upstream_request_id: None,
        error_message: None,
        error_body: None,
        usage: None,
        usage_raw: None,
        span: SpanId::random(),
        server: server.clone(),
        wait_ms: 0,
        via: None,
        timeout: None,
        tail: false,
    };
    let sent = tokio::select! {
        () = cx.gone.cancelled() => {
            account::settle(&cx.account, &cx.log, End::Cancelled).await;
            let mut resp = Response::new(crate::ctx::empty());
            *resp.status_mut() = StatusCode::from_u16(499).unwrap_or(StatusCode::BAD_REQUEST);
            return Ok(resp);
        }
        () = tokio::time::sleep_until(head_at) => {
            let attempt = fact(None, Class::Timeout);
            cx.log.update(|r| r.attempts.push(attempt));
            return Err(timers.late());
        }
        r = cx.rt.egress.send(request) => r,
    };
    let up = match sent {
        Ok(up) => up,
        Err(e) => {
            let attempt = fact(None, Class::ConnectError);
            cx.log.update(|r| r.attempts.push(attempt));
            return Err(transport(&e));
        }
    };
    let status = up.status;
    let attempt = fact(
        Some(status.as_u16()),
        antirion_resilience::classify::status(status.as_u16(), false),
    );
    let upstream_failed = status.is_server_error().then(|| {
        GatewayError::new(
            Code::UpstreamUnavailable,
            Stage::Attempt,
            Message::Fixed("The provider answered with a server error."),
        )
    });
    cx.log.update(|r| {
        r.attempts.push(attempt);
        if let Some(e) = &upstream_failed {
            r.fail(e);
        }
    });
    let stream = streams(&up.headers);
    if let Some(id) = antirion_egress::headers::upstream_request_id(&up.headers).cloned() {
        cx.log
            .update(|r| r.upstream_request_id = id.to_str().ok().map(str::to_owned));
    }
    let mut resp = Response::new(streamed(
        timers.body(crate::outcome::data_stream(up.body), stream),
    ));
    *resp.status_mut() = status;
    for (name, value) in &up.headers {
        if !HOP_BY_HOP.contains(&name.as_str()) && !(stream && name == CONTENT_LENGTH) {
            resp.headers_mut().append(name.clone(), value.clone());
        }
    }
    if stream {
        let h = resp.headers_mut();
        h.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        h.insert(
            HeaderName::from_static("x-accel-buffering"),
            HeaderValue::from_static("no"),
        );
    }
    account::settle(&cx.account, &cx.log, End::Response(status.as_u16())).await;
    cx.finish(&mut resp, false);
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn mounts_match_at_segment_boundaries() {
        let api = json!({"passthrough": {"pt": {"path_prefix": "/pt/openai/", "provider": "p"}}});
        let m = matched(Some(&api), "/pt/openai/v1/x").unwrap();
        assert_eq!(m.name, "pt");
        assert_eq!(m.methods, [Method::GET, Method::POST, Method::DELETE]);
        assert!(matched(Some(&api), "/pt/openai").is_some());
        assert!(matched(Some(&api), "/pt/openaix/v1").is_none());
        assert_eq!(below("/pt/openai/v1/x", "/pt/openai"), Some("/v1/x"));
    }
}
