//! Endpoints answered without an upstream: the Models list and stored Responses.

use antirion_core::Stage;
use antirion_dialects::Endpoint;
use antirion_error::{Code, GatewayError, Message};
use antirion_runtime::identity::KeyRt;
use bytes::Bytes;
use http::header::CONTENT_TYPE;
use http::{HeaderValue, Response};
use serde_json::{Value, json};

use crate::authorize::{candidates, deployment_allowed, model_allowed, model_not_allowed};
use crate::ctx::{Body, Ctx, full};

/// `GET /v1/models` and `GET /v1/models/{id}`: exactly the Models the key can use, those whose names
/// pass its model lists and that have a deployment it may use.
/// The list shape a models request asks for.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Shape {
    OpenAi,
    Anthropic,
    Gemini,
}

fn shape(parts: &http::request::Parts) -> Shape {
    let path = parts.uri.path();
    if path.starts_with("/gemini/") || path.starts_with("/v1beta/") {
        Shape::Gemini
    } else if path.starts_with("/anthropic/") || parts.headers.contains_key("anthropic-version") {
        Shape::Anthropic
    } else {
        Shape::OpenAi
    }
}

fn query(parts: &http::request::Parts, name: &str) -> Option<String> {
    parts.uri.query()?.split('&').find_map(|p| {
        let (k, v) = p.split_once('=')?;
        (k == name).then(|| v.to_owned())
    })
}

fn bad_param(param: &str, message: &'static str) -> GatewayError {
    GatewayError::new(Code::InvalidRequest, Stage::Parse, Message::Fixed(message)).param(param.to_owned())
}

/// The Anthropic- or Gemini-shaped page of model names.
fn paged(
    cx: &Ctx,
    parts: &http::request::Parts,
    shape: Shape,
    names: &[&str],
) -> Result<Value, GatewayError> {
    let created_at = antirion_core::UtcMicros(cx.rt.now().0).to_rfc3339_seconds();
    if shape == Shape::Anthropic {
        let limit = match query(parts, "limit") {
            None => 20,
            Some(l) => l
                .parse::<usize>()
                .ok()
                .filter(|n| (1..=1000).contains(n))
                .ok_or_else(|| bad_param("limit", "limit must be between 1 and 1000."))?,
        };
        let (start, end) = match (query(parts, "after_id"), query(parts, "before_id")) {
            (Some(a), _) => {
                let s = names.iter().position(|n| *n == a).map_or(names.len(), |i| i + 1);
                (s, (s + limit).min(names.len()))
            }
            (None, Some(b)) => {
                let e = names.iter().position(|n| *n == b).unwrap_or(0);
                (e.saturating_sub(limit), e)
            }
            (None, None) => (0, limit.min(names.len())),
        };
        let page = names.get(start..end).unwrap_or_default();
        let data: Vec<Value> = page
            .iter()
            .map(|n| json!({"type": "model", "id": n, "display_name": n, "created_at": created_at}))
            .collect();
        return Ok(json!({
            "data": data,
            "has_more": end < names.len(),
            "first_id": page.first(),
            "last_id": page.last(),
        }));
    }
    let size = match query(parts, "pageSize") {
        None => 50,
        Some(l) => l
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=1000).contains(n))
            .ok_or_else(|| bad_param("pageSize", "pageSize must be between 1 and 1000."))?,
    };
    let start = query(parts, "pageToken")
        .and_then(|t| names.iter().position(|n| *n == t))
        .map_or(0, |i| i + 1);
    let end = (start + size).min(names.len());
    let page = names.get(start..end).unwrap_or_default();
    let models: Vec<Value> = page
        .iter()
        .map(|n| {
            json!({"name": format!("models/{n}"), "displayName": n,
                   "supportedGenerationMethods": ["generateContent", "streamGenerateContent", "countTokens"]})
        })
        .collect();
    let mut body = json!({"models": models});
    if end < names.len()
        && let (Some(last), Some(o)) = (page.last(), body.as_object_mut())
    {
        o.insert("nextPageToken".into(), Value::from(*last));
    }
    Ok(body)
}

pub(crate) fn models(
    cx: &mut Ctx,
    parts: &http::request::Parts,
    endpoint: &Endpoint,
    key: &KeyRt,
) -> Response<Body> {
    let shape = shape(parts);
    if shape == Shape::Anthropic {
        cx.envelope = antirion_core::Envelope::Anthropic;
    }
    let cx = &*cx;
    let levels = cx.snap.identity.access_levels(key);
    if let Some(id) = &endpoint.path_param
        && !model_allowed(&levels, id)
    {
        return cx.error(&model_not_allowed(id));
    }
    let usable = |m: &antirion_router::ModelRt| {
        let known = candidates(&cx.snap.providers, &cx.snap.routing, m);
        known.is_empty() || known.iter().any(|d| deployment_allowed(&levels, d))
    };
    let now = cx.rt.now();
    let caller = crate::authorize::caller(key);
    let created = now.0 / 1_000_000;
    let object = |name: &str, owned_by: &str| json!({"id": name, "object": "model", "created": created, "owned_by": owned_by});
    let visible = cx.snap.routing.listed().into_iter().filter(|(name, m)| {
        model_allowed(&levels, name) && !m.lifecycle.retired(now) && m.admits(caller) && usable(m)
    });
    let body = match &endpoint.path_param {
        None if shape != Shape::OpenAi => {
            let names: Vec<&str> = visible.map(|(name, _)| name).collect();
            match paged(cx, parts, shape, &names) {
                Ok(b) => b,
                Err(e) => return cx.error(&e),
            }
        }
        None => {
            let data: Vec<Value> = visible.map(|(name, m)| object(name, &m.owned_by)).collect();
            json!({"object": "list", "data": data})
        }
        Some(id) => match visible.into_iter().find(|(name, _)| name == id) {
            Some((name, m)) => object(name, &m.owned_by),
            None => {
                let e = GatewayError::new(
                    Code::ModelNotFound,
                    Stage::Resolve,
                    Message::built(format!(
                        "The model `{}` does not exist or you do not have access to it.",
                        id.chars().take(256).collect::<String>()
                    )),
                )
                .param("model");
                return cx.error(&e);
            }
        },
    };
    json_response(cx, &body)
}

/// `count_tokens` for a model with no native counting API: a local estimate of the input, about four
/// characters per token, never sent upstream.
pub(crate) fn count_estimate(cx: &Ctx, raw: &[u8], responses: bool) -> Response<Body> {
    let body: Value = serde_json::from_slice(raw).unwrap_or(Value::Null);
    let mut chars = 0usize;
    let mut walk = vec![&body];
    while let Some(v) = walk.pop() {
        match v {
            Value::String(s) => chars += s.chars().count(),
            Value::Array(a) => walk.extend(a.iter()),
            Value::Object(o) => walk.extend(o.iter().filter(|(k, _)| k.as_str() != "model").map(|(_, v)| v)),
            _ => {}
        }
    }
    let tokens = chars.div_ceil(4).max(1);
    let body = if responses {
        crate::apis::responses_count(tokens as u64)
    } else {
        json!({"input_tokens": tokens})
    };
    let mut resp = json_response(cx, &body);
    resp.headers_mut().insert(
        http::HeaderName::from_static(antirion_core::headers::TOKEN_COUNT_SOURCE),
        HeaderValue::from_static("estimate"),
    );
    resp
}

pub(crate) fn json_response(cx: &Ctx, body: &Value) -> Response<Body> {
    let mut resp = Response::new(full(Bytes::from(serde_json::to_vec(body).unwrap_or_default())));
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    cx.finish(&mut resp, false);
    resp
}
