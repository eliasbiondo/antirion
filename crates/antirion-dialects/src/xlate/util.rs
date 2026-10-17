//! Small helpers shared by the codecs: JSON access, tool-argument text, hashes and base64.

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

pub(crate) type Obj = Map<String, Value>;

/// The string member `k` of `v`.
pub(crate) fn str_of<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

/// A JSON object built from pairs, keeping their order; `Null` values are left out.
pub(crate) fn obj<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    let mut m = Map::new();
    for (k, v) in pairs {
        if !v.is_null() {
            m.insert(k.to_owned(), v);
        }
    }
    Value::Object(m)
}

/// Inserts `v` under `k` unless it is `Null`.
pub(crate) fn put(m: &mut Obj, k: &str, v: Value) {
    if !v.is_null() {
        m.insert(k.to_owned(), v);
    }
}

/// Compact JSON text: no insignificant whitespace, members in order, non-ASCII unescaped.
pub(crate) fn compact(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

/// Tool-call arguments text as an object: `""` is `{}`; `None` when the text is not a JSON object.
pub(crate) fn parse_args(raw: &str) -> Option<Value> {
    if raw.trim().is_empty() {
        return Some(Value::Object(Map::new()));
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(v @ Value::Object(_)) => Some(v),
        _ => None,
    }
}

/// Lowercase hex SHA-256 of `s`.
pub(crate) fn sha_hex(s: &str) -> String {
    let digest = Sha256::digest(s.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The first `n` hex characters of SHA-256 of `s`.
pub(crate) fn sha_prefix(s: &str, n: usize) -> String {
    sha_hex(s).chars().take(n).collect()
}

/// Decodes standard base64 (padding optional, whitespace not allowed).
pub(crate) fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let trimmed = s.trim_end_matches('=');
    let padded = format!("{trimmed}{}", "=".repeat((4 - trimmed.len() % 4) % 4));
    STANDARD.decode(padded.as_bytes()).ok()
}

pub(crate) fn b64_encode(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

pub(crate) fn b64url_encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub(crate) fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(s.trim_end_matches('=').as_bytes()).ok()
}

/// A base64 payload as UTF-8 text (plain-text documents).
pub(crate) fn text_payload(data: &str) -> Option<String> {
    String::from_utf8(b64_decode(data)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_keep_order_and_unicode() {
        let v = parse_args(r#"{"b":1,"a":"é","c":{"z":[1,2],"y":null}}"#).unwrap();
        assert_eq!(compact(&v), r#"{"b":1,"a":"é","c":{"z":[1,2],"y":null}}"#);
        assert_eq!(parse_args(""), Some(serde_json::json!({})));
        assert_eq!(parse_args("[1,2]"), None);
        assert_eq!(parse_args("not json"), None);
    }

    #[test]
    fn hashes_and_base64() {
        assert_eq!(sha_prefix("a.b", 8).len(), 8);
        assert_eq!(b64_decode("aGk"), Some(b"hi".to_vec()));
        assert_eq!(b64_decode("@@@"), None);
        assert_eq!(b64url_decode(&b64url_encode(b"x+/")), Some(b"x+/".to_vec()));
    }
}
