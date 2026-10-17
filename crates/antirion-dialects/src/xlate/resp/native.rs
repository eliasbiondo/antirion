//! Upstream codecs, response side: Anthropic Messages, Gemini and Converse bodies.

use serde_json::Value;

use super::cite::{Search, from_anthropic, from_grounding};
use super::decode::{args_of, call};
use super::{Bad, Candidate, Cx, Out, OutReasoning, Response, Stop, Usage};
use crate::xlate::ir::Family;
use crate::xlate::util::{sha_prefix, str_of};
use crate::xlate::warn::{Kind, Warnings};

fn n(v: Option<&Value>) -> u64 {
    v.and_then(Value::as_u64).unwrap_or(0)
}

/// Usage of an Anthropic body or `message_start`/`message_delta` usage (disjoint counts).
pub(crate) fn anthropic_usage(u: &Value) -> Usage {
    let read = n(u.get("cache_read_input_tokens"));
    let write = n(u.get("cache_creation_input_tokens"));
    Usage {
        input: n(u.get("input_tokens")) + read + write,
        output: n(u.get("output_tokens")),
        cache_read: read,
        cache_write: Some(write),
        reasoning: None,
    }
}

/// Usage of a Gemini `usageMetadata`.
pub(crate) fn gemini_usage(u: &Value) -> Usage {
    let thoughts = u.get("thoughtsTokenCount").and_then(Value::as_u64);
    Usage {
        input: n(u.get("promptTokenCount")) + n(u.get("toolUsePromptTokenCount")),
        output: n(u.get("candidatesTokenCount")) + thoughts.unwrap_or(0),
        cache_read: n(u.get("cachedContentTokenCount")),
        cache_write: None,
        reasoning: thoughts,
    }
}

/// Usage of a Converse `usage` (disjoint counts).
pub(crate) fn converse_usage(u: &Value) -> Usage {
    let read = n(u.get("cacheReadInputTokens"));
    let write = n(u.get("cacheWriteInputTokens"));
    Usage {
        input: n(u.get("inputTokens")) + read + write,
        output: n(u.get("outputTokens")),
        cache_read: read,
        cache_write: Some(write),
        reasoning: None,
    }
}

pub(super) fn anthropic(body: &Value, cx: &Cx<'_>, w: &mut Warnings) -> Result<Response, Bad> {
    let content = body
        .get("content")
        .and_then(Value::as_array)
        .ok_or(Bad::Unusable)?;
    let mut blocks = Vec::new();
    let mut cites = Vec::new();
    let mut searches = Vec::new();
    let mut offset = 0usize;
    for b in content {
        match str_of(b, "type") {
            Some("text") => {
                let start = offset;
                offset += str_of(b, "text").map_or(0, |t| t.chars().count());
                anthropic_block(b, cx, &mut blocks, w);
                for c in b.get("citations").and_then(Value::as_array).into_iter().flatten() {
                    cites.push(from_anthropic(c, start, offset));
                }
            }
            Some("server_tool_use") if str_of(b, "name") == Some("web_search") => searches.push(Search {
                id: str_of(b, "id").unwrap_or_default().to_owned(),
                query: b
                    .pointer("/input/query")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            }),
            Some("web_search_tool_result") => {}
            _ => anthropic_block(b, cx, &mut blocks, w),
        }
    }
    Ok(Response {
        created: None,
        id: str_of(body, "id").map(str::to_owned),
        candidates: vec![Candidate {
            blocks,
            stop: str_of(body, "stop_reason").map_or(Stop::EndTurn, Stop::from_anthropic),
            stop_sequence: str_of(body, "stop_sequence").map(str::to_owned),
            cites,
            searches,
        }],
        usage: body.get("usage").filter(|u| u.is_object()).map(anthropic_usage),
    })
}

pub(crate) fn anthropic_block(b: &Value, cx: &Cx<'_>, blocks: &mut Vec<Out>, w: &mut Warnings) {
    match str_of(b, "type") {
        Some("text") => {
            if let Some(t) = str_of(b, "text").filter(|t| !t.is_empty()) {
                blocks.push(Out::Text(t.to_owned()));
            }
        }
        Some("thinking") => blocks.push(Out::Reasoning(OutReasoning {
            text: str_of(b, "thinking").unwrap_or_default().to_owned(),
            signature: str_of(b, "signature")
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            redacted: None,
            family: Family::Anthropic,
        })),
        Some("redacted_thinking") => blocks.push(Out::Reasoning(OutReasoning {
            text: String::new(),
            signature: None,
            redacted: str_of(b, "data").map(str::to_owned),
            family: Family::Anthropic,
        })),
        Some("tool_use") => blocks.push(call(
            cx,
            str_of(b, "id").unwrap_or_default(),
            str_of(b, "name").unwrap_or_default(),
            args_of(b.get("input")),
            None,
        )),
        Some(other) => w.response(Kind::Dropped, &format!("content[{other}]")),
        None => {}
    }
}

/// The id of a Gemini call without one: derived from the response id and the part's position.
pub(crate) fn gemini_call_id(response_id: &str, index: usize) -> String {
    format!("call_{}", sha_prefix(&format!("{response_id}:{index}"), 24))
}

/// One Gemini part as blocks.
pub(crate) fn gemini_part(
    p: &Value,
    cx: &Cx<'_>,
    index: usize,
    response_id: &str,
    blocks: &mut Vec<Out>,
    w: &mut Warnings,
) {
    let sig = str_of(p, "thoughtSignature").map(str::to_owned);
    if let Some(fc) = p.get("functionCall") {
        if let Some(s) = &sig {
            blocks.push(Out::Reasoning(OutReasoning {
                text: String::new(),
                signature: Some(s.clone()),
                redacted: None,
                family: Family::Gemini,
            }));
        }
        let id = str_of(fc, "id")
            .filter(|i| !i.is_empty())
            .map_or_else(|| gemini_call_id(response_id, index), str::to_owned);
        blocks.push(call(
            cx,
            &id,
            str_of(fc, "name").unwrap_or_default(),
            args_of(fc.get("args")),
            sig,
        ));
        return;
    }
    if let Some(t) = str_of(p, "text") {
        if p.get("thought") == Some(&Value::Bool(true)) {
            blocks.push(Out::Reasoning(OutReasoning {
                text: t.to_owned(),
                signature: sig,
                redacted: None,
                family: Family::Gemini,
            }));
        } else if !t.is_empty() {
            blocks.push(Out::Text(t.to_owned()));
        }
        return;
    }
    if let Some(key) = p
        .as_object()
        .and_then(|o| o.keys().find(|k| k.as_str() != "thoughtSignature"))
    {
        w.response(Kind::Dropped, &format!("candidates[].content.parts[{key}]"));
    }
}

const MALFORMED: [&str; 3] = [
    "MALFORMED_FUNCTION_CALL",
    "UNEXPECTED_TOOL_CALL",
    "MALFORMED_RESPONSE",
];

pub(super) fn gemini(body: &Value, cx: &Cx<'_>, w: &mut Warnings) -> Result<Response, Bad> {
    let response_id = str_of(body, "responseId").unwrap_or(cx.fallback_id).to_owned();
    let usage = body
        .get("usageMetadata")
        .filter(|u| u.is_object())
        .map(gemini_usage);
    let list = body.get("candidates").and_then(Value::as_array);
    let Some(list) = list.filter(|l| !l.is_empty()) else {
        if body.pointer("/promptFeedback/blockReason").is_some() {
            return Ok(Response {
                created: None,
                id: Some(response_id),
                candidates: vec![Candidate {
                    blocks: Vec::new(),
                    stop: Stop::ContentFilter,
                    stop_sequence: None,
                    cites: Vec::new(),
                    searches: Vec::new(),
                }],
                usage,
            });
        }
        return Err(Bad::Unusable);
    };
    let mut candidates = Vec::new();
    let mut index = 0usize;
    for c in list {
        let mut blocks = Vec::new();
        for p in c
            .pointer("/content/parts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            gemini_part(p, cx, index, &response_id, &mut blocks, w);
            index += 1;
        }
        let finish = str_of(c, "finishReason").unwrap_or("STOP");
        if MALFORMED.contains(&finish) && blocks.is_empty() {
            return Err(Bad::Malformed);
        }
        let has_call = blocks.iter().any(|b| matches!(b, Out::Call(_)));
        let (cites, searches) = match c.get("groundingMetadata") {
            Some(g) => from_grounding(g, &joined_text(&blocks)),
            None => (Vec::new(), Vec::new()),
        };
        candidates.push(Candidate {
            blocks,
            stop: Stop::from_gemini(finish, has_call),
            stop_sequence: None,
            cites,
            searches,
        });
    }
    Ok(Response {
        created: None,
        id: Some(response_id),
        candidates,
        usage,
    })
}

pub(crate) fn converse_block(b: &Value, cx: &Cx<'_>, blocks: &mut Vec<Out>, w: &mut Warnings) {
    if let Some(t) = str_of(b, "text") {
        if !t.is_empty() {
            blocks.push(Out::Text(t.to_owned()));
        }
    } else if let Some(tu) = b.get("toolUse") {
        blocks.push(call(
            cx,
            str_of(tu, "toolUseId").unwrap_or_default(),
            str_of(tu, "name").unwrap_or_default(),
            args_of(tu.get("input")),
            None,
        ));
    } else if let Some(rc) = b.get("reasoningContent") {
        let rt = rc.get("reasoningText");
        blocks.push(Out::Reasoning(OutReasoning {
            text: rt.and_then(|r| str_of(r, "text")).unwrap_or_default().to_owned(),
            signature: rt
                .and_then(|r| str_of(r, "signature"))
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            redacted: str_of(rc, "redactedContent").map(str::to_owned),
            family: Family::Anthropic,
        }));
    } else if let Some(key) = b.as_object().and_then(|o| o.keys().next()) {
        w.response(Kind::Dropped, &format!("output.message.content[{key}]"));
    }
}

pub(super) fn converse(body: &Value, cx: &Cx<'_>, w: &mut Warnings) -> Result<Response, Bad> {
    let content = body
        .pointer("/output/message/content")
        .and_then(Value::as_array)
        .ok_or(Bad::Unusable)?;
    let mut blocks = Vec::new();
    for b in content {
        converse_block(b, cx, &mut blocks, w);
    }
    let reason = str_of(body, "stopReason").unwrap_or("end_turn");
    if matches!(reason, "malformed_model_output" | "malformed_tool_use") && blocks.is_empty() {
        return Err(Bad::Malformed);
    }
    Ok(Response {
        created: None,
        id: None,
        candidates: vec![Candidate {
            blocks,
            stop: Stop::from_converse(reason),
            stop_sequence: body
                .pointer("/additionalModelResponseFields/stop_sequence")
                .and_then(Value::as_str)
                .map(str::to_owned),
            cites: Vec::new(),
            searches: Vec::new(),
        }],
        usage: body.get("usage").filter(|u| u.is_object()).map(converse_usage),
    })
}

/// The text blocks of a candidate joined.
fn joined_text(blocks: &[Out]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            Out::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}
