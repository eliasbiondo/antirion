//! The `antirion.guardrail.v1` webhook contract: the request body and the strict reading of the
//! answer. Anything outside the contract is a failure, never a pass.

use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Map, Value, json};
use sha2::Sha256;

use crate::def::GuardStage;
use crate::segments::Segment;

/// The request facts a webhook receives.
#[derive(Clone, Debug, Default)]
pub struct CallFacts {
    pub request_id: String,
    /// `openai.chat_completions`, `anthropic.messages`, …
    pub endpoint: String,
    pub model: String,
    pub organization: String,
    pub team: String,
    pub project: String,
    pub key: String,
    pub end_user: Option<String>,
}

/// The verdict a webhook answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Said {
    Pass,
    Flag,
    Block,
}

/// A webhook answer that follows the contract.
#[derive(Clone, Debug)]
pub struct Answer {
    pub verdict: Said,
    pub category: Option<String>,
    pub reason: Option<String>,
    /// `(segment id, replacement text)`.
    pub mutations: Vec<(String, String)>,
    /// The replacement of a `mode: body` answer: always a JSON object.
    pub body: Option<Value>,
}

/// The JSON body of one call; `segments` are those in the guardrail's scope, numbered `s0`, `s1`, …
#[must_use]
pub fn request_body(guardrail: &str, stage: GuardStage, facts: &CallFacts, segments: &[&Segment]) -> Vec<u8> {
    let segs: Vec<Value> = segments
        .iter()
        .enumerate()
        .map(|(i, s)| {
            json!({
                "id": format!("s{i}"),
                "kind": s.kind.as_str(),
                "origin": s.origin.as_str(),
                "path": s.path(),
                "text": s.text,
            })
        })
        .collect();
    let body = json!({
        "version": "antirion.guardrail.v1",
        "request_id": facts.request_id,
        "guardrail": guardrail,
        "stage": stage.as_str(),
        "final": true,
        "endpoint": facts.endpoint,
        "model": facts.model,
        "tenant": {"organization": facts.organization, "team": facts.team, "project": facts.project, "key": facts.key},
        "end_user": facts.end_user,
        "segments": segs,
    });
    serde_json::to_vec(&body).unwrap_or_default()
}

/// The JSON body of one call in `mode: body`: the whole request or response.
#[must_use]
pub fn body_request(guardrail: &str, stage: GuardStage, facts: &CallFacts, body: &Value) -> Vec<u8> {
    let call = json!({
        "version": "antirion.guardrail.v1",
        "request_id": facts.request_id,
        "guardrail": guardrail,
        "stage": stage.as_str(),
        "body": body,
    });
    serde_json::to_vec(&call).unwrap_or_default()
}

/// `sha256=<hex HMAC-SHA256(secret, body)>`.
#[must_use]
pub fn signature(secret: &str, body: &[u8]) -> Option<String> {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes()).ok()?;
    mac.update(body);
    let hex: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Some(format!("sha256={hex}"))
}

fn category_ok(c: &str) -> bool {
    !c.is_empty()
        && c.len() <= 64
        && c.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

fn optional_string(o: &Map<String, Value>, key: &str, max: usize) -> Result<Option<String>, String> {
    match o.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.chars().count() <= max => Ok(Some(s.clone())),
        Some(_) => Err(format!("`{key}` is not a string of at most {max} characters")),
    }
}

/// Reads an answer: status 200 and a JSON object with a known verdict and well-typed fields.
pub fn parse_answer(status: u16, body: &[u8]) -> Result<Answer, String> {
    if status != 200 {
        return Err(format!("the webhook answered with status {status}"));
    }
    let v: Value = serde_json::from_slice(body).map_err(|_| "the answer is not JSON".to_owned())?;
    let o = v.as_object().ok_or("the answer is not a JSON object")?;
    let verdict = match o.get("verdict").and_then(Value::as_str) {
        Some("pass") => Said::Pass,
        Some("flag") => Said::Flag,
        Some("block") => Said::Block,
        Some(_) => return Err("unknown verdict".to_owned()),
        None => return Err("the answer has no verdict".to_owned()),
    };
    let category = optional_string(o, "category", 64)?;
    if category.as_deref().is_some_and(|c| !category_ok(c)) {
        return Err("`category` has characters outside [A-Za-z0-9_.-]".to_owned());
    }
    let reason = optional_string(o, "reason", 500)?;
    let mutations = match o.get("mutations") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|m| {
                let seg = m.get("segment").and_then(Value::as_str);
                let text = m.get("text").and_then(Value::as_str);
                match (seg, text) {
                    (Some(s), Some(t)) => Ok((s.to_owned(), t.to_owned())),
                    _ => Err("a mutation needs a string `segment` and `text`".to_owned()),
                }
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err("`mutations` is not an array".to_owned()),
    };
    let body = match o.get("body") {
        None | Some(Value::Null) => None,
        Some(b @ Value::Object(_)) => Some(b.clone()),
        Some(_) => return Err("the replacement `body` is not a JSON object".to_owned()),
    };
    Ok(Answer {
        verdict,
        category,
        reason,
        mutations,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_violations_are_errors() {
        for (status, body) in [
            (201, r#"{"verdict":"pass"}"#),
            (200, "<html>"),
            (200, r#"[{"verdict":"pass"}]"#),
            (200, r#"{"category":"pii"}"#),
            (200, r#"{"verdict":"maybe"}"#),
            (200, r#"{"verdict":true}"#),
            (200, r#"{"verdict":"block","category":7}"#),
            (200, r#"{"verdict":"pass","mutations":"s1"}"#),
            (200, r#"{"verdict":"pass","body":[1]}"#),
            (
                200,
                r#"{"verdict":"pass","mutations":[{"segment":"s1","text":5}]}"#,
            ),
        ] {
            assert!(parse_answer(status, body.as_bytes()).is_err(), "{body}");
        }
        let a = parse_answer(
            200,
            br#"{"verdict":"block","category":"confidential","reason":"r"}"#,
        )
        .unwrap();
        assert_eq!(a.verdict, Said::Block);
        assert_eq!(a.category.as_deref(), Some("confidential"));
    }
}
