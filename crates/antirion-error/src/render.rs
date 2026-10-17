//! Rendering a [`GatewayError`] in every envelope.

use antirion_core::{Envelope, RequestId, headers as h};
use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use serde_json::{Map, Value, json};

use crate::{Code, GatewayError};

/// A rendered error: status, headers and body.
#[derive(Debug)]
pub struct Rendered {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

/// The `antirion` object every data-plane error carries.
#[must_use]
pub fn antirion_object(err: &GatewayError, request_id: &RequestId) -> Value {
    let mut obj = Map::new();
    obj.insert("code".into(), Value::from(err.code.as_str()));
    if let Some(reason) = err.reason {
        obj.insert("reason".into(), Value::from(reason.as_str()));
    }
    obj.insert("request_id".into(), Value::from(request_id.as_str()));
    obj.insert("stage".into(), Value::from(err.stage.as_str()));
    obj.insert("retryable".into(), Value::from(err.retryable()));
    obj.insert("attempts".into(), Value::from(err.attempts));
    let upstream = err.upstream.as_deref();
    obj.insert(
        "provider".into(),
        upstream.map_or(Value::Null, |u| Value::from(u.provider.as_str())),
    );
    obj.insert(
        "upstream_status".into(),
        upstream.and_then(|u| u.status).map_or(Value::Null, Value::from),
    );
    obj.insert(
        "failure_class".into(),
        upstream.map_or(Value::Null, |u| Value::from(u.failure_class)),
    );
    let upstream_error = upstream.and_then(|u| u.error.as_ref());
    let own_code = upstream_error
        .and_then(|e| e.code.as_deref())
        .filter(|c| !c.is_empty() && c.parse::<u64>().is_err());
    obj.insert(
        "upstream_type".into(),
        own_code
            .or_else(|| upstream_error.and_then(|e| e.kind.as_deref()))
            .map_or(Value::Null, Value::from),
    );
    obj.insert(
        "limit".into(),
        err.limit.as_deref().cloned().unwrap_or(Value::Null),
    );
    if !err.hide_guardrail {
        obj.insert(
            "guardrail".into(),
            err.guardrail.as_deref().map_or(Value::Null, |g| g.shown.clone()),
        );
    }
    Value::Object(obj)
}

/// Renders the error in `envelope`. `jsonrpc_id` is the request's JSON-RPC id when it was read.
#[must_use]
pub fn render(
    err: &GatewayError,
    envelope: Envelope,
    request_id: &RequestId,
    jsonrpc_id: Option<&Value>,
) -> Rendered {
    let status = StatusCode::from_u16(err.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let message = err.message.as_str();
    let antirion = antirion_object(err, request_id);
    let t = Types::of(err, status.as_u16());
    let body = match envelope {
        Envelope::OpenAi => json!({
            "error": {
                "message": message,
                "type": t.openai_type,
                "param": t.param,
                "code": t.openai_code,
            },
            "antirion": antirion,
        }),
        Envelope::Anthropic => json!({
            "type": "error",
            "error": {"type": t.anthropic, "message": anthropic_message(err)},
            "request_id": request_id.as_str(),
            "antirion": antirion,
        }),
        Envelope::Gemini => json!({
            "error": {"code": status.as_u16(), "message": anthropic_message(err), "status": t.gemini},
            "antirion": antirion,
        }),
        Envelope::Bedrock => json!({
            "__type": t.bedrock,
            "message": anthropic_message(err),
            "antirion": antirion,
        }),
        Envelope::JsonRpc => json!({
            "jsonrpc": "2.0",
            "id": jsonrpc_id.cloned().unwrap_or(Value::Null),
            "error": {"code": err.code.jsonrpc_code(), "message": message, "data": {"antirion": antirion}},
        }),
    };
    let body = Bytes::from(serde_json::to_vec(&body).unwrap_or_default());

    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(v) = HeaderValue::from_str(request_id.as_str()) {
        if envelope == Envelope::Anthropic {
            headers.insert(HeaderName::from_static(h::REQUEST_ID_ANTHROPIC), v.clone());
        }
        headers.insert(HeaderName::from_static(h::X_REQUEST_ID), v);
    }
    headers.insert(
        HeaderName::from_static(h::RESPONSE_PATH),
        HeaderValue::from_static(err.response_path()),
    );
    if envelope == Envelope::Bedrock
        && let Ok(v) = HeaderValue::from_str(&t.bedrock)
    {
        headers.insert(HeaderName::from_static(h::X_AMZN_ERROR_TYPE), v);
    }
    if let Some(wait) = err.retry_after {
        headers.insert(header::RETRY_AFTER, HeaderValue::from(retry_after_seconds(wait)));
    }
    if let Some(status) = err.upstream.as_ref().and_then(|u| u.status) {
        headers.insert(
            HeaderName::from_static(h::UPSTREAM_STATUS),
            HeaderValue::from(status),
        );
    }
    let code = status.as_u16();
    if (matches!(code, 408 | 409 | 429) || code >= 500) && !err.retryable() {
        headers.insert(
            HeaderName::from_static(h::X_SHOULD_RETRY),
            HeaderValue::from_static("false"),
        );
    }
    for (name, value) in err.extra_headers.iter().flat_map(|h| h.iter()) {
        headers.append(name.clone(), value.clone());
    }
    Rendered {
        status,
        headers,
        body,
    }
}

/// The type names and codes an error is rendered with in each envelope.
struct Types {
    openai_type: String,
    openai_code: String,
    param: Option<String>,
    anthropic: String,
    gemini: String,
    bedrock: String,
}

/// The code whose row gives an `upstream_rejected` error its type names, by upstream status.
fn rejected_row(status: u16, upstream_kind: Option<&str>) -> Code {
    match status {
        401 => Code::Unauthenticated,
        403 => Code::Forbidden,
        404 if upstream_kind.is_some_and(|k| k.contains("model")) => Code::ModelNotFound,
        404 => Code::NotFound,
        408 => Code::RequestTimeout,
        413 => Code::PayloadTooLarge,
        _ => Code::InvalidRequest,
    }
}

impl Types {
    fn of(err: &GatewayError, status: u16) -> Types {
        let upstream = err.upstream.as_deref().and_then(|u| u.error.as_deref());
        let row = if err.code == Code::UpstreamRejected {
            rejected_row(status, upstream.and_then(|u| u.kind.as_deref()))
        } else {
            err.code
        };
        let mut t = Types {
            openai_type: row.openai_type().to_owned(),
            openai_code: err.openai_code.unwrap_or(err.code.openai_code()).to_owned(),
            param: err.param.as_deref().map(str::to_owned),
            anthropic: row.anthropic_type().to_owned(),
            gemini: row.gemini_status().to_owned(),
            bedrock: row.bedrock_type().to_owned(),
        };
        if err.code == Code::UpstreamUnavailable
            && err.reason.is_some_and(|r| r.as_str() == "upstream_overloaded")
        {
            "overloaded_error".clone_into(&mut t.anthropic);
        }
        if status == 409 {
            "ABORTED".clone_into(&mut t.gemini);
            "ConflictException".clone_into(&mut t.bedrock);
        }
        let Some(u) = upstream else { return t };
        let normalised = err
            .reason
            .map(|r| r.as_str())
            .filter(|r| matches!(*r, "context_length_exceeded" | "content_policy_violation"));
        if u.passthrough {
            if let Some(k) = &u.kind {
                t.openai_type.clone_from(k);
                t.anthropic.clone_from(k);
                t.gemini.clone_from(k);
                t.bedrock.clone_from(k);
            }
            if let Some(c) = &u.code {
                t.openai_code.clone_from(c);
            } else if err.code == Code::UpstreamRejected {
                t.openai_code = normalised
                    .map(str::to_owned)
                    .or_else(|| u.kind.clone())
                    .unwrap_or_else(|| row.openai_code().to_owned());
            }
            t.param.clone_from(&u.param);
        } else if err.code == Code::UpstreamRejected {
            let own_code = u.code.clone().filter(|c| c.parse::<u64>().is_err());
            t.openai_code = normalised
                .map(str::to_owned)
                .or(own_code)
                .or_else(|| u.kind.clone())
                .unwrap_or_else(|| row.openai_code().to_owned());
            if u.param.is_some() {
                t.param.clone_from(&u.param);
            }
        }
        if err.code == Code::UpstreamUnavailable
            && err.reason.is_some_and(|r| r.as_str() == "upstream_overloaded")
        {
            "overloaded_error".clone_into(&mut t.anthropic);
        }
        t
    }
}

/// Dialects without `param` start the message with the field path, so the client can still see it.
fn anthropic_message(err: &GatewayError) -> String {
    match err.param.as_deref() {
        Some(p) if err.code == Code::InvalidRequest && !err.message.as_str().starts_with(p) => {
            format!("{p}: {}", err.message.as_str())
        }
        _ => err.message.as_str().to_owned(),
    }
}

/// `Retry-After` in whole seconds: the wait rounded up, at least 1.
#[must_use]
pub fn retry_after_seconds(wait: std::time::Duration) -> u64 {
    let secs = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
    secs.max(1)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use antirion_core::Stage;

    use super::*;
    use crate::{ALL_CODES, Code, Message, reason::hygiene};

    fn body(r: &Rendered) -> Value {
        serde_json::from_slice(&r.body).unwrap()
    }

    #[test]
    fn every_code_renders_in_every_envelope() {
        let rid = RequestId::generate();
        for code in ALL_CODES {
            for env in [
                Envelope::OpenAi,
                Envelope::Anthropic,
                Envelope::Gemini,
                Envelope::Bedrock,
                Envelope::JsonRpc,
            ] {
                let err = GatewayError::new(code, Stage::Accept, Message::Fixed("x"));
                let r = render(&err, env, &rid, None);
                let b = body(&r);
                let obj = if env == Envelope::JsonRpc {
                    &b["error"]["data"]["antirion"]
                } else {
                    &b["antirion"]
                };
                assert_eq!(obj["code"], code.as_str());
                assert_eq!(obj["request_id"], rid.as_str());
                assert_eq!(r.headers["content-type"], "application/json");
                assert_eq!(r.headers["cache-control"], "no-store");
            }
        }
    }

    #[test]
    fn passthrough_without_a_code_takes_the_upstream_type() {
        let rid = RequestId::generate();
        let mut err = GatewayError::new(
            Code::UpstreamRejected,
            Stage::Attempt,
            Message::Fixed("Invalid input"),
        );
        err.upstream = Some(Box::new(crate::UpstreamFacts {
            provider: "p".to_owned(),
            status: Some(400),
            failure_class: "client_error",
            client_status: Some(400),
            error: Some(Box::new(crate::UpstreamError {
                kind: Some("invalid_request_error".to_owned()),
                code: None,
                param: Some("input".to_owned()),
                passthrough: true,
            })),
        }));
        let b = body(&render(&err, Envelope::OpenAi, &rid, None));
        assert_eq!(b["error"]["code"], "invalid_request_error");
        assert_eq!(b["error"]["param"], "input");
    }

    #[test]
    fn openai_envelope_has_all_four_members() {
        let rid = RequestId::generate();
        let err = GatewayError::with_reason(hygiene::INVALID_JSON, Stage::Parse, Message::Fixed("bad"));
        let b = body(&render(&err, Envelope::OpenAi, &rid, None));
        assert_eq!(b["error"]["type"], "invalid_request_error");
        assert_eq!(b["error"]["code"], "invalid_value");
        assert!(b["error"]["param"].is_null());
        assert_eq!(b["antirion"]["reason"], "invalid_json");
    }

    #[test]
    fn reason_is_omitted_when_absent() {
        let rid = RequestId::generate();
        let err = GatewayError::new(Code::Internal, Stage::Respond, Message::Fixed("x"));
        let b = body(&render(&err, Envelope::OpenAi, &rid, None));
        assert!(b["antirion"].get("reason").is_none());
        assert!(b["antirion"]["limit"].is_null());
    }

    #[test]
    fn retry_signal_headers() {
        let rid = RequestId::generate();
        let mut err = GatewayError::new(Code::RateLimited, Stage::Admit, Message::Fixed("x"));
        err.retry_after = Some(Duration::from_millis(1500));
        let r = render(&err, Envelope::OpenAi, &rid, None);
        assert_eq!(r.headers["retry-after"], "2");
        assert!(r.headers.get("x-should-retry").is_none());
        let fixed = GatewayError::new(Code::RequestExceedsLimit, Stage::Admit, Message::Fixed("x"));
        let r = render(&fixed, Envelope::OpenAi, &rid, None);
        assert_eq!(r.headers["x-should-retry"], "false");
    }

    #[test]
    fn anthropic_carries_request_id_twice() {
        let rid = RequestId::generate();
        let err = GatewayError::new(Code::PayloadTooLarge, Stage::Accept, Message::Fixed("x"));
        let r = render(&err, Envelope::Anthropic, &rid, None);
        assert_eq!(r.headers["request-id"], rid.as_str());
        assert_eq!(body(&r)["request_id"], rid.as_str());
        assert_eq!(body(&r)["error"]["type"], "request_too_large");
    }

    #[test]
    fn retry_after_rounds_up_to_at_least_one() {
        assert_eq!(retry_after_seconds(Duration::ZERO), 1);
        assert_eq!(retry_after_seconds(Duration::from_secs(7)), 7);
        assert_eq!(retry_after_seconds(Duration::from_millis(6001)), 7);
    }
}
