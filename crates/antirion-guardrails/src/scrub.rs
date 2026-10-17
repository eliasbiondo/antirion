//! The built-in PII and secret detectors over stored content: every string of a JSON value is
//! searched with every entity and detector, and the matches are replaced.

use serde_json::Value;

use crate::def::Replacement;
use crate::pattern::Span;

/// Every PII entity and secret in `text`, merged.
#[must_use]
pub fn sensitive(text: &str) -> Vec<Span> {
    let spans: Vec<Span> = crate::pii::ALL
        .iter()
        .flat_map(|e| e.spans(text))
        .chain(crate::secrets::ALL.iter().flat_map(|d| d.spans(text)))
        .collect();
    crate::redact::merge(spans)
}

/// Replaces every match in the strings of `v`; true when anything matched.
pub fn scrub(v: &mut Value, how: Replacement, hash_key: Option<&[u8]>) -> bool {
    match v {
        Value::String(s) => {
            let spans = sensitive(s);
            if spans.is_empty() {
                return false;
            }
            *s = crate::redact::apply(
                s,
                &spans,
                crate::redact::Style {
                    how,
                    hash_key,
                    keep: &[],
                },
            );
            true
        }
        Value::Array(items) => {
            let mut found = false;
            for i in items {
                found |= scrub(i, how, hash_key);
            }
            found
        }
        Value::Object(m) => {
            let mut found = false;
            for i in m.values_mut() {
                found |= scrub(i, how, hash_key);
            }
            found
        }
        _ => false,
    }
}

/// Whether any string of `v` holds a match.
#[must_use]
pub fn holds_sensitive(v: &Value) -> bool {
    match v {
        Value::String(s) => !sensitive(s).is_empty(),
        Value::Array(items) => items.iter().any(holds_sensitive),
        Value::Object(m) => m.values().any(holds_sensitive),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn strings_anywhere_in_a_value_are_scrubbed() {
        let mut v = json!({"messages": [{"role": "user", "content": "mail ada@example.com now"}], "n": 3});
        assert!(holds_sensitive(&v));
        assert!(scrub(&mut v, Replacement::Label, None));
        assert_eq!(v["messages"][0]["content"], "mail [EMAIL] now");
        assert!(!holds_sensitive(&v));
        let mut h = json!("key AKIAABCDEFGHIJKLMNOP here");
        assert!(scrub(&mut h, Replacement::Hash, Some(b"k")));
        assert!(h.as_str().unwrap().starts_with("key [AWS_ACCESS_KEY:"));
        assert!(!scrub(&mut json!({"a": "nothing"}), Replacement::Label, None));
    }
}
