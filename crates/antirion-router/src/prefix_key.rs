//! The implicit session value of prompt-prefix affinity: a hash of the system and developer text,
//! the tool definitions (canonical JSON) and the first user message, so it stays the same while a
//! conversation grows and changes when any of the three changes. Tenant and route are added by the
//! binding key.

use serde_json::Value;

use crate::canon;

fn part(buf: &mut Vec<u8>, p: &[u8]) {
    buf.extend_from_slice(&(p.len() as u64).to_be_bytes());
    buf.extend_from_slice(p);
}

/// The value, or `None` when the request has no user message.
#[must_use]
pub fn value(body: &Value) -> Option<String> {
    let first_user = canon::messages(body)
        .into_iter()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        .map(|m| canon::content_text(canon::message_content(m)))
        .or_else(|| body.get("input").and_then(Value::as_str).map(str::to_owned))?;
    let mut buf = Vec::new();
    part(&mut buf, canon::system_text(body).as_bytes());
    part(
        &mut buf,
        body.get("tools")
            .map(canon::canonical)
            .unwrap_or_default()
            .as_bytes(),
    );
    part(&mut buf, first_user.as_bytes());
    Some(format!("{:032x}", xxhash_rust::xxh3::xxh3_128(&buf)))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn stable_as_history_grows_and_sensitive_to_the_prefix() {
        let turn1 = json!({"messages": [{"role": "system", "content": "be brief"}, {"role": "user", "content": "hi"}]});
        let turn3 = json!({"messages": [{"role": "system", "content": "be brief"}, {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "hello"}, {"role": "user", "content": "more"}]});
        let other = json!({"messages": [{"role": "system", "content": "be long"}, {"role": "user", "content": "hi"}]});
        let tools_a = json!({"tools": [{"type": "function", "function": {"name": "f", "parameters": {"a": 1, "b": 2}}}],
            "messages": [{"role": "user", "content": "hi"}]});
        let tools_b = json!({"tools": [{"function": {"parameters": {"b": 2, "a": 1}, "name": "f"}, "type": "function"}],
            "messages": [{"role": "user", "content": "hi"}]});
        assert_eq!(value(&turn1), value(&turn3));
        assert_ne!(value(&turn1), value(&other));
        assert_eq!(
            value(&tools_a),
            value(&tools_b),
            "tool definitions compare as canonical JSON"
        );
        assert_eq!(value(&json!({"messages": []})), None);
    }
}
