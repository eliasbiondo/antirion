//! Encrypted reasoning and thinking signatures a request replays from earlier turns: Responses
//! `reasoning` items with `encrypted_content`, Anthropic `thinking` and `redacted_thinking`
//! blocks, Chat `reasoning_details` and Gemini thought signatures on tool calls, Gemini
//! `thoughtSignature` parts and Converse `reasoningContent` blocks. When an upstream rejects them,
//! the one repair retry sends the request without them.

use serde_json::{Map, Value};

use crate::parse::Parsed;

fn is_replayed(item: &Value) -> bool {
    match item.get("type").and_then(Value::as_str) {
        Some("reasoning") => item.get("encrypted_content").is_some_and(|c| !c.is_null()),
        Some("thinking" | "redacted_thinking") => true,
        _ => item.get("reasoningContent").is_some(),
    }
}

/// The message lists of a request body, in every dialect's shape.
fn lists(body: &mut Map<String, Value>) -> impl Iterator<Item = &mut Vec<Value>> {
    body.iter_mut()
        .filter(|(k, _)| matches!(k.as_str(), "input" | "messages" | "contents"))
        .filter_map(|(_, v)| v.as_array_mut())
}

/// Removes the replayed reasoning of one message (or Responses input item); whether any was.
fn strip_message(m: &mut Value) -> bool {
    let Some(o) = m.as_object_mut() else {
        return false;
    };
    let mut removed = o.remove("reasoning_details").is_some();
    removed |= o.remove("reasoning_content").is_some();
    for calls in o.get_mut("tool_calls").and_then(Value::as_array_mut).into_iter() {
        for c in calls.iter_mut() {
            if let Some(google) = c
                .pointer_mut("/extra_content/google")
                .and_then(Value::as_object_mut)
            {
                removed |= google.remove("thought_signature").is_some();
            }
        }
    }
    for key in ["content", "parts"] {
        let Some(parts) = o.get_mut(key).and_then(Value::as_array_mut) else {
            continue;
        };
        let before = parts.len();
        parts.retain(|p| !is_replayed(p));
        removed |= parts.len() != before;
        for p in parts.iter_mut() {
            if let Some(po) = p.as_object_mut() {
                removed |= po.remove("thoughtSignature").is_some();
                removed |= po.remove("thought_signature").is_some();
            }
        }
    }
    removed
}

/// The body without replayed reasoning; `None` when it carries none.
pub(crate) fn stripped(body: &Value) -> Option<Value> {
    let mut v = body.clone();
    let o = v.as_object_mut()?;
    let mut removed = false;
    for list in lists(o) {
        let before = list.len();
        list.retain(|item| !is_replayed(item));
        removed |= list.len() != before;
        list.retain_mut(|m| {
            if !strip_message(m) {
                return true;
            }
            removed = true;
            m.get("content")
                .or_else(|| m.get("parts"))
                .and_then(Value::as_array)
                .is_none_or(|a| !a.is_empty())
        });
    }
    removed.then_some(v)
}

/// Whether the request replays encrypted reasoning or thinking signatures.
pub(crate) fn carries(parsed: &Parsed) -> bool {
    parsed.doc().is_some_and(|b| stripped(b).is_some())
}

/// The request without its replayed reasoning, when it carries any.
pub(crate) fn repaired(parsed: &Parsed) -> Option<Parsed> {
    let v = stripped(parsed.doc()?)?;
    let raw = bytes::Bytes::from(serde_json::to_vec(&v).ok()?);
    let skim = antirion_json::skim(&raw, &antirion_json::JsonLimits::default()).ok()?;
    Some(Parsed::json(raw, skim, Some(v)))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn responses_reasoning_items_are_removed() {
        let body = json!({"model": "m", "input": [
            {"role": "user", "content": "hi"},
            {"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "gAAA"},
            {"role": "assistant", "content": [{"type": "output_text", "text": "ok"}]}
        ]});
        let out = stripped(&body).unwrap();
        assert_eq!(out["input"].as_array().unwrap().len(), 2);
        assert!(!out.to_string().contains("encrypted_content"));
        assert!(stripped(&json!({"input": "plain"})).is_none());
    }

    #[test]
    fn thinking_blocks_and_signatures_are_removed() {
        let body = json!({"messages": [
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "x", "signature": "sig"},
                {"type": "text", "text": "answer"}
            ]},
            {"role": "assistant", "content": [{"type": "redacted_thinking", "data": "d"}]}
        ]});
        let out = stripped(&body).unwrap();
        let msgs = out["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2, "a message left empty is dropped");
        assert_eq!(msgs[1]["content"], json!([{"type": "text", "text": "answer"}]));
        let gemini = json!({"contents": [{"role": "model", "parts": [
            {"functionCall": {"name": "f"}, "thoughtSignature": "s"}
        ]}]});
        assert!(
            !stripped(&gemini)
                .unwrap()
                .to_string()
                .contains("thoughtSignature")
        );
    }
}
