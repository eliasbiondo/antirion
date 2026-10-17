//! The bytes the gateway itself writes into a stream: forwarded events (rewritten when the model name
//! must change), heartbeats, the usage chunk, error events and streams synthesized from a complete
//! response.

use antirion_error::{Code, GatewayError, antirion_object};
use antirion_json::{JsonLimits, Splice, skim};
use bytes::{BufMut, Bytes, BytesMut};
use serde_json::{Value, json};

use super::Setup;
use super::encode::ClientDialect;
use super::events::{StreamDialect, StreamFacts};
use super::pump::UpEvent;
use crate::reqlog::Usage;

pub(super) fn frame(event: Option<&str>, json: &[u8]) -> Bytes {
    let mut out = BytesMut::with_capacity(json.len() + 32);
    if let Some(e) = event {
        out.put_slice(b"event: ");
        out.put_slice(e.as_bytes());
        out.put_u8(b'\n');
    }
    out.put_slice(b"data: ");
    out.put_slice(json);
    out.put_slice(b"\n\n");
    out.freeze()
}

fn compact(v: &Value) -> Vec<u8> {
    serde_json::to_vec(v).unwrap_or_default()
}

/// A Chat chunk as one `data:` event.
pub(crate) fn chat_frame(v: &Value) -> Bytes {
    frame(None, &compact(v))
}

/// A Gemini chunk as one `data:` event with Google's `CR LF CR LF` separator.
pub(crate) fn gemini_frame(v: &Value) -> Bytes {
    let mut out = BytesMut::new();
    out.put_slice(b"data: ");
    out.put_slice(&compact(v));
    out.put_slice(b"\r\n\r\n");
    out.freeze()
}

/// A ConverseStream event as one AWS event-stream message.
pub(crate) fn converse_frame(event_type: &str, v: &Value) -> Bytes {
    antirion_wire::eventstream::encode(
        &[
            (":event-type", event_type),
            (":content-type", "application/json"),
            (":message-type", "event"),
        ],
        &compact(v),
    )
}

/// A named event whose `event:` line is the JSON object's `type`.
pub(crate) fn named(v: &Value) -> Bytes {
    let kind = v.get("type").and_then(Value::as_str).unwrap_or("message");
    frame(Some(kind), &compact(v))
}

/// The bytes of a forwarded event: the upstream's own, or the event with `model` replaced (then the
/// second value is true).
pub(super) fn forward(setup: &Setup, ev: &UpEvent, json: Option<&Value>) -> (Bytes, bool) {
    let wanted = match (&setup.rewrite_model, setup.dialect) {
        (Some(m), StreamDialect::Chat) => m,
        (Some(m), StreamDialect::Anthropic | StreamDialect::Responses) => {
            let mut j = json.cloned().unwrap_or(Value::Null);
            return if nested_model(&mut j, m) {
                (named(&j), true)
            } else {
                (ev.raw.clone(), false)
            };
        }
        _ => return (ev.raw.clone(), false),
    };
    let current = json.and_then(|j| j.get("model")).and_then(Value::as_str);
    if current.is_none_or(|c| c == wanted) {
        return (ev.raw.clone(), false);
    }
    let data = Bytes::copy_from_slice(ev.data.trim().as_bytes());
    let Ok(sk) = skim(&data, &JsonLimits::default()) else {
        return (ev.raw.clone(), false);
    };
    let Some(member) = sk.member("model") else {
        return (ev.raw.clone(), false);
    };
    let mut splice = Splice::default();
    splice.replace_span(
        member.value.clone(),
        Bytes::from(compact(&Value::from(wanted.as_str()))),
    );
    (frame(None, &splice.apply(&data)), true)
}

/// Replaces the model an Anthropic `message_start` or a Responses event carries in its nested
/// object; whether anything changed.
pub(super) fn nested_model(event: &mut Value, wanted: &str) -> bool {
    for member in ["message", "response"] {
        if let Some(m) = event.get_mut(member).and_then(|o| o.get_mut("model"))
            && m.as_str().is_some_and(|c| c != wanted)
        {
            *m = Value::from(wanted);
            return true;
        }
    }
    false
}

/// A heartbeat: a comment line that dispatches nothing, or the native ping event. Gemini and
/// Bedrock clients get none.
pub(super) fn heartbeat(client: ClientDialect) -> Bytes {
    match client {
        ClientDialect::Anthropic => Bytes::from_static(b"event: ping\ndata: {\"type\":\"ping\"}\n\n"),
        ClientDialect::Chat | ClientDialect::Responses => Bytes::from_static(b": keepalive\n"),
        ClientDialect::Gemini | ClientDialect::Bedrock => Bytes::new(),
    }
}

/// The comment line buffer mode sends while it holds a stream (nothing in framings without
/// comments).
pub(super) fn buffer_beat(client: ClientDialect) -> Bytes {
    match client {
        ClientDialect::Gemini | ClientDialect::Bedrock => Bytes::new(),
        _ => Bytes::from_static(b": keep-alive\n\n"),
    }
}

/// The canonical Gemini status name of an HTTP status.
fn gemini_status(status: u16) -> &'static str {
    match status {
        400 => "INVALID_ARGUMENT",
        401 => "UNAUTHENTICATED",
        403 => "PERMISSION_DENIED",
        404 => "NOT_FOUND",
        429 => "RESOURCE_EXHAUSTED",
        500 => "INTERNAL",
        502 | 503 => "UNAVAILABLE",
        504 => "DEADLINE_EXCEEDED",
        _ => "UNKNOWN",
    }
}

/// The Bedrock exception an in-stream failure is reported as.
fn bedrock_exception(code: Code, invoke: bool) -> &'static str {
    match code {
        Code::UpstreamRateLimited => "throttlingException",
        Code::UpstreamTimeout if invoke => "modelTimeoutException",
        Code::UpstreamRejected | Code::GuardrailBlocked => "validationException",
        Code::Draining => "serviceUnavailableException",
        Code::Internal => "internalServerException",
        _ => "modelStreamErrorException",
    }
}

/// The usage chunk of a Chat stream.
pub(crate) fn usage_chunk(facts: &StreamFacts, model: Option<&str>, usage: Usage) -> Bytes {
    let input = usage.input.unwrap_or(0);
    let output = usage.output.unwrap_or(0);
    let v = json!({
        "id": facts.id.as_deref().unwrap_or("chatcmpl-antirion"),
        "object": "chat.completion.chunk",
        "created": facts.created.unwrap_or(0),
        "model": model.or(facts.model.as_deref()).unwrap_or(""),
        "choices": [],
        "usage": {"prompt_tokens": input, "completion_tokens": output, "total_tokens": input.saturating_add(output)},
    });
    frame(None, &compact(&v))
}

/// The one error event that ends a failed stream.
pub(super) fn error_event(
    setup: &Setup,
    err: &GatewayError,
    stream_end: &str,
    upstream_type: Option<&str>,
    seq: Option<i64>,
) -> Bytes {
    let mut a = antirion_object(err, &setup.request_id);
    if let Some(obj) = a.as_object_mut() {
        obj.insert("stream_end".into(), Value::from(stream_end));
        obj.insert(
            "upstream_type".into(),
            upstream_type.map_or(Value::Null, Value::from),
        );
    }
    let message = err.message.as_str();
    match setup.client {
        ClientDialect::Anthropic => {
            let kind = match upstream_type {
                Some(t) if stream_end == "upstream_error" => t,
                _ if err.code == Code::UpstreamRateLimited => "rate_limit_error",
                _ if err.code == Code::Draining => "overloaded_error",
                _ if err.code == Code::GuardrailBlocked => "invalid_request_error",
                _ => "api_error",
            };
            let v = json!({"type": "error", "error": {"type": kind, "message": message}, "antirion": a});
            frame(Some("error"), &compact(&v))
        }
        ClientDialect::Chat => {
            let kind = match upstream_type {
                Some(t) if stream_end == "upstream_error" => t,
                _ if err.code == Code::UpstreamRateLimited => "rate_limit_error",
                _ if err.code == Code::GuardrailBlocked => "invalid_request_error",
                _ => "server_error",
            };
            let v = json!({
                "error": {"message": message, "type": kind, "param": null, "code": stream_end},
                "antirion": a,
            });
            frame(None, &compact(&v))
        }
        ClientDialect::Responses => {
            let v = json!({
                "type": "error",
                "code": stream_end,
                "message": message,
                "param": null,
                "sequence_number": seq.unwrap_or(0),
                "antirion": a,
            });
            frame(Some("error"), &compact(&v))
        }
        ClientDialect::Gemini => {
            let status = err.status();
            let v = json!({"error": {"code": status, "message": message, "status": gemini_status(status)}, "antirion": a});
            let mut out = BytesMut::new();
            out.put_slice(b"data: ");
            out.put_slice(&compact(&v));
            out.put_slice(b"\r\n\r\n");
            out.freeze()
        }
        ClientDialect::Bedrock => {
            let invoke = setup.dialect != StreamDialect::Converse;
            let v = json!({"message": message, "antirion": a});
            antirion_wire::eventstream::encode(
                &[
                    (":exception-type", bedrock_exception(err.code, invoke)),
                    (":content-type", "application/json"),
                    (":message-type", "exception"),
                ],
                &compact(&v),
            )
        }
    }
}

/// The events of a Chat or Completions stream rebuilt from a complete response, without the final
/// usage chunk and `[DONE]` (the relay adds those); `None` when the body is not a complete response.
pub(crate) fn synthesize(
    body: &Value,
    model: Option<&str>,
    client_wants_usage: bool,
    facts: &mut StreamFacts,
) -> Option<Vec<Bytes>> {
    let choices = body.get("choices")?.as_array().filter(|c| !c.is_empty())?;
    let completion = body.get("object").and_then(Value::as_str) == Some("text_completion");
    let id = body
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("chatcmpl-antirion")
        .to_owned();
    let created = body.get("created").and_then(Value::as_i64).unwrap_or(0);
    let model = model
        .map(str::to_owned)
        .or_else(|| body.get("model").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    let object = if completion {
        "text_completion"
    } else {
        "chat.completion.chunk"
    };
    let chunk = |choice: Value| {
        frame(
            None,
            &compact(
                &json!({"id": id, "object": object, "created": created, "model": model, "choices": [choice]}),
            ),
        )
    };
    let mut out = Vec::new();
    for (i, choice) in choices.iter().enumerate() {
        let index = choice.get("index").and_then(Value::as_u64).unwrap_or(i as u64);
        let finish = choice
            .get("finish_reason")
            .cloned()
            .unwrap_or(Value::from("stop"));
        if completion {
            let text = choice.get("text")?.as_str()?;
            out.push(chunk(
                json!({"index": index, "text": text, "finish_reason": null}),
            ));
            out.push(chunk(
                json!({"index": index, "text": "", "finish_reason": finish}),
            ));
            facts.output_chars += text.chars().count() as u64;
        } else {
            let message = choice.get("message")?.as_object()?;
            let mut delta = serde_json::Map::new();
            delta.insert("role".into(), Value::from("assistant"));
            for name in ["content", "reasoning_content", "refusal"] {
                if let Some(v) = message.get(name).filter(|v| !v.is_null()) {
                    if let Some(s) = v.as_str() {
                        facts.output_chars += s.chars().count() as u64;
                    }
                    delta.insert(name.into(), v.clone());
                }
            }
            if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                let calls: Vec<Value> = calls
                    .iter()
                    .enumerate()
                    .map(|(n, c)| {
                        let mut c = c.clone();
                        if let Some(o) = c.as_object_mut() {
                            o.insert("index".into(), Value::from(n));
                        }
                        c
                    })
                    .collect();
                delta.insert("tool_calls".into(), Value::Array(calls));
            }
            out.push(chunk(
                json!({"index": index, "delta": delta, "finish_reason": null}),
            ));
            out.push(chunk(
                json!({"index": index, "delta": {}, "finish_reason": finish}),
            ));
        }
        facts.choices.insert(index, true);
        if let Some(f) = finish.as_str() {
            facts.finish_reasons.push(f.to_owned());
        }
    }
    facts.id = Some(id.clone());
    facts.created = Some(created);
    facts.model = Some(model.clone());
    if let Some(u) = body.get("usage").filter(|u| !u.is_null()) {
        facts.usage.merge(Usage::openai(u));
        if client_wants_usage && !completion {
            facts.usage_chunk_seen = true;
            out.push(frame(
                None,
                &compact(&json!({"id": id, "object": object, "created": created, "model": model, "choices": [], "usage": u})),
            ));
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthesizes_a_chat_stream() {
        let body = json!({"id": "c1", "object": "chat.completion", "created": 1, "model": "up",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "hi"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4}});
        let mut facts = StreamFacts::default();
        let out = synthesize(&body, Some("m"), true, &mut facts).unwrap();
        assert_eq!(out.len(), 3);
        assert!(facts.all_finished());
        assert!(facts.usage_chunk_seen);
        let first: Value = serde_json::from_slice(&out[0][6..out[0].len() - 2]).unwrap();
        assert_eq!(first["model"], "m");
        assert_eq!(first["choices"][0]["delta"]["content"], "hi");
        assert!(synthesize(&json!({"id": "x"}), None, false, &mut StreamFacts::default()).is_none());
    }
}
