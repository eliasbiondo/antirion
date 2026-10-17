//! Cohere v2 Chat bodies. Messages and tools have the Chat Completions shapes, with assistant text
//! as content parts; the generation controls have Cohere's own names (`p`, `k`, `stop_sequences`).

use serde_json::{Map, Value, json};

use super::Enc;
use super::sampling::Structured;
use crate::xlate::ir::Choice;
use crate::xlate::util::put;
use crate::xlate::warn::Kind;

pub(super) fn encode(e: &mut Enc<'_>) -> Value {
    e.web_search();
    let req = e.req;
    let mut out = Map::new();
    out.insert("model".into(), Value::from(e.t.model.as_str()));
    let messages: Vec<Value> = super::chat::messages(e).into_iter().map(message).collect();
    out.insert("messages".into(), Value::Array(messages));
    let mut tools = super::chat::tools(e);
    match req.choice.as_ref().map(|c| &c.0) {
        Some(Choice::None) => {
            out.insert("tool_choice".into(), "NONE".into());
        }
        Some(Choice::Required) => {
            out.insert("tool_choice".into(), "REQUIRED".into());
        }
        Some(Choice::Named(n)) => {
            let wanted = e.names.up(n);
            tools.retain(|t| t.pointer("/function/name").and_then(Value::as_str) == Some(wanted.as_str()));
            out.insert("tool_choice".into(), "REQUIRED".into());
        }
        Some(Choice::Allowed { required, names }) => {
            let allowed: Vec<String> = names.iter().map(|n| e.names.up(n)).collect();
            tools.retain(|t| {
                t.pointer("/function/name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| allowed.iter().any(|a| a == n))
            });
            if *required {
                out.insert("tool_choice".into(), "REQUIRED".into());
            }
        }
        Some(Choice::Auto) | None => {}
    }
    if !tools.is_empty() {
        out.insert("tools".into(), Value::Array(tools));
    }
    if let Some((p, src)) = &req.parallel {
        e.warn(if *p { Kind::Ignored } else { Kind::Dropped }, src);
    }
    if let Some((v, _)) = &req.max_tokens {
        out.insert("max_tokens".into(), (*v).into());
    }
    put(
        &mut out,
        "temperature",
        e.temperature(false).unwrap_or(Value::Null),
    );
    let sent = out.contains_key("temperature");
    put(&mut out, "p", e.top_p(false, sent).unwrap_or(Value::Null));
    if let Some(k) = &req.top_k {
        out.insert("k".into(), k.value.clone());
    }
    if let Some(stop) = e.stop() {
        out.insert("stop_sequences".into(), stop.into());
    }
    for (k, n) in [
        ("frequency_penalty", &req.frequency_penalty),
        ("presence_penalty", &req.presence_penalty),
        ("seed", &req.seed),
    ] {
        if let Some(n) = n {
            out.insert(k.into(), n.value.clone());
        }
    }
    e.drop_field(req.logit_bias.as_ref());
    e.drop_field(req.logprobs.as_ref());
    e.drop_field(req.top_logprobs.as_ref());
    match e.structured(true) {
        Structured::Native(Some(schema)) => {
            out.insert(
                "response_format".into(),
                json!({"type": "json_object", "json_schema": schema}),
            );
        }
        Structured::Native(None) => {
            out.insert("response_format".into(), json!({"type": "json_object"}));
        }
        _ => {}
    }
    if let Some((_, src)) = &req.end_user {
        e.warn(Kind::Ignored, src);
    }
    if req.stream {
        out.insert("stream".into(), true.into());
    }
    Value::Object(out)
}

/// A Chat message in Cohere's shape: assistant text as content parts (no `content` beside tool
/// calls alone), `developer` as `system`.
fn message(m: Value) -> Value {
    let Value::Object(mut o) = m else {
        return m;
    };
    if o.get("role").and_then(Value::as_str) == Some("developer") {
        o.insert("role".into(), "system".into());
    }
    if o.get("role").and_then(Value::as_str) == Some("assistant") {
        match o.remove("content") {
            Some(Value::String(s)) if !s.is_empty() => {
                o.insert("content".into(), json!([{"type": "text", "text": s}]));
            }
            Some(Value::Array(parts)) if !parts.is_empty() => {
                o.insert("content".into(), Value::Array(parts));
            }
            _ if o.contains_key("tool_calls") => {}
            _ => {
                o.insert("content".into(), json!([{"type": "text", "text": ""}]));
            }
        }
        o.remove("refusal");
    }
    Value::Object(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assistant_text_becomes_parts() {
        let m = message(json!({"role": "assistant", "content": "hi"}));
        assert_eq!(
            m,
            json!({"role": "assistant", "content": [{"type": "text", "text": "hi"}]})
        );
        let calls = message(json!({"role": "assistant", "content": null, "tool_calls": [{"id": "c"}]}));
        assert_eq!(calls, json!({"role": "assistant", "tool_calls": [{"id": "c"}]}));
        assert_eq!(
            message(json!({"role": "developer", "content": "s"})),
            json!({"role": "system", "content": "s"})
        );
    }
}
