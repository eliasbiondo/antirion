//! Provider prompt-cache breakpoints: `cache_control` markers placed on an upstream Messages body
//! (tools, then system, then one message), each only when the prompt prefix it closes is long enough.

use serde_json::{Map, Value, json};

use crate::policy::{PromptCaching, Target};

/// The provider's limit on breakpoints per request.
const MAX_BREAKPOINTS: usize = 4;

/// Whether a request body already carries a caller marker where callers place them.
#[must_use]
pub fn has_caller_marker(body: &Value) -> bool {
    let marked = |v: &Value| v.get("cache_control").is_some();
    let list = |name: &str| body.get(name).and_then(Value::as_array).into_iter().flatten();
    let in_items = |name: &str| {
        list(name).any(|m| {
            marked(m)
                || m.get("content")
                    .and_then(Value::as_array)
                    .is_some_and(|c| c.iter().any(marked))
        })
    };
    list("system").any(marked) || list("tools").any(marked) || in_items("messages") || in_items("input")
}

fn text_len(v: &Value) -> usize {
    serde_json::to_string(v).map_or(0, |s| s.chars().count())
}

/// Places breakpoints on `body` as `pc` says; `estimate` turns a prefix length in characters into
/// tokens. Returns how many were placed.
pub fn inject(body: &mut Value, pc: &PromptCaching, estimate: impl Fn(usize) -> u64) -> usize {
    let marker = if pc.one_hour {
        json!({"type": "ephemeral", "ttl": "1h"})
    } else {
        json!({"type": "ephemeral"})
    };
    let wants = |t: Target| pc.targets.contains(&t);
    let long_enough = |chars: usize| estimate(chars) >= pc.min_prefix_tokens;
    let Some(o) = body.as_object_mut() else {
        return 0;
    };
    let mut placed = 0;
    let tools_len: usize = o
        .get("tools")
        .and_then(Value::as_array)
        .map_or(0, |t| t.iter().map(text_len).sum());
    if wants(Target::Tools)
        && long_enough(tools_len)
        && let Some(last) = o
            .get_mut("tools")
            .and_then(Value::as_array_mut)
            .and_then(|t| t.last_mut())
            .and_then(Value::as_object_mut)
    {
        last.insert("cache_control".into(), marker.clone());
        placed += 1;
    }
    let system_len: usize = match o.get("system") {
        Some(Value::Array(s)) => s.iter().map(text_len).sum(),
        Some(s @ Value::String(_)) => text_len(s),
        _ => 0,
    };
    let mark_system =
        wants(Target::System) && placed < MAX_BREAKPOINTS && long_enough(tools_len + system_len);
    if mark_system && let Some(Value::String(s)) = o.get("system") {
        let block = json!([{"type": "text", "text": s}]);
        o.insert("system".into(), block);
    }
    if mark_system
        && let Some(last) = o
            .get_mut("system")
            .and_then(Value::as_array_mut)
            .and_then(|s| s.last_mut())
            .and_then(Value::as_object_mut)
    {
        last.insert("cache_control".into(), marker.clone());
        placed += 1;
    }
    if wants(Target::Messages) && placed < MAX_BREAKPOINTS {
        let messages = o.get("messages").and_then(Value::as_array);
        let count = messages.map_or(0, Vec::len);
        let offset = usize::try_from(pc.message_offset).unwrap_or(usize::MAX);
        if let Some(index) = count.checked_sub(1).and_then(|l| l.checked_sub(offset)) {
            let prefix: usize = messages.into_iter().flatten().take(index + 1).map(text_len).sum();
            if long_enough(tools_len + system_len + prefix)
                && let Some(message) = o
                    .get_mut("messages")
                    .and_then(Value::as_array_mut)
                    .and_then(|m| m.get_mut(index))
                    .and_then(Value::as_object_mut)
                && mark_last_block(message, &marker)
            {
                placed += 1;
            }
        }
    }
    placed
}

/// Marks the last content block of a message, turning string content into one text block.
fn mark_last_block(message: &mut Map<String, Value>, marker: &Value) -> bool {
    if let Some(Value::String(s)) = message.get("content") {
        let block = json!([{"type": "text", "text": s}]);
        message.insert("content".into(), block);
    }
    match message
        .get_mut("content")
        .and_then(Value::as_array_mut)
        .and_then(|c| c.last_mut())
        .and_then(Value::as_object_mut)
    {
        Some(last) => {
            last.insert("cache_control".into(), marker.clone());
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pc(targets: &[Target], min: u64, offset: u64) -> PromptCaching {
        PromptCaching {
            auto_inject: true,
            targets: targets.to_vec(),
            min_prefix_tokens: min,
            message_offset: offset,
            one_hour: false,
            openai_cache_key: false,
        }
    }

    fn quarter(chars: usize) -> u64 {
        u64::try_from(chars.div_ceil(4)).unwrap_or(u64::MAX)
    }

    #[test]
    fn places_tools_system_and_message_markers() {
        let long = "w ".repeat(3000);
        let mut body = json!({"tools": [{"name": "a", "description": long}, {"name": "b"}],
            "system": "be brief",
            "messages": [{"role": "user", "content": "q1"}, {"role": "assistant", "content": [{"type": "text", "text": "a"}]},
                         {"role": "user", "content": [{"type": "text", "text": "x"}, {"type": "text", "text": "y"}]}]});
        let all = [Target::Tools, Target::System, Target::Messages];
        assert_eq!(inject(&mut body, &pc(&all, 1024, 0), quarter), 3);
        assert_eq!(body["tools"][1]["cache_control"], json!({"type": "ephemeral"}));
        assert_eq!(
            body["system"],
            json!([{"type": "text", "text": "be brief", "cache_control": {"type": "ephemeral"}}])
        );
        assert!(body["messages"][2]["content"][1].get("cache_control").is_some());
        assert!(body["messages"][2]["content"][0].get("cache_control").is_none());
    }

    #[test]
    fn short_prefixes_and_offsets_place_nothing() {
        let mut body = json!({"system": "short", "messages": [{"role": "user", "content": "q"}]});
        assert_eq!(inject(&mut body, &pc(&[Target::System], 1024, 0), quarter), 0);
        assert_eq!(body["system"], json!("short"));
        assert_eq!(inject(&mut body, &pc(&[Target::Messages], 0, 1), quarter), 0);
        assert_eq!(inject(&mut body, &pc(&[Target::Messages], 0, 0), quarter), 1);
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"],
            json!({"type": "ephemeral"})
        );
    }

    #[test]
    fn caller_markers_are_found_where_callers_place_them() {
        assert!(has_caller_marker(
            &json!({"messages": [{"content": [{"cache_control": {}}]}]})
        ));
        assert!(has_caller_marker(&json!({"system": [{"cache_control": {}}]})));
        assert!(!has_caller_marker(
            &json!({"tools": [{"input_schema": {"cache_control": {}}}]})
        ));
    }
}
