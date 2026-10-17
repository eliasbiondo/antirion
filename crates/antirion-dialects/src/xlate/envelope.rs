//! The signature envelope `ant1.<family>.<provider or ->.<payload>`: how a reasoning payload travels
//! through a client field of another family, and is recognised when it comes back.

use super::ir::Family;
use super::util::{b64url_decode, b64url_encode};

const PREFIX: &str = "ant1.";

/// Wraps a payload; the provider is named for the `openai` family, whose scope is one provider.
#[must_use]
pub fn wrap(family: Family, provider: Option<&str>, payload: &str) -> String {
    let provider = provider
        .filter(|p| !p.is_empty() && !p.contains('.'))
        .unwrap_or("-");
    format!(
        "{PREFIX}{}.{provider}.{}",
        family.as_str(),
        b64url_encode(payload.as_bytes())
    )
}

/// An unwrapped envelope: family, provider (when named) and payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unwrapped {
    pub family: Family,
    pub provider: Option<String>,
    pub payload: String,
}

/// Unwraps a value that carries the envelope prefix; `None` for any other value.
#[must_use]
pub fn unwrap(value: &str) -> Option<Unwrapped> {
    let rest = value.strip_prefix(PREFIX)?;
    let (family, rest) = rest.split_once('.')?;
    let (provider, payload) = rest.split_once('.')?;
    let bytes = b64url_decode(payload)?;
    Some(Unwrapped {
        family: Family::parse(family),
        provider: (provider != "-").then(|| provider.to_owned()),
        payload: String::from_utf8(bytes).ok()?,
    })
}

/// Whether a value carries the envelope prefix (wrapped values that fail to parse are foreign).
#[must_use]
pub fn is_wrapped(value: &str) -> bool {
    value.starts_with(PREFIX)
}

/// Unwraps the envelopes in an Anthropic body's history: Anthropic payloads get their own value
/// back, other families' reasoning blocks are removed. Whether anything changed.
pub fn unwrap_anthropic_history(body: &mut serde_json::Value) -> bool {
    let mut changed = false;
    let messages = body.get_mut("messages").and_then(serde_json::Value::as_array_mut);
    for m in messages.into_iter().flatten() {
        let Some(blocks) = m.get_mut("content").and_then(serde_json::Value::as_array_mut) else {
            continue;
        };
        blocks.retain_mut(|b| {
            let field = match b.get("type").and_then(serde_json::Value::as_str) {
                Some("thinking") => "signature",
                Some("redacted_thinking") => "data",
                _ => return true,
            };
            let Some(value) = b
                .get(field)
                .and_then(serde_json::Value::as_str)
                .filter(|v| is_wrapped(v))
            else {
                return true;
            };
            changed = true;
            match unwrap(value) {
                Some(u) if u.family == Family::Anthropic => {
                    if let Some(o) = b.as_object_mut() {
                        o.insert(field.to_owned(), u.payload.into());
                    }
                    true
                }
                _ => false,
            }
        });
    }
    changed
}

/// Unwraps the envelopes in a Gemini body's history: Gemini thought signatures get their own value
/// back; other families' signatures are removed, with the thought parts that carried them. Whether
/// anything changed.
pub fn unwrap_gemini_history(body: &mut serde_json::Value) -> bool {
    let mut changed = false;
    let contents = body.get_mut("contents").and_then(serde_json::Value::as_array_mut);
    for c in contents.into_iter().flatten() {
        let Some(parts) = c.get_mut("parts").and_then(serde_json::Value::as_array_mut) else {
            continue;
        };
        parts.retain_mut(|p| {
            let Some(o) = p.as_object_mut() else { return true };
            let Some(value) = o
                .get("thoughtSignature")
                .and_then(serde_json::Value::as_str)
                .filter(|v| is_wrapped(v))
            else {
                return true;
            };
            changed = true;
            match unwrap(value) {
                Some(u) if u.family == Family::Gemini => {
                    o.insert("thoughtSignature".to_owned(), u.payload.into());
                    true
                }
                _ => {
                    o.remove("thoughtSignature");
                    o.get("thought") != Some(&serde_json::Value::Bool(true))
                }
            }
        });
    }
    changed
}

/// Unwraps the envelopes in a Converse body's history: Anthropic signatures get their own value
/// back, other families' reasoning blocks are removed. Whether anything changed.
pub fn unwrap_converse_history(body: &mut serde_json::Value) -> bool {
    let mut changed = false;
    let messages = body.get_mut("messages").and_then(serde_json::Value::as_array_mut);
    for m in messages.into_iter().flatten() {
        let Some(blocks) = m.get_mut("content").and_then(serde_json::Value::as_array_mut) else {
            continue;
        };
        blocks.retain_mut(|b| {
            let Some(text) = b.pointer_mut("/reasoningContent/reasoningText") else {
                return true;
            };
            let Some(value) = text
                .get("signature")
                .and_then(serde_json::Value::as_str)
                .filter(|v| is_wrapped(v))
            else {
                return true;
            };
            changed = true;
            match unwrap(value) {
                Some(u) if u.family == Family::Anthropic => {
                    if let Some(o) = text.as_object_mut() {
                        o.insert("signature".to_owned(), u.payload.into());
                    }
                    true
                }
                _ => false,
            }
        });
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_histories_unwrap_their_own_family() {
        let gemini_sig = wrap(Family::Gemini, None, "g");
        let other = wrap(Family::Anthropic, None, "a");
        let mut body = serde_json::json!({"contents": [{"role": "model", "parts": [
            {"text": "t", "thought": true, "thoughtSignature": other},
            {"functionCall": {"name": "f", "args": {}}, "thoughtSignature": gemini_sig}]}]});
        assert!(unwrap_gemini_history(&mut body));
        assert_eq!(
            body["contents"][0]["parts"],
            serde_json::json!([{"functionCall": {"name": "f", "args": {}}, "thoughtSignature": "g"}])
        );
        let mut body = serde_json::json!({"messages": [{"role": "assistant", "content": [
            {"reasoningContent": {"reasoningText": {"text": "x", "signature": wrap(Family::Anthropic, None, "s")}}},
            {"reasoningContent": {"reasoningText": {"text": "y", "signature": wrap(Family::OpenAi, Some("p"), "e")}}},
            {"text": "hi"}]}]});
        assert!(unwrap_converse_history(&mut body));
        assert_eq!(
            body["messages"][0]["content"],
            serde_json::json!([{"reasoningContent": {"reasoningText": {"text": "x", "signature": "s"}}}, {"text": "hi"}])
        );
    }

    #[test]
    fn round_trip() {
        let w = wrap(Family::OpenAi, Some("u"), "gAAAA+/=");
        assert!(w.starts_with("ant1.openai.u."));
        assert!(!w.contains('='));
        let u = unwrap(&w).unwrap();
        assert_eq!(
            (u.family, u.provider.as_deref(), u.payload.as_str()),
            (Family::OpenAi, Some("u"), "gAAAA+/=")
        );
        let a = unwrap(&wrap(Family::Anthropic, None, "sig")).unwrap();
        assert_eq!(a.provider, None);
        assert!(unwrap("EqQB").is_none());
    }
}
