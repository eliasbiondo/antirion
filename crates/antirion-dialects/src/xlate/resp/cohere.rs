//! Cohere v2 Chat responses decoded into the IR.

use serde_json::Value;

use super::decode::{args_of, call};
use super::{Bad, Candidate, Cx, Out, OutReasoning, Response, Stop, Usage};
use crate::xlate::ir::Family;
use crate::xlate::util::str_of;

/// A Cohere `finish_reason`; `has_call` when the message has tool calls.
#[must_use]
pub(crate) fn stop(reason: &str, has_call: bool) -> Stop {
    match reason {
        "COMPLETE" if has_call => Stop::ToolUse,
        "COMPLETE" => Stop::EndTurn,
        "TOOL_CALL" => Stop::ToolUse,
        "MAX_TOKENS" => Stop::MaxTokens,
        "STOP_SEQUENCE" => Stop::StopSequence,
        _ => Stop::Other,
    }
}

/// Cohere usage: the counted `tokens`, else the `billed_units`.
pub(crate) fn usage(u: &Value) -> Usage {
    let counts = u
        .get("tokens")
        .filter(|t| t.is_object())
        .or_else(|| u.get("billed_units"))
        .unwrap_or(u);
    let n = |k: &str| {
        counts
            .get(k)
            .and_then(Value::as_f64)
            .map_or(0, |f| f.max(0.0) as u64)
    };
    Usage {
        input: n("input_tokens"),
        output: n("output_tokens"),
        cache_read: u.get("cached_tokens").and_then(Value::as_u64).unwrap_or(0),
        cache_write: None,
        reasoning: None,
    }
}

pub(super) fn decode(body: &Value, cx: &Cx<'_>) -> Result<Response, Bad> {
    let message = body
        .get("message")
        .filter(|m| m.is_object())
        .ok_or(Bad::Unusable)?;
    let mut blocks = Vec::new();
    for part in message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match str_of(part, "type") {
            Some("text") => blocks.push(Out::Text(str_of(part, "text").unwrap_or_default().to_owned())),
            Some("thinking") => blocks.push(Out::Reasoning(OutReasoning {
                text: str_of(part, "thinking").unwrap_or_default().to_owned(),
                signature: None,
                redacted: None,
                family: Family::None,
            })),
            _ => {}
        }
    }
    let calls = message.get("tool_calls").and_then(Value::as_array);
    for c in calls.into_iter().flatten() {
        let f = c.get("function").cloned().unwrap_or(Value::Null);
        let Some(name) = str_of(&f, "name").filter(|n| !n.is_empty()) else {
            return Err(Bad::Malformed);
        };
        blocks.push(call(
            cx,
            str_of(c, "id").unwrap_or_default(),
            name,
            args_of(f.get("arguments")),
            None,
        ));
    }
    let has_call = calls.is_some_and(|c| !c.is_empty());
    let reason = str_of(body, "finish_reason").unwrap_or("COMPLETE");
    Ok(Response {
        id: str_of(body, "id").map(str::to_owned),
        created: None,
        candidates: vec![Candidate {
            blocks,
            stop: stop(reason, has_call),
            stop_sequence: None,
            cites: Vec::new(),
            searches: Vec::new(),
        }],
        usage: body.get("usage").filter(|u| u.is_object()).map(usage),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn usage_prefers_counted_tokens() {
        let u = usage(&json!({"billed_units": {"input_tokens": 3, "output_tokens": 2},
                              "tokens": {"input_tokens": 10, "output_tokens": 4}}));
        assert_eq!((u.input, u.output), (10, 4));
        let billed = usage(&json!({"billed_units": {"input_tokens": 3.0, "output_tokens": 2.0}}));
        assert_eq!((billed.input, billed.output), (3, 2));
        assert_eq!(stop("COMPLETE", true), Stop::ToolUse);
        assert_eq!(stop("MAX_TOKENS", false), Stop::MaxTokens);
    }
}
