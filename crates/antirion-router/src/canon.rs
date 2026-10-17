//! Canonical JSON (members sorted by key, no whitespace) and the text of message contents, for the
//! hashes that must not depend on how a client ordered its members.

use serde_json::Value;

/// Writes `v` as canonical JSON into `out`.
pub fn write(v: &Value, out: &mut String) {
    match v {
        Value::Object(o) => {
            let mut keys: Vec<&String> = o.keys().collect();
            keys.sort_unstable();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(k.clone()).to_string());
                out.push(':');
                if let Some(x) = o.get(k) {
                    write(x, out);
                }
            }
            out.push('}');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(x, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// `v` as canonical JSON.
#[must_use]
pub fn canonical(v: &Value) -> String {
    let mut out = String::new();
    write(v, &mut out);
    out
}

/// The text of a message `content`: a string, or its text parts joined; other parts as canonical
/// JSON, so two contents that differ only in media still differ.
#[must_use]
pub fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => {
            let mut out = String::new();
            for p in parts {
                let text = p.get("text").and_then(Value::as_str);
                match (p.get("type").and_then(Value::as_str), text) {
                    (Some("text" | "input_text" | "output_text") | None, Some(t)) => out.push_str(t),
                    _ => write(p, &mut out),
                }
            }
            out
        }
        Value::Null => String::new(),
        other => canonical(other),
    }
}

/// The system or developer text of a request in any dialect.
#[must_use]
pub fn system_text(body: &Value) -> String {
    let mut out = String::new();
    if let Some(s) = body.get("system") {
        out.push_str(&content_text(s));
    }
    if let Some(s) = body.get("instructions").and_then(Value::as_str) {
        out.push_str(s);
    }
    if let Some(s) = body.get("systemInstruction") {
        out.push_str(&content_text(s.get("parts").unwrap_or(s)));
    }
    for m in messages(body) {
        if matches!(
            m.get("role").and_then(Value::as_str),
            Some("system" | "developer")
        ) {
            out.push_str(&content_text(m.get("content").unwrap_or(&Value::Null)));
        }
    }
    out
}

/// The messages of a request in any dialect: `messages`, a Responses `input` list or Gemini
/// `contents` (whose `parts` stand for the content).
#[must_use]
pub fn messages(body: &Value) -> Vec<&Value> {
    body.get("messages")
        .and_then(Value::as_array)
        .or_else(|| body.get("input").and_then(Value::as_array))
        .or_else(|| body.get("contents").and_then(Value::as_array))
        .map(|l| l.iter().collect())
        .unwrap_or_default()
}

/// The content of a message: `content`, or Gemini's `parts`.
#[must_use]
pub fn message_content(m: &Value) -> &Value {
    m.get("content")
        .or_else(|| m.get("parts"))
        .unwrap_or(&Value::Null)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn members_are_sorted_at_every_depth() {
        assert_eq!(
            canonical(&json!({"b": 1, "a": {"d": [1, {"z": 0, "y": "x"}], "c": null}})),
            r#"{"a":{"c":null,"d":[1,{"y":"x","z":0}]},"b":1}"#
        );
    }

    #[test]
    fn system_text_reads_every_dialect() {
        assert_eq!(
            system_text(
                &json!({"messages": [{"role": "system", "content": "s"}, {"role": "user", "content": "u"}]})
            ),
            "s"
        );
        assert_eq!(
            system_text(&json!({"system": [{"type": "text", "text": "a"}], "messages": []})),
            "a"
        );
        assert_eq!(system_text(&json!({"instructions": "i", "input": "x"})), "i");
    }
}
