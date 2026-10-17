//! The non-streaming shape of a stored stream, for requests that hit it without streaming and for
//! the storability checks.

use antirion_cache::StoredEvent;
use serde_json::{Map, Value};

use crate::relay::assemble::Assembler;

/// A tool call whose arguments are not JSON: the stream is not storable.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct BrokenToolCall;

/// The assembled response of `endpoint_id`'s stream, when its dialect has one.
pub(super) fn assemble(endpoint_id: &str, events: &[StoredEvent]) -> Result<Option<Value>, BrokenToolCall> {
    let payloads = || {
        events
            .iter()
            .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
    };
    match endpoint_id {
        "chat_completions" => {
            let mut a = Assembler::default();
            for p in payloads() {
                a.push(&p);
            }
            Ok(Some(a.finish()))
        }
        "messages" => anthropic(payloads()).map(Some),
        "responses" => Ok(payloads()
            .filter(|p| p.get("type").and_then(Value::as_str) == Some("response.completed"))
            .find_map(|p| p.get("response").cloned())),
        _ => Ok(None),
    }
}

/// An Anthropic message from its events.
fn anthropic(payloads: impl Iterator<Item = Value>) -> Result<Value, BrokenToolCall> {
    let mut message = Map::new();
    let mut blocks: Vec<Value> = Vec::new();
    let mut partial: Vec<String> = Vec::new();
    for p in payloads {
        let index = p
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|i| usize::try_from(i).ok());
        match p.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                if let Some(m) = p.get("message").and_then(Value::as_object) {
                    message = m.clone();
                }
            }
            Some("content_block_start") => {
                if let (Some(i), Some(b)) = (index, p.get("content_block")) {
                    if blocks.len() <= i {
                        blocks.resize(i + 1, Value::Null);
                        partial.resize(i + 1, String::new());
                    }
                    if let Some(slot) = blocks.get_mut(i) {
                        *slot = b.clone();
                    }
                }
            }
            Some("content_block_delta") => {
                let (Some(i), Some(d)) = (index, p.get("delta")) else {
                    continue;
                };
                let Some(block) = blocks.get_mut(i).and_then(Value::as_object_mut) else {
                    continue;
                };
                let append = |block: &mut Map<String, Value>, field: &str, text: &str| {
                    let current = block.get(field).and_then(Value::as_str).unwrap_or_default();
                    let joined = format!("{current}{text}");
                    block.insert(field.into(), Value::from(joined));
                };
                match d.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        append(
                            block,
                            "text",
                            d.get("text").and_then(Value::as_str).unwrap_or_default(),
                        );
                    }
                    Some("thinking_delta") => append(
                        block,
                        "thinking",
                        d.get("thinking").and_then(Value::as_str).unwrap_or_default(),
                    ),
                    Some("signature_delta") => {
                        block.insert(
                            "signature".into(),
                            d.get("signature").cloned().unwrap_or(Value::Null),
                        );
                    }
                    Some("input_json_delta") => {
                        if let Some(s) = partial.get_mut(i) {
                            s.push_str(d.get("partial_json").and_then(Value::as_str).unwrap_or_default());
                        }
                    }
                    _ => {}
                }
            }
            Some("content_block_stop") => {
                let Some(i) = index else { continue };
                let text = partial.get(i).cloned().unwrap_or_default();
                if let Some(block) = blocks.get_mut(i).and_then(Value::as_object_mut)
                    && block.get("type").and_then(Value::as_str) == Some("tool_use")
                    && !text.is_empty()
                {
                    let input = serde_json::from_str::<Value>(&text).map_err(|_| BrokenToolCall)?;
                    block.insert("input".into(), input);
                }
            }
            Some("message_delta") => {
                if let Some(d) = p.get("delta").and_then(Value::as_object) {
                    for (k, v) in d {
                        message.insert(k.clone(), v.clone());
                    }
                }
                if let Some(u) = p.get("usage").and_then(Value::as_object) {
                    let mut usage = message
                        .get("usage")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    for (k, v) in u {
                        usage.insert(k.clone(), v.clone());
                    }
                    message.insert("usage".into(), Value::Object(usage));
                }
            }
            _ => {}
        }
    }
    message.insert(
        "content".into(),
        Value::Array(blocks.into_iter().filter(|b| !b.is_null()).collect()),
    );
    Ok(Value::Object(message))
}

/// The usage member of an assembled response, or the one a stream reported.
pub(super) fn usage(assembled: Option<&Value>, reported: Option<&Value>) -> Option<Value> {
    assembled
        .and_then(|v| v.get("usage"))
        .filter(|u| !u.is_null())
        .or(reported)
        .cloned()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn ev(data: Value) -> StoredEvent {
        StoredEvent {
            event: data.get("type").and_then(Value::as_str).map(str::to_owned),
            data: data.to_string(),
        }
    }

    #[test]
    fn anthropic_messages_are_assembled() {
        let events = [
            ev(
                json!({"type": "message_start", "message": {"id": "m", "role": "assistant", "content": [],
                "stop_reason": null, "usage": {"input_tokens": 5, "output_tokens": 1}}}),
            ),
            ev(
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
            ),
            ev(
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hel"}}),
            ),
            ev(
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "lo"}}),
            ),
            ev(json!({"type": "content_block_stop", "index": 0})),
            ev(
                json!({"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "t", "name": "f", "input": {}}}),
            ),
            ev(
                json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"a\":"}}),
            ),
            ev(
                json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "1}"}}),
            ),
            ev(json!({"type": "content_block_stop", "index": 1})),
            ev(
                json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 7}}),
            ),
            ev(json!({"type": "message_stop"})),
        ];
        let m = assemble("messages", &events).unwrap().unwrap();
        assert_eq!(m["content"][0]["text"], "Hello");
        assert_eq!(m["content"][1]["input"], json!({"a": 1}));
        assert_eq!(m["stop_reason"], "tool_use");
        assert_eq!(m["usage"], json!({"input_tokens": 5, "output_tokens": 7}));
        let broken = [
            ev(
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "input": {}}}),
            ),
            ev(
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"a\":"}}),
            ),
            ev(json!({"type": "content_block_stop", "index": 0})),
        ];
        assert_eq!(assemble("messages", &broken), Err(BrokenToolCall));
    }

    #[test]
    fn chat_streams_are_assembled() {
        let events = [
            ev(
                json!({"id": "c", "object": "chat.completion.chunk", "choices": [{"index": 0, "delta": {"content": "a"}, "finish_reason": null}]}),
            ),
            ev(
                json!({"id": "c", "object": "chat.completion.chunk", "choices": [{"index": 0, "delta": {"content": "b"}, "finish_reason": "stop"}]}),
            ),
        ];
        let v = assemble("chat_completions", &events).unwrap().unwrap();
        assert_eq!(v["choices"][0]["message"]["content"], "ab");
        assert_eq!(v["object"], "chat.completion");
    }
}
