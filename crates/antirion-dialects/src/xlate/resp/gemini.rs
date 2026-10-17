//! Client codec, response side: Gemini `generateContent`.

use serde_json::{Map, Value, json};

use super::{Cx, Out, OutCall, OutReasoning, Response, Stop, Usage};
use crate::xlate::envelope;
use crate::xlate::ir::Family;
use crate::xlate::util::parse_args;
use crate::xlate::warn::{Kind, Warnings};

/// Gemini `usageMetadata`: candidates exclude thoughts, the prompt includes cached tokens.
pub(crate) fn usage(u: &Usage) -> Value {
    let thoughts = u.reasoning.unwrap_or(0);
    let mut m = Map::new();
    m.insert("promptTokenCount".into(), u.input.into());
    m.insert(
        "candidatesTokenCount".into(),
        u.output.saturating_sub(thoughts).into(),
    );
    m.insert("totalTokenCount".into(), (u.input + u.output).into());
    if u.cache_read > 0 {
        m.insert("cachedContentTokenCount".into(), u.cache_read.into());
    }
    if thoughts > 0 {
        m.insert("thoughtsTokenCount".into(), thoughts.into());
    }
    Value::Object(m)
}

/// The `thoughtSignature` a Gemini client sees: Gemini payloads as they are, other families' in the
/// envelope.
pub(crate) fn signature(r: &OutReasoning, provider: &str) -> Option<String> {
    let payload = r.signature.as_ref().or(r.redacted.as_ref())?;
    match r.family {
        Family::Gemini => Some(payload.clone()),
        Family::None => None,
        family => {
            let provider = (family == Family::OpenAi).then_some(provider);
            Some(envelope::wrap(family, provider, payload))
        }
    }
}

/// A thought part.
pub(crate) fn thought(text: &str, sig: Option<String>) -> Value {
    let mut p = Map::new();
    p.insert("text".into(), text.into());
    p.insert("thought".into(), true.into());
    if let Some(s) = sig {
        p.insert("thoughtSignature".into(), s.into());
    }
    Value::Object(p)
}

/// A function call part; arguments that are not a JSON object become `{}`.
pub(crate) fn call_part(c: &OutCall, sig: Option<String>, w: &mut Warnings) -> Value {
    let args = parse_args(&c.args).unwrap_or_else(|| {
        w.response(Kind::Approximated, "tool_calls[].arguments");
        json!({})
    });
    let mut p = Map::new();
    p.insert(
        "functionCall".into(),
        json!({"id": c.id, "name": c.name, "args": args}),
    );
    if let Some(s) = sig {
        p.insert("thoughtSignature".into(), s.into());
    }
    Value::Object(p)
}

/// The Gemini `finishReason` of a stop.
pub(crate) fn finish(stop: Stop) -> &'static str {
    match stop {
        Stop::EndTurn | Stop::StopSequence | Stop::ToolUse | Stop::PauseTurn => "STOP",
        Stop::MaxTokens | Stop::ContextWindow => "MAX_TOKENS",
        Stop::ContentFilter | Stop::Refusal => "SAFETY",
        Stop::Other => "OTHER",
    }
}

/// The parts of one candidate.
pub(crate) fn parts(blocks: &[Out], provider: &str, w: &mut Warnings) -> Vec<Value> {
    let mut out = Vec::new();
    let mut iter = blocks.iter().peekable();
    while let Some(b) = iter.next() {
        match b {
            Out::Text(t) | Out::Refusal(t) => {
                if !t.is_empty() {
                    out.push(json!({"text": t}));
                }
            }
            Out::Reasoning(r) if r.family == Family::Anthropic && r.redacted.is_some() => {
                w.response(Kind::Dropped, "content[redacted_thinking]");
            }
            Out::Reasoning(r) => {
                let sig = signature(r, provider);
                let carried_by_call = r.text.is_empty()
                    && matches!(iter.peek(), Some(Out::Call(c)) if c.signature.is_some() && c.signature == sig);
                if carried_by_call || (r.text.is_empty() && sig.is_none()) {
                    continue;
                }
                out.push(thought(&r.text, sig));
            }
            Out::Call(c) => out.push(call_part(c, c.signature.clone(), w)),
        }
    }
    out
}

pub(super) fn encode(resp: &Response, cx: &Cx<'_>, w: &mut Warnings) -> Value {
    if resp.candidates.iter().any(|c| !c.cites.is_empty()) {
        w.response(Kind::Dropped, "citations");
    }
    if resp.candidates.iter().any(|c| !c.searches.is_empty()) {
        w.response(Kind::Dropped, "web_search");
    }
    let candidates: Vec<Value> = resp
        .candidates
        .iter()
        .enumerate()
        .map(|(i, c)| {
            json!({
                "content": {"role": "model", "parts": parts(&c.blocks, cx.provider, w)},
                "finishReason": finish(c.stop),
                "index": i,
            })
        })
        .collect();
    let mut out = Map::new();
    out.insert("candidates".into(), Value::Array(candidates));
    match &resp.usage {
        Some(u) => {
            out.insert("usageMetadata".into(), usage(u));
        }
        None => w.response(Kind::Ignored, "usage"),
    }
    out.insert("modelVersion".into(), cx.model.into());
    out.insert(
        "responseId".into(),
        resp.id.as_deref().unwrap_or(cx.fallback_id).into(),
    );
    Value::Object(out)
}
