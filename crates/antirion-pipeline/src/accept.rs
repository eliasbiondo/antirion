//! Stage `accept`: probes, CORS preflights, the endpoint catalog and the declared body size, all
//! decided from the request head.

use antirion_core::Stage;
use antirion_dialects::{Endpoint, Kind, Matched, legacy_path, match_endpoint};
use antirion_error::reason::hygiene;
use antirion_error::{Code, GatewayError, Message};
use bytes::Bytes;
use http::header::{
    ACCESS_CONTROL_ALLOW_CREDENTIALS, ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS,
    ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_MAX_AGE, ACCESS_CONTROL_REQUEST_HEADERS,
    ACCESS_CONTROL_REQUEST_METHOD, ALLOW, CONTENT_LENGTH, CONTENT_TYPE, VARY,
};
use http::request::Parts;
use http::{HeaderMap, HeaderValue, Method, Response, StatusCode};

use crate::ctx::{Body, Ctx, empty, full};

/// A request refused by an ingress pause that ends in `wait`.
pub(crate) fn paused(stage: Stage, wait: std::time::Duration) -> GatewayError {
    let mut e = GatewayError::with_reason(
        antirion_runtime::operations::ingress::reasons::INGRESS_PAUSED,
        stage,
        Message::Fixed("Ingress is paused; retry once the pause ends."),
    );
    e.retry_after = Some(wait);
    e
}

/// What `accept` decided.
pub(crate) enum Accepted {
    Respond(Response<Body>),
    Endpoint(Endpoint),
}

/// Header sections larger than this, or with more fields, are refused.
const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_HEADER_COUNT: usize = 200;

/// Whether a header section exceeds the size or field-count limit, counting each field as its name,
/// its value and the four bytes of `: ` and CRLF.
pub(crate) fn headers_too_large(headers: &HeaderMap) -> bool {
    if headers.len() > MAX_HEADER_COUNT {
        return true;
    }
    let bytes: usize = headers.iter().map(|(n, v)| n.as_str().len() + v.len() + 4).sum();
    bytes > MAX_HEADER_BYTES
}

pub(crate) fn run(cx: &Ctx, parts: &Parts) -> Accepted {
    if headers_too_large(&parts.headers) {
        let e = GatewayError::new(
            Code::HeadersTooLarge,
            Stage::Accept,
            Message::Fixed("The request headers are too large."),
        )
        .closing();
        return Accepted::Respond(cx.error(&e));
    }
    let uri_len = parts.uri.path_and_query().map_or(0, |p| p.as_str().len());
    if uri_len > cx.snap.security.max_uri_length {
        let e = GatewayError::with_reason(
            hygiene::URI_TOO_LONG,
            Stage::Accept,
            Message::Fixed("The request URI is too long."),
        );
        return Accepted::Respond(cx.error(&e));
    }
    let path = parts.uri.path();
    if path == "/-/livez" || path == "/-/readyz" {
        return Accepted::Respond(probe(cx, parts, path == "/-/livez"));
    }
    if path.starts_with("/-/") {
        return Accepted::Respond(cx.error(&unknown_endpoint(&parts.method, path)));
    }
    if cx.rt.drain.refuses_new() {
        let mut e = GatewayError::with_reason(
            antirion_runtime::drain::reasons::NODE_DRAINING,
            Stage::Accept,
            Message::Fixed("The node is draining; retry on another node."),
        )
        .closing();
        e.retry_after = Some(std::time::Duration::from_secs(1));
        return Accepted::Respond(cx.error(&e));
    }
    if let Some(wait) = cx.rt.state.operations.pauses.refusal(cx.started_at, None) {
        return Accepted::Respond(cx.error(&paused(Stage::Accept, wait)));
    }
    if parts.method == Method::OPTIONS
        && cx.origin.is_some()
        && parts.headers.contains_key(ACCESS_CONTROL_REQUEST_METHOD)
    {
        return Accepted::Respond(preflight(cx, parts));
    }
    let api = cx.snap.config.doc.get("api");
    let mounts_off = api.and_then(|a| a.get("provider_mounts")) == Some(&serde_json::Value::Bool(false));
    let mounted = ["/openai/", "/anthropic/", "/gemini/", "/bedrock/"]
        .iter()
        .any(|m| path.starts_with(m));
    if antirion_dialects::normalise(path).is_some()
        && let Some(m) = crate::passthrough::matched(api, path)
    {
        if m.methods.contains(&parts.method) {
            return Accepted::Endpoint(antirion_dialects::passthrough(&m.name));
        }
        let list: Vec<&str> = m.methods.iter().map(Method::as_str).collect();
        let mut e = GatewayError::new(
            Code::MethodNotAllowed,
            Stage::Accept,
            Message::Fixed("Method not allowed on this path."),
        );
        if let Ok(v) = HeaderValue::from_str(&list.join(", ")) {
            e = e.header(ALLOW, v);
        }
        return Accepted::Respond(cx.error(&e));
    }
    let legacy = api
        .and_then(|a| a.get("azure_paths"))
        .is_some_and(|v| v == &serde_json::Value::Bool(true))
        .then(|| legacy_path(path))
        .flatten();
    if mounts_off && mounted && legacy.is_none() {
        return Accepted::Respond(cx.error(&unknown_endpoint(&parts.method, path)));
    }
    let matched = match &legacy {
        Some((canonical, _)) => match_endpoint(&parts.method, canonical),
        None if crate::socket::is_upgrade(parts) => crate::socket::matched(api),
        None => match_endpoint(&parts.method, path),
    };
    let endpoint = match matched {
        Matched::Found(e)
            if api
                .and_then(|a| a.get("endpoints"))
                .and_then(|cfg| cfg.get(e.id))
                .and_then(|c| c.get("enabled"))
                == Some(&serde_json::Value::Bool(false)) =>
        {
            return Accepted::Respond(cx.error(&unknown_endpoint(&parts.method, path)));
        }
        Matched::Found(e) if e.kind == Kind::Feedback && !cx.snap.obs.feedback.enabled => {
            return Accepted::Respond(cx.error(&unknown_endpoint(&parts.method, path)));
        }
        Matched::Found(mut e) => {
            if let Some((_, model)) = legacy {
                e.path_param = Some(model);
            }
            e
        }
        Matched::NotFound => return Accepted::Respond(cx.error(&unknown_endpoint(&parts.method, path))),
        Matched::NotSupported => {
            return Accepted::Respond(cx.error(&not_found(hygiene::ENDPOINT_NOT_SUPPORTED)));
        }
        Matched::InvalidPath => {
            let e = GatewayError::with_reason(
                hygiene::INVALID_PATH,
                Stage::Accept,
                Message::Fixed("The request path is not allowed."),
            );
            return Accepted::Respond(cx.error(&e));
        }
        Matched::MethodNotAllowed(allowed) => {
            let list: Vec<&str> = allowed.iter().map(Method::as_str).collect();
            let mut e = GatewayError::new(
                Code::MethodNotAllowed,
                Stage::Accept,
                Message::Fixed("Method not allowed on this path."),
            );
            if let Ok(v) = HeaderValue::from_str(&list.join(", ")) {
                e = e.header(ALLOW, v);
            }
            return Accepted::Respond(cx.error(&e));
        }
    };
    if matches!(
        endpoint.kind,
        antirion_dialects::Kind::Realtime | antirion_dialects::Kind::ResponsesSocket
    ) && let Some(origin) = cx.origin.as_deref()
        && !cx
            .snap
            .security
            .cors_data
            .as_ref()
            .is_some_and(|c| c.allows(origin))
    {
        let e = GatewayError::with_reason(
            hygiene::CORS_ORIGIN_NOT_ALLOWED,
            Stage::Accept,
            Message::Fixed("The origin is not allowed to open a WebSocket here."),
        );
        return Accepted::Respond(cx.error(&e));
    }
    let declared = parts
        .headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    let upload = crate::parse::is_multipart(&endpoint)
        || (endpoint.kind == antirion_dialects::Kind::Files && parts.method == Method::POST);
    let cap = if upload {
        cx.snap.security.max_upload_body
    } else {
        cx.snap.security.max_body
    };
    if declared.is_some_and(|n| n > cap) {
        return Accepted::Respond(cx.error(&too_large(Stage::Accept)));
    }
    Accepted::Endpoint(endpoint)
}

/// The 404 of a path no endpoint serves; the message names the method and the path (without its
/// query, at most 256 characters).
pub(crate) fn unknown_endpoint(method: &Method, path: &str) -> GatewayError {
    let shown: String = path
        .chars()
        .take(256)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    GatewayError::with_reason(
        hygiene::UNKNOWN_ENDPOINT,
        Stage::Accept,
        Message::built(format!("Unknown endpoint: {method} {shown}")),
    )
}

pub(crate) fn not_found(reason: antirion_error::Reason) -> GatewayError {
    GatewayError::with_reason(reason, Stage::Accept, Message::Fixed("Unknown request path."))
}

pub(crate) fn too_large(stage: Stage) -> GatewayError {
    GatewayError::with_reason(
        hygiene::BODY_TOO_LARGE,
        stage,
        Message::Fixed("The request body is too large."),
    )
    .closing()
}

fn probe(cx: &Ctx, parts: &Parts, live: bool) -> Response<Body> {
    if parts.method != Method::GET && parts.method != Method::HEAD {
        let e = GatewayError::new(
            Code::MethodNotAllowed,
            Stage::Accept,
            Message::Fixed("Probes accept GET and HEAD only."),
        )
        .header(ALLOW, HeaderValue::from_static("GET, HEAD"));
        return cx.error(&e);
    }
    let (status, body) = if live {
        antirion_runtime::probes::livez(&cx.rt)
    } else {
        antirion_runtime::probes::readyz(&cx.rt)
    };
    let bytes = if parts.method == Method::HEAD {
        Bytes::new()
    } else {
        Bytes::from(serde_json::to_vec(&body).unwrap_or_default())
    };
    let mut resp = Response::new(full(bytes));
    *resp.status_mut() = status;
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    cx.finish(&mut resp, false);
    resp.headers_mut().remove(antirion_core::headers::VERSION);
    resp
}

fn preflight(cx: &Ctx, parts: &Parts) -> Response<Body> {
    let origin = cx.origin.as_deref().unwrap_or_default();
    let Some(cors) = cx.snap.security.cors_data.as_ref().filter(|c| c.allows(origin)) else {
        let e = GatewayError::with_reason(
            hygiene::CORS_ORIGIN_NOT_ALLOWED,
            Stage::Accept,
            Message::Fixed("The origin is not allowed."),
        );
        return cx.error(&e);
    };
    let mut resp = Response::new(empty());
    *resp.status_mut() = StatusCode::NO_CONTENT;
    let h = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(cors.allow_origin_value(origin)) {
        h.insert(ACCESS_CONTROL_ALLOW_ORIGIN, v);
    }
    if let Ok(v) = HeaderValue::from_str(&cors.methods) {
        h.insert(ACCESS_CONTROL_ALLOW_METHODS, v);
    }
    let requested = parts.headers.get(ACCESS_CONTROL_REQUEST_HEADERS).cloned();
    match (&cors.allow_headers, requested) {
        (Some(list), _) => {
            if let Ok(v) = HeaderValue::from_str(list) {
                h.insert(ACCESS_CONTROL_ALLOW_HEADERS, v);
            }
        }
        (None, Some(v)) => {
            h.insert(ACCESS_CONTROL_ALLOW_HEADERS, v);
        }
        (None, None) => {}
    }
    h.insert(ACCESS_CONTROL_MAX_AGE, HeaderValue::from(cors.max_age_secs));
    if cors.allow_credentials {
        h.insert(ACCESS_CONTROL_ALLOW_CREDENTIALS, HeaderValue::from_static("true"));
    }
    h.append(VARY, HeaderValue::from_static("Access-Control-Request-Method"));
    h.append(VARY, HeaderValue::from_static("Access-Control-Request-Headers"));
    cx.finish(&mut resp, false);
    resp
}

#[cfg(test)]
mod tests {
    use http::HeaderName;

    use super::*;

    #[test]
    fn header_limits() {
        let mut h = HeaderMap::new();
        for i in 0..150 {
            let name = HeaderName::from_bytes(format!("x-pad-{i:03}").as_bytes()).unwrap();
            h.insert(name, HeaderValue::from_str(&"a".repeat(320)).unwrap());
        }
        assert!(!headers_too_large(&h));
        for i in 0..51 {
            let name = HeaderName::from_bytes(format!("x-more-{i}").as_bytes()).unwrap();
            h.insert(name, HeaderValue::from_static("v"));
        }
        assert!(headers_too_large(&h));
        let mut big = HeaderMap::new();
        for i in 0..66 {
            let name = HeaderName::from_bytes(format!("x-pad-{i:02}").as_bytes()).unwrap();
            big.insert(name, HeaderValue::from_str(&"a".repeat(1000)).unwrap());
        }
        assert!(headers_too_large(&big));
    }
}
