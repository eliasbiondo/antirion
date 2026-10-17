//! Lifecycle classification of native upstream streams: Anthropic Messages (over SSE, or inside
//! Bedrock `chunk` messages), Gemini, and Bedrock ConverseStream.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;

use super::events::{Classified, Kind, StreamDialect, StreamFacts, message_of, nonempty};
use super::pump::{Frame, UpEvent};
use crate::reqlog::{Convention, Usage};

/// The effective status of an Anthropic error type.
pub(crate) fn anthropic_status(err: &Value) -> u16 {
    match err.get("type").and_then(Value::as_str).unwrap_or_default() {
        "invalid_request_error" => 400,
        "authentication_error" => 401,
        "billing_error" => 402,
        "permission_error" => 403,
        "not_found_error" => 404,
        "conflict_error" => 409,
        "request_too_large" => 413,
        "rate_limit_error"
            if err
                .get("details")
                .and_then(|d| d.get("error_code"))
                .and_then(Value::as_str)
                == Some("enforced_spend_limit_reached") =>
        {
            402
        }
        "rate_limit_error" => 429,
        "api_error" => 500,
        "overloaded_error" => 529,
        "timeout_error" => 504,
        _ => 502,
    }
}

/// The effective status of a Bedrock exception name.
pub(crate) fn bedrock_status(name: &str) -> u16 {
    let lower = name.to_ascii_lowercase();
    match lower.trim_end_matches("exception") {
        "throttling" => 429,
        "validation" => 400,
        "accessdenied" => 403,
        "resourcenotfound" => 404,
        "serviceunavailable" => 503,
        "internalserver" => 500,
        "modeltimeout" => 504,
        _ => 502,
    }
}

fn plain(kind: Kind) -> Classified {
    Classified {
        kind,
        forward: false,
        json: None,
    }
}

fn chars(s: Option<&str>) -> u64 {
    s.map_or(0, |s| u64::try_from(s.chars().count()).unwrap_or(u64::MAX))
}

fn anthropic_usage(u: Option<&Value>) -> Usage {
    let get = |n: &str| u.and_then(|u| u.get(n)).and_then(Value::as_u64);
    let cache = get("cache_read_input_tokens").unwrap_or(0) + get("cache_creation_input_tokens").unwrap_or(0);
    let usage = Usage {
        input: get("input_tokens").map(|i| i + cache),
        output: get("output_tokens"),
        ..Usage::default()
    };
    match u {
        Some(u) => usage.with_subsets(Convention::Anthropic, u),
        None => usage,
    }
}

/// One Anthropic event (its JSON already parsed).
fn anthropic(event: Option<&str>, json: Value, facts: &mut StreamFacts) -> Classified {
    let kind = json
        .get("type")
        .and_then(Value::as_str)
        .or(event)
        .unwrap_or("message")
        .to_owned();
    let class = match kind.as_str() {
        "error" => {
            let err = json.get("error").cloned().unwrap_or(Value::Null);
            Kind::Error {
                status: anthropic_status(&err),
                kind: err.get("type").and_then(Value::as_str).map(str::to_owned),
                message: message_of(&err),
            }
        }
        "message_start" => {
            let m = json.get("message");
            facts.usage.merge(anthropic_usage(m.and_then(|m| m.get("usage"))));
            if facts.id.is_none() {
                facts.id = m
                    .and_then(|m| m.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                facts.model = m
                    .and_then(|m| m.get("model"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            Kind::Preamble
        }
        "content_block_start" => {
            if json.pointer("/content_block/type").and_then(Value::as_str) == Some("tool_use") {
                facts.tool_calls += 1;
            }
            Kind::Preamble
        }
        "ping" => Kind::Preamble,
        "content_block_delta" => {
            let d = json.get("delta");
            for n in ["text", "thinking", "partial_json"] {
                facts.output_chars += chars(d.and_then(|d| d.get(n)).and_then(Value::as_str));
            }
            Kind::Content
        }
        "content_block_stop" => Kind::Content,
        "message_delta" => {
            facts.usage.merge(anthropic_usage(json.get("usage")));
            if let Some(r) = json
                .get("delta")
                .and_then(|d| d.get("stop_reason"))
                .and_then(Value::as_str)
            {
                facts.native_finished = true;
                facts.finish_reasons.push(r.to_owned());
            }
            Kind::Content
        }
        "message_stop" => Kind::Terminator,
        _ => Kind::Preamble,
    };
    let forward = !matches!(class, Kind::Error { .. });
    Classified {
        kind: class,
        forward,
        json: Some(json),
    }
}

/// Gemini's finish reason for a request whose function-call history lacks a thought signature.
pub(crate) const MISSING_THOUGHT_SIGNATURE: &str = "MISSING_THOUGHT_SIGNATURE";

/// Whether a Gemini answer is the rejection of a replay without its thought signature: a candidate
/// that finished with `MISSING_THOUGHT_SIGNATURE` and produced no part.
pub(crate) fn missing_thought_signature(v: &Value) -> bool {
    v.get("candidates")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|c| {
            c.get("finishReason").and_then(Value::as_str) == Some(MISSING_THOUGHT_SIGNATURE)
                && !nonempty(c.get("content").and_then(|c| c.get("parts")))
        })
}

fn gemini(json: Value, facts: &mut StreamFacts) -> Classified {
    if let Some(err) = json.get("error").filter(|e| !e.is_null()) {
        let status = err
            .get("code")
            .and_then(Value::as_u64)
            .and_then(|c| u16::try_from(c).ok())
            .filter(|c| (400..=599).contains(c))
            .unwrap_or(502);
        return plain(Kind::Error {
            status,
            kind: err.get("status").and_then(Value::as_str).map(str::to_owned),
            message: message_of(err),
        });
    }
    if let Some(u) = json.get("usageMetadata") {
        let get = |n: &str| u.get(n).and_then(Value::as_u64);
        let output = match (get("candidatesTokenCount"), get("thoughtsTokenCount")) {
            (None, None) => None,
            (c, t) => Some(c.unwrap_or(0) + t.unwrap_or(0)),
        };
        facts.usage.merge(
            Usage {
                input: get("promptTokenCount"),
                output,
                ..Usage::default()
            }
            .with_subsets(Convention::Gemini, u),
        );
    }
    if facts.model.is_none() {
        facts.model = json
            .get("modelVersion")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    if missing_thought_signature(&json) {
        return plain(Kind::Error {
            status: 400,
            kind: Some(MISSING_THOUGHT_SIGNATURE.to_owned()),
            message: Some("The upstream rejected the request: a thought signature is missing.".to_owned()),
        });
    }
    let mut content = false;
    for c in json
        .get("candidates")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let parts = c.get("content").and_then(|c| c.get("parts"));
        if nonempty(parts) {
            content = true;
        }
        for p in parts.and_then(Value::as_array).into_iter().flatten() {
            facts.output_chars += chars(p.get("text").and_then(Value::as_str));
            if p.get("functionCall").is_some() {
                facts.tool_calls += 1;
            }
        }
        if let Some(r) = c.get("finishReason").and_then(Value::as_str) {
            content = true;
            facts.native_finished = true;
            facts.finish_reasons.push(r.to_owned());
        }
    }
    if json
        .get("promptFeedback")
        .and_then(|p| p.get("blockReason"))
        .is_some()
    {
        content = true;
        facts.native_finished = true;
    }
    Classified {
        kind: if content { Kind::Content } else { Kind::Preamble },
        forward: true,
        json: Some(json),
    }
}

fn converse(event: &str, json: Value, facts: &mut StreamFacts) -> Classified {
    let class = match event {
        "contentBlockStart" => {
            if json.pointer("/start/toolUse").is_some() {
                facts.tool_calls += 1;
            }
            Kind::Preamble
        }
        "messageStart" => Kind::Preamble,
        "contentBlockDelta" => {
            let d = json.get("delta");
            facts.output_chars += chars(d.and_then(|d| d.get("text")).and_then(Value::as_str));
            Kind::Content
        }
        "contentBlockStop" => Kind::Content,
        "messageStop" => {
            facts.native_finished = true;
            if let Some(r) = json.get("stopReason").and_then(Value::as_str) {
                facts.finish_reasons.push(r.to_owned());
            }
            Kind::Content
        }
        "metadata" => {
            let u = json.get("usage");
            let get = |n: &str| u.and_then(|u| u.get(n)).and_then(Value::as_u64);
            let usage = Usage {
                input: get("inputTokens"),
                output: get("outputTokens"),
                ..Usage::default()
            };
            facts.usage.merge(match u {
                Some(u) => usage.with_subsets(Convention::BedrockConverse, u),
                None => usage,
            });
            if facts.native_finished {
                Kind::Terminator
            } else {
                Kind::Content
            }
        }
        _ => Kind::Preamble,
    };
    Classified {
        kind: class,
        forward: true,
        json: Some(json),
    }
}

/// The Anthropic (or model-native) event inside a Bedrock `chunk` message.
pub(crate) fn invoke_inner(data: &str) -> Option<Value> {
    let outer: Value = serde_json::from_str(data).ok()?;
    let bytes = STANDARD.decode(outer.get("bytes")?.as_str()?).ok()?;
    serde_json::from_slice::<Value>(&bytes)
        .ok()
        .filter(Value::is_object)
}

const ANTHROPIC_EVENTS: &[&str] = &[
    "message_start",
    "content_block_start",
    "content_block_delta",
    "content_block_stop",
    "message_delta",
    "message_stop",
    "ping",
    "error",
];

/// Classifies one event of a native stream and updates the stream's facts.
pub(super) fn classify(dialect: StreamDialect, ev: &UpEvent, facts: &mut StreamFacts) -> Classified {
    match &ev.frame {
        Frame::Exception(name) | Frame::Error(name) => {
            let json: Value = serde_json::from_str(&ev.data).unwrap_or(Value::Null);
            return plain(Kind::Error {
                status: bedrock_status(name),
                kind: Some(name.clone()).filter(|n| !n.is_empty()),
                message: message_of(&json),
            });
        }
        Frame::Event => {}
    }
    let data = ev.data.trim_matches(|c: char| c.is_ascii_whitespace());
    if data.is_empty() && !dialect.event_stream() {
        return plain(Kind::Empty);
    }
    match dialect {
        StreamDialect::InvokeAnthropic | StreamDialect::Invoke => {
            if ev.event.as_deref().is_some_and(|e| e != "chunk") {
                return Classified {
                    kind: Kind::Preamble,
                    forward: true,
                    json: None,
                };
            }
            let Some(inner) = invoke_inner(data) else {
                return plain(Kind::Malformed);
            };
            let typed = inner
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|t| ANTHROPIC_EVENTS.contains(&t));
            if typed {
                return anthropic(None, inner, facts);
            }
            if let Some(m) = inner.get("amazon-bedrock-invocationMetrics") {
                facts.usage.merge(Usage {
                    input: m.get("inputTokenCount").and_then(Value::as_u64),
                    output: m.get("outputTokenCount").and_then(Value::as_u64),
                    ..Usage::default()
                });
            }
            Classified {
                kind: Kind::Content,
                forward: true,
                json: Some(inner),
            }
        }
        _ => {
            let Ok(json) = serde_json::from_str::<Value>(data) else {
                return plain(Kind::Malformed);
            };
            if !json.is_object() {
                return plain(Kind::Malformed);
            }
            match dialect {
                StreamDialect::Gemini => gemini(json, facts),
                StreamDialect::Converse => converse(ev.event.as_deref().unwrap_or_default(), json, facts),
                _ => anthropic(ev.event.as_deref(), json, facts),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use serde_json::json;

    use super::*;

    fn ev(event: Option<&str>, data: &str, frame: Frame) -> UpEvent {
        UpEvent {
            raw: Bytes::new(),
            event: event.map(str::to_owned),
            data: data.to_owned(),
            frame,
        }
    }

    #[test]
    fn anthropic_lifecycle() {
        let mut f = StreamFacts::default();
        let c = |d: &str, f: &mut StreamFacts| {
            classify(StreamDialect::Anthropic, &ev(None, d, Frame::Event), f).kind
        };
        assert!(matches!(
            c(
                r#"{"type":"message_start","message":{"usage":{"input_tokens":3}}}"#,
                &mut f
            ),
            Kind::Preamble
        ));
        assert!(matches!(c(r#"{"type":"ping"}"#, &mut f), Kind::Preamble));
        assert!(matches!(
            c(
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#,
                &mut f
            ),
            Kind::Content
        ));
        assert!(matches!(
            c(
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
                &mut f
            ),
            Kind::Content
        ));
        assert!(f.native_finished);
        assert_eq!((f.usage.input, f.usage.output), (Some(3), Some(1)));
        assert!(matches!(
            c(r#"{"type":"message_stop"}"#, &mut f),
            Kind::Terminator
        ));
        assert!(matches!(
            c(
                r#"{"type":"error","error":{"type":"overloaded_error","message":"busy"}}"#,
                &mut f
            ),
            Kind::Error { status: 529, .. }
        ));
    }

    #[test]
    fn bedrock_messages() {
        let mut f = StreamFacts::default();
        let exc = ev(
            None,
            r#"{"message":"slow down"}"#,
            Frame::Exception("throttlingException".into()),
        );
        assert!(matches!(
            classify(StreamDialect::Converse, &exc, &mut f).kind,
            Kind::Error { status: 429, .. }
        ));
        let inner =
            STANDARD.encode(json!({"type": "content_block_delta", "delta": {"text": "x"}}).to_string());
        let chunk = ev(Some("chunk"), &json!({"bytes": inner}).to_string(), Frame::Event);
        let c = classify(StreamDialect::InvokeAnthropic, &chunk, &mut f);
        assert!(matches!(c.kind, Kind::Content));
        assert_eq!(c.json.unwrap()["type"], "content_block_delta");
        let stop = ev(Some("messageStop"), r#"{"stopReason":"end_turn"}"#, Frame::Event);
        classify(StreamDialect::Converse, &stop, &mut f);
        let meta = ev(
            Some("metadata"),
            r#"{"usage":{"inputTokens":1,"outputTokens":2}}"#,
            Frame::Event,
        );
        assert!(matches!(
            classify(StreamDialect::Converse, &meta, &mut f).kind,
            Kind::Terminator
        ));
        assert_eq!(bedrock_status("ModelStreamErrorException"), 502);
    }

    #[test]
    fn gemini_chunks() {
        let mut f = StreamFacts::default();
        let c = classify(
            StreamDialect::Gemini,
            &ev(
                None,
                r#"{"candidates":[{"content":{"parts":[{"text":"a"}]},"finishReason":"STOP"}]}"#,
                Frame::Event,
            ),
            &mut f,
        );
        assert!(matches!(c.kind, Kind::Content));
        assert!(f.complete_at_clean_end(StreamDialect::Gemini));
        let e = classify(
            StreamDialect::Gemini,
            &ev(
                None,
                r#"{"error":{"code":429,"message":"quota","status":"RESOURCE_EXHAUSTED"}}"#,
                Frame::Event,
            ),
            &mut f,
        );
        assert!(matches!(e.kind, Kind::Error { status: 429, .. }));
    }
}
