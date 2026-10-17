//! Continuation turns: requests that carry on a turn the gateway already routed (a tool result, a
//! function call output, an assistant turn, or a user turn with no text and no attachment), in any
//! dialect.

use serde_json::Value;

/// What one content part of a user turn holds.
enum Part {
    Text(bool),
    ToolResult,
    Attachment,
}

fn part(p: &Value) -> Part {
    if let Some(t) = p.get("text").and_then(Value::as_str) {
        return Part::Text(!t.trim().is_empty());
    }
    if p.get("functionResponse").is_some() {
        return Part::ToolResult;
    }
    match p.get("type").and_then(Value::as_str) {
        Some("tool_result" | "function_call_output" | "tool_call_output") => Part::ToolResult,
        Some("text" | "input_text") => Part::Text(false),
        _ => Part::Attachment,
    }
}

/// A user turn is a continuation when it has no text and no attachment.
fn user_continues(content: &Value) -> bool {
    match content {
        Value::String(s) => s.trim().is_empty(),
        Value::Array(parts) => parts.iter().all(|p| match part(p) {
            Part::Text(non_empty) => !non_empty,
            Part::ToolResult => true,
            Part::Attachment => false,
        }),
        Value::Null => true,
        _ => false,
    }
}

/// Whether the request's last message continues an earlier routed turn.
#[must_use]
pub fn is_continuation(body: &Value) -> bool {
    let list = body
        .get("messages")
        .and_then(Value::as_array)
        .or_else(|| body.get("input").and_then(Value::as_array))
        .or_else(|| body.get("contents").and_then(Value::as_array));
    let Some(last) = list.and_then(|l| l.last()) else {
        return body
            .get("input")
            .and_then(Value::as_str)
            .is_some_and(|s| s.trim().is_empty());
    };
    if let Some(
        "function_call"
        | "function_call_output"
        | "custom_tool_call"
        | "custom_tool_call_output"
        | "reasoning",
    ) = last.get("type").and_then(Value::as_str)
    {
        return true;
    }
    match last.get("role").and_then(Value::as_str) {
        Some("tool" | "function" | "assistant" | "model") => true,
        Some("user") | None => user_continues(
            last.get("content")
                .or_else(|| last.get("parts"))
                .unwrap_or(&Value::Null),
        ),
        Some(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn tool_results_and_assistant_turns_continue() {
        assert!(is_continuation(
            &json!({"messages": [{"role": "user", "content": "hi"},
            {"role": "assistant", "tool_calls": []}, {"role": "tool", "content": "42"}]})
        ));
        assert!(is_continuation(
            &json!({"messages": [{"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "t", "content": "42"}]}]})
        ));
        assert!(is_continuation(
            &json!({"input": [{"role": "user", "content": "hi"},
            {"type": "function_call_output", "call_id": "c", "output": "42"}]})
        ));
        assert!(is_continuation(
            &json!({"messages": [{"role": "user", "content": "   "}]})
        ));
        assert!(is_continuation(
            &json!({"contents": [{"role": "user", "parts": [{"functionResponse": {}}]}]})
        ));
    }

    #[test]
    fn user_text_and_attachments_do_not() {
        assert!(!is_continuation(
            &json!({"messages": [{"role": "user", "content": "hi"}]})
        ));
        assert!(!is_continuation(
            &json!({"messages": [{"role": "user", "content": [
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,AA"}}]}]})
        ));
        assert!(!is_continuation(&json!({"input": "hello"})));
        assert!(!is_continuation(
            &json!({"messages": [{"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "t"}, {"type": "text", "text": "and now?"}]}]})
        ));
    }
}
