//! Client codec, response side: Chat Completions.

use serde_json::{Map, Value, json};

use super::{Cx, Out, OutReasoning, Response, Stop, Usage, prefixed};
use crate::xlate::ir::Family;
use crate::xlate::warn::{Kind, Warnings};

/// A reasoning block as a `reasoning_details` entry (payloads as-is; `format` names the family).
pub(crate) fn detail(r: &OutReasoning) -> Value {
    let format = r.family.as_str();
    if let Some(data) = &r.redacted {
        return json!({"type": "reasoning.encrypted", "data": data, "format": format});
    }
    match (&r.signature, r.family) {
        (Some(sig), Family::OpenAi | Family::Gemini) if r.text.is_empty() || r.family == Family::OpenAi => {
            json!({"type": "reasoning.encrypted", "data": sig, "format": format})
        }
        (Some(sig), _) => {
            json!({"type": "reasoning.text", "text": r.text, "signature": sig, "format": format})
        }
        (None, _) => json!({"type": "reasoning.text", "text": r.text, "format": format}),
    }
}

/// Chat `usage`.
pub(crate) fn usage(u: &Usage) -> Value {
    let mut prompt = Map::new();
    prompt.insert("cached_tokens".into(), u.cache_read.into());
    if let Some(w) = u.cache_write {
        prompt.insert("cache_write_tokens".into(), w.into());
    }
    json!({
        "prompt_tokens": u.input,
        "completion_tokens": u.output,
        "total_tokens": u.input + u.output,
        "prompt_tokens_details": prompt,
        "completion_tokens_details": {"reasoning_tokens": u.reasoning.unwrap_or(0)},
    })
}

/// A Chat tool call.
pub(crate) fn tool_call(id: &str, name: &str, args: &str, signature: Option<&str>) -> Value {
    let mut tc = Map::new();
    tc.insert("id".into(), id.into());
    tc.insert("type".into(), "function".into());
    tc.insert("function".into(), json!({"name": name, "arguments": args}));
    if let Some(s) = signature {
        tc.insert(
            "extra_content".into(),
            json!({"google": {"thought_signature": s}}),
        );
    }
    Value::Object(tc)
}

pub(super) fn encode(resp: &Response, cx: &Cx<'_>, w: &mut Warnings) -> Value {
    let mut choices = Vec::new();
    for (i, c) in resp.candidates.iter().enumerate() {
        let mut text = String::new();
        let mut refusal: Option<String> = None;
        let mut reasoning = String::new();
        let mut details = Vec::new();
        let mut calls = Vec::new();
        for b in &c.blocks {
            match b {
                Out::Text(t) => text.push_str(t),
                Out::Refusal(r) => refusal.get_or_insert_with(String::new).push_str(r),
                Out::Reasoning(r) => {
                    reasoning.push_str(&r.text);
                    details.push(detail(r));
                }
                Out::Call(call) => calls.push(tool_call(
                    &call.id,
                    &call.name,
                    &call.args,
                    call.signature.as_deref(),
                )),
            }
        }
        if c.stop == Stop::Refusal && refusal.is_none() && !text.is_empty() {
            refusal = Some(std::mem::take(&mut text));
        }
        let mut msg = Map::new();
        msg.insert("role".into(), "assistant".into());
        let content = if !text.is_empty() {
            Value::from(text)
        } else if !calls.is_empty() || refusal.is_some() {
            Value::Null
        } else {
            Value::from("")
        };
        msg.insert("content".into(), content);
        if let Some(r) = refusal {
            msg.insert("refusal".into(), r.into());
        }
        if !reasoning.is_empty() {
            msg.insert("reasoning_content".into(), reasoning.into());
        }
        if !details.is_empty() {
            msg.insert("reasoning_details".into(), Value::Array(details));
        }
        if !calls.is_empty() {
            msg.insert("tool_calls".into(), Value::Array(calls));
        }
        if !c.cites.is_empty() {
            msg.insert(
                "annotations".into(),
                c.cites.iter().map(super::cite::chat_annotation).collect(),
            );
        }
        choices.push(json!({"index": i, "message": msg, "logprobs": null, "finish_reason": c.stop.chat()}));
    }
    let mut out = Map::new();
    out.insert(
        "id".into(),
        prefixed("chatcmpl-", resp.id.as_deref().or(Some(cx.fallback_id))).into(),
    );
    out.insert("object".into(), "chat.completion".into());
    out.insert(
        "created".into(),
        resp.created.filter(|c| *c > 0).unwrap_or(cx.created).into(),
    );
    out.insert("model".into(), cx.model.into());
    out.insert("choices".into(), Value::Array(choices));
    match &resp.usage {
        Some(u) => {
            out.insert("usage".into(), usage(u));
        }
        None => w.response(Kind::Ignored, "usage"),
    }
    Value::Object(out)
}
