//! Client codec, response side: Anthropic Messages.

use serde_json::{Value, json};

use super::{Cx, Out, OutReasoning, Response, Stop, Usage, prefixed};
use crate::xlate::envelope;
use crate::xlate::ir::Family;
use crate::xlate::util::parse_args;
use crate::xlate::warn::{Kind, Warnings};

/// The signature an Anthropic client sees for a reasoning block: Anthropic signatures as they are,
/// other families' payloads in the envelope, `""` when there is none.
pub(crate) fn signature(r: &OutReasoning, provider: &str) -> String {
    let payload = r.signature.as_ref().or(r.redacted.as_ref());
    match (r.family, payload) {
        (Family::Anthropic, Some(sig)) => sig.clone(),
        (Family::None, _) | (_, None) => String::new(),
        (family, Some(p)) => {
            let provider = (family == Family::OpenAi).then_some(provider);
            envelope::wrap(family, provider, p)
        }
    }
}

/// An Anthropic reasoning block.
pub(crate) fn reasoning_block(r: &OutReasoning, provider: &str) -> Value {
    if r.family == Family::Anthropic
        && let Some(data) = &r.redacted
    {
        return json!({"type": "redacted_thinking", "data": data});
    }
    json!({"type": "thinking", "thinking": r.text, "signature": signature(r, provider)})
}

/// Anthropic `usage`: input without the cache counts, cache fields always present.
pub(crate) fn usage(u: &Usage) -> Value {
    let write = u.cache_write.unwrap_or(0);
    json!({
        "input_tokens": u.input.saturating_sub(u.cache_read).saturating_sub(write),
        "output_tokens": u.output,
        "cache_read_input_tokens": u.cache_read,
        "cache_creation_input_tokens": write,
    })
}

/// A tool call's input object; arguments that are not a JSON object become `{}`.
pub(crate) fn input(args: &str, w: &mut Warnings) -> Value {
    parse_args(args).unwrap_or_else(|| {
        w.response(Kind::Approximated, "tool_calls[].arguments");
        json!({})
    })
}

pub(super) fn encode(resp: &Response, cx: &Cx<'_>, w: &mut Warnings) -> Value {
    let mut content = Vec::new();
    let (stop, stop_sequence) = match resp.candidates.first() {
        Some(c) => {
            let has_refusal = c.blocks.iter().any(|b| matches!(b, Out::Refusal(_)));
            if !c.searches.is_empty() {
                w.response(Kind::Dropped, "web_search");
            }
            let mut offset = 0usize;
            for b in &c.blocks {
                match b {
                    Out::Text(t) => {
                        if !t.is_empty() {
                            content.extend(super::cite::anthropic_text(t, offset, &c.cites));
                        }
                        offset += t.chars().count();
                    }
                    Out::Refusal(t) => {
                        if !t.is_empty() {
                            content.push(json!({"type": "text", "text": t}));
                        }
                    }
                    Out::Reasoning(r) => content.push(reasoning_block(r, cx.provider)),
                    Out::Call(call) => content
                        .push(json!({"type": "tool_use", "id": call.id, "name": call.name,
                                                            "input": input(&call.args, w)})),
                }
            }
            let stop = if has_refusal { Stop::Refusal } else { c.stop };
            let seq = if stop == Stop::StopSequence {
                c.stop_sequence.clone()
            } else {
                None
            };
            (stop, seq)
        }
        None => (Stop::EndTurn, None),
    };
    json!({
        "id": prefixed("msg_", resp.id.as_deref().or(Some(cx.fallback_id))),
        "type": "message",
        "role": "assistant",
        "model": cx.model,
        "content": content,
        "stop_reason": stop.anthropic(),
        "stop_sequence": stop_sequence,
        "usage": usage(&resp.usage.clone().unwrap_or_default()),
    })
}
