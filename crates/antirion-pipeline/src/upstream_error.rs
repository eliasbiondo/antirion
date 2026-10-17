//! Upstream error bodies: the message the client sees, extracted from a known error shape and
//! scrubbed of credentials before it reaches any sink.

use serde_json::Value;

use crate::scrub::Scrub;

/// What an upstream error body says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Extracted {
    /// The scrubbed message, when the body is in a known error shape.
    pub(crate) message: Option<String>,
    /// The upstream's error `type`, when it gave one.
    pub(crate) kind: Option<String>,
    /// The upstream's error `code`, as a string.
    pub(crate) code: Option<String>,
    /// OpenAI `error.param`.
    pub(crate) param: Option<String>,
    /// A sentence describing a body in no known shape.
    pub(crate) fallback: String,
}

const MAX_MESSAGE: usize = 2000;

fn first_string<'a>(v: &'a Value, paths: &[&[&str]]) -> Option<&'a str> {
    paths.iter().find_map(|path| {
        path.iter()
            .try_fold(v, |cur, seg| cur.get(*seg))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    })
}

/// Extracts the message of an upstream error response.
pub(crate) fn extract(status: u16, content_type: Option<&str>, body: &[u8], scrub: &Scrub) -> Extracted {
    let mut out = Extracted::default();
    if let Ok(v) = serde_json::from_slice::<Value>(body) {
        let errors0 = v
            .get("errors")
            .and_then(|e| e.get(0))
            .cloned()
            .unwrap_or(Value::Null);
        let msg = first_string(
            &v,
            &[
                &["error", "message"],
                &["error"],
                &["message"],
                &["Message"],
                &["detail"],
                &["detail", "message"],
                &["msg"],
                &["error_msg"],
                &["response", "error", "message"],
            ],
        )
        .or_else(|| first_string(&errors0, &[&["message"]]))
        .map(str::to_owned)
        .or_else(|| {
            v.get("error")
                .and_then(|e| e.get("message"))
                .filter(|m| !m.is_null() && !m.is_string())
                .map(Value::to_string)
        });
        out.message = msg.map(|m| scrub.apply(&truncate(&m, MAX_MESSAGE)));
        let top_type = v.get("type").and_then(Value::as_str).filter(|t| *t != "error");
        out.kind = first_string(&v, &[&["error", "type"], &["error", "status"], &["__type"]])
            .or(top_type)
            .map(|k| k.split(':').next().unwrap_or(k).to_owned());
        out.code = match v.get("error").and_then(|e| e.get("code")) {
            Some(Value::String(c)) if !c.is_empty() => Some(c.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        };
        out.param = v
            .get("error")
            .and_then(|e| e.get("param"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        out.fallback = format!("Upstream returned HTTP {status} with an unrecognised error body");
        return out;
    }
    let plain = content_type.is_none_or(|ct| ct.trim().to_ascii_lowercase().starts_with("text/plain"));
    out.fallback = match std::str::from_utf8(body) {
        Ok(text) if plain && body.len() <= 4096 => {
            let cleaned: String = text
                .chars()
                .take(512)
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            scrub.apply(&format!("Upstream returned HTTP {status}: {cleaned}"))
        }
        _ => {
            let ct = content_type.unwrap_or("none");
            scrub.apply(&format!(
                "Upstream returned HTTP {status} with a non-JSON body ({ct})"
            ))
        }
    };
    out
}

fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s.get(..i).unwrap_or(s).to_owned(),
        None => s.to_owned(),
    }
}

/// The normalised reason of an upstream rejection, from structured codes first and the provider's
/// message patterns otherwise.
pub(crate) fn normalised_reason(ex: &Extracted) -> Option<antirion_error::Reason> {
    let code = ex.code.as_deref().unwrap_or_default();
    let kind = ex.kind.as_deref().unwrap_or_default();
    let message = ex.message.as_deref().unwrap_or_default().to_ascii_lowercase();
    if code == "context_length_exceeded"
        || (kind == "invalid_request_error" && message.starts_with("prompt is too long"))
        || (kind == "INVALID_ARGUMENT" && message.contains("exceeds the maximum number of tokens"))
        || (kind == "ValidationException"
            && (message.contains("input is too long") || message.contains("too many input tokens")))
    {
        return Some(crate::reasons::CONTEXT_LENGTH_EXCEEDED);
    }
    if matches!(code, "content_policy_violation" | "content_filter") {
        return Some(crate::reasons::CONTENT_POLICY_VIOLATION);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_shapes() {
        let e = extract(
            500,
            None,
            br#"{"error":{"message":"upstream exploded","type":"server_error"}}"#,
            &Scrub::default(),
        );
        assert_eq!(e.message.as_deref(), Some("upstream exploded"));
        assert_eq!(e.kind.as_deref(), Some("server_error"));
        let e = extract(400, None, br#"{"detail":"bad"}"#, &Scrub::default());
        assert_eq!(e.message.as_deref(), Some("bad"));
        let e = extract(401, None, br#"{"oops":1}"#, &Scrub::default());
        assert_eq!(e.message, None);
        assert_eq!(
            e.fallback,
            "Upstream returned HTTP 401 with an unrecognised error body"
        );
    }

    #[test]
    fn non_json_bodies() {
        let e = extract(503, Some("text/plain"), b"busy\nnow", &Scrub::default());
        assert_eq!(e.fallback, "Upstream returned HTTP 503: busy now");
        let e = extract(401, Some("text/html"), b"<html>x</html>", &Scrub::default());
        assert_eq!(
            e.fallback,
            "Upstream returned HTTP 401 with a non-JSON body (text/html)"
        );
    }
}
