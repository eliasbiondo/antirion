//! Writing a document as block-style YAML. Strings and keys are double-quoted with JSON escapes,
//! which YAML 1.2 reads back unchanged, so the output never depends on plain-scalar resolution.

use std::fmt::Write;

use serde_json::Value;

/// The YAML text of `v`.
#[must_use]
pub fn to_yaml(v: &Value) -> String {
    let mut out = String::new();
    match v {
        Value::Object(m) if !m.is_empty() => block_map(v, 0, &mut out),
        Value::Array(a) if !a.is_empty() => block_seq(v, 0, &mut out),
        other => {
            out.push_str(&scalar(other));
            out.push('\n');
        }
    }
    out
}

fn scalar(v: &Value) -> String {
    match v {
        Value::Object(_) => String::from("{}"),
        Value::Array(_) => String::from("[]"),
        Value::String(s) => quote(s),
        other => other.to_string(),
    }
}

fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| String::from("\"\""))
}

fn is_block(v: &Value) -> bool {
    match v {
        Value::Object(m) => !m.is_empty(),
        Value::Array(a) => !a.is_empty(),
        _ => false,
    }
}

fn block_map(v: &Value, indent: usize, out: &mut String) {
    let Value::Object(m) = v else { return };
    for (k, item) in m {
        let _ = write!(out, "{:indent$}{}:", "", quote(k));
        if is_block(item) {
            out.push('\n');
            match item {
                Value::Object(_) => block_map(item, indent + 2, out),
                _ => block_seq(item, indent + 2, out),
            }
        } else {
            let _ = writeln!(out, " {}", scalar(item));
        }
    }
}

fn block_seq(v: &Value, indent: usize, out: &mut String) {
    let Value::Array(a) = v else { return };
    for item in a {
        let _ = write!(out, "{:indent$}-", "");
        if is_block(item) {
            out.push('\n');
            match item {
                Value::Object(_) => block_map(item, indent + 2, out),
                _ => block_seq(item, indent + 2, out),
            }
        } else {
            let _ = writeln!(out, " {}", scalar(item));
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{Format, parse};

    #[test]
    fn round_trips_through_the_strict_loader() {
        let doc = json!({
            "version": 1,
            "providers": {"p": {"type": "openai", "headers": {"x": "a$$b"}, "list": [1, "on", {"k": null}, []]}},
            "empty": {},
            "weird key.x": "line\nbreak \"quoted\" yes"
        });
        let text = to_yaml(&doc);
        assert_eq!(parse(text.as_bytes(), Format::Yaml).unwrap(), doc, "{text}");
    }
}
