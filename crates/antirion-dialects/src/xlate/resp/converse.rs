//! Client codec, response side: Bedrock Converse.

use serde_json::{Map, Value, json};

use super::{Cx, Out, OutReasoning, Response, Stop, Usage};
use crate::xlate::envelope;
use crate::xlate::ir::Family;
use crate::xlate::warn::{Kind, Warnings};

/// Converse `usage`: disjoint input and cache counts.
pub(crate) fn usage(u: &Usage) -> Value {
    let write = u.cache_write.unwrap_or(0);
    let mut m = Map::new();
    m.insert(
        "inputTokens".into(),
        u.input.saturating_sub(u.cache_read).saturating_sub(write).into(),
    );
    m.insert("outputTokens".into(), u.output.into());
    m.insert("totalTokens".into(), (u.input + u.output).into());
    if u.cache_read > 0 {
        m.insert("cacheReadInputTokens".into(), u.cache_read.into());
    }
    if u.cache_write.is_some() {
        m.insert("cacheWriteInputTokens".into(), write.into());
    }
    Value::Object(m)
}

/// The Converse `stopReason` of a stop.
pub(crate) fn stop_reason(stop: Stop) -> &'static str {
    match stop {
        Stop::EndTurn | Stop::PauseTurn | Stop::Other => "end_turn",
        Stop::StopSequence => "stop_sequence",
        Stop::MaxTokens => "max_tokens",
        Stop::ToolUse => "tool_use",
        Stop::ContentFilter | Stop::Refusal => "content_filtered",
        Stop::ContextWindow => "model_context_window_exceeded",
    }
}

/// The `reasoningContent` of a reasoning block: Anthropic payloads as they are, other families'
/// signatures in the envelope.
pub(crate) fn reasoning(r: &OutReasoning, provider: &str) -> Value {
    if r.family == Family::Anthropic
        && let Some(data) = &r.redacted
    {
        return json!({"redactedContent": data});
    }
    let mut text = Map::new();
    text.insert("text".into(), r.text.as_str().into());
    let payload = r.signature.as_ref().or(r.redacted.as_ref());
    match (r.family, payload) {
        (Family::Anthropic, Some(sig)) => {
            text.insert("signature".into(), sig.as_str().into());
        }
        (Family::None, _) | (_, None) => {}
        (family, Some(p)) => {
            let provider = (family == Family::OpenAi).then_some(provider);
            text.insert("signature".into(), envelope::wrap(family, provider, p).into());
        }
    }
    json!({"reasoningText": text})
}

pub(super) fn encode(resp: &Response, cx: &Cx<'_>, w: &mut Warnings) -> Value {
    if resp.candidates.iter().any(|c| !c.cites.is_empty()) {
        w.response(Kind::Dropped, "citations");
    }
    if resp.candidates.iter().any(|c| !c.searches.is_empty()) {
        w.response(Kind::Dropped, "web_search");
    }
    let mut content = Vec::new();
    let (stop, seq) = match resp.candidates.first() {
        Some(c) => {
            for b in &c.blocks {
                match b {
                    Out::Text(t) | Out::Refusal(t) => {
                        if !t.is_empty() {
                            content.push(json!({"text": t}));
                        }
                    }
                    Out::Reasoning(r) => content.push(json!({"reasoningContent": reasoning(r, cx.provider)})),
                    Out::Call(call) => content.push(json!({"toolUse": {
                        "toolUseId": call.id, "name": call.name,
                        "input": super::anthropic::input(&call.args, w)}})),
                }
            }
            let seq = (c.stop == Stop::StopSequence)
                .then(|| c.stop_sequence.clone())
                .flatten();
            (c.stop, seq)
        }
        None => (Stop::EndTurn, None),
    };
    let mut out = Map::new();
    out.insert(
        "output".into(),
        json!({"message": {"role": "assistant", "content": content}}),
    );
    out.insert("stopReason".into(), stop_reason(stop).into());
    out.insert("usage".into(), usage(&resp.usage.clone().unwrap_or_default()));
    out.insert("metrics".into(), json!({"latencyMs": 0}));
    if let Some(s) = seq {
        out.insert(
            "additionalModelResponseFields".into(),
            json!({"stop_sequence": s}),
        );
    }
    Value::Object(out)
}
