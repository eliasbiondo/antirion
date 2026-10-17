//! Upstream codecs, response side: OpenAI Chat and Responses bodies, and the dispatch to the native
//! decoders.

use serde_json::Value;

use super::cite::{Cite, Search, from_annotation};
use super::{Bad, Candidate, Cx, Out, OutCall, OutReasoning, Response, Stop, Usage, native};
use crate::xlate::encode::JSON_TOOL;
use crate::xlate::ir::Family;
use crate::xlate::target::Upstream;
use crate::xlate::util::{compact, str_of};
use crate::xlate::warn::{Kind, Warnings};

/// Decodes an upstream response of `cx.upstream`.
pub fn decode(body: &Value, cx: &Cx<'_>, w: &mut Warnings) -> Result<Response, Bad> {
    let mut resp = match cx.upstream {
        Upstream::Chat => chat(body, cx)?,
        Upstream::Responses => responses(body, cx, w)?,
        Upstream::Anthropic => native::anthropic(body, cx, w)?,
        Upstream::Gemini => native::gemini(body, cx, w)?,
        Upstream::Converse => native::converse(body, cx, w)?,
        Upstream::Cohere => super::cohere::decode(body, cx)?,
    };
    if cx.restore.json_tool {
        for c in &mut resp.candidates {
            unwrap_json_tool(c);
        }
    }
    Ok(resp)
}

/// Emulated structured output: the forced tool's input is the answer's text.
pub(crate) fn unwrap_json_tool(c: &mut Candidate) {
    let mut found = false;
    for b in &mut c.blocks {
        if let Out::Call(call) = b
            && call.name == JSON_TOOL
        {
            *b = Out::Text(call.args.clone());
            found = true;
        }
    }
    if found && c.stop == Stop::ToolUse {
        c.stop = Stop::EndTurn;
    }
}

/// An upstream call with the client's id and name restored.
pub(crate) fn call(cx: &Cx<'_>, id: &str, name: &str, args: String, signature: Option<String>) -> Out {
    Out::Call(OutCall {
        id: cx.restore.ids.client(id),
        name: cx.restore.names.client(name),
        args,
        signature,
    })
}

/// Arguments text from a value that is a string (kept as sent) or an object (serialised).
pub(crate) fn args_of(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => "{}".to_owned(),
        Some(other) => compact(other),
    }
}

fn num(v: Option<&Value>, path: &[&str]) -> Option<u64> {
    let mut cur = v?;
    for p in path {
        cur = cur.get(*p)?;
    }
    cur.as_u64()
}

/// Usage of a Chat body (also used by stream decoders).
pub(crate) fn chat_usage(u: &Value) -> Usage {
    Usage {
        input: num(Some(u), &["prompt_tokens"]).unwrap_or(0),
        output: num(Some(u), &["completion_tokens"]).unwrap_or(0),
        cache_read: num(Some(u), &["prompt_tokens_details", "cached_tokens"]).unwrap_or(0),
        cache_write: num(Some(u), &["prompt_tokens_details", "cache_write_tokens"]),
        reasoning: num(Some(u), &["completion_tokens_details", "reasoning_tokens"]),
    }
}

/// Usage of a Responses body.
pub(crate) fn responses_usage(u: &Value) -> Usage {
    Usage {
        input: num(Some(u), &["input_tokens"]).unwrap_or(0),
        output: num(Some(u), &["output_tokens"]).unwrap_or(0),
        cache_read: num(Some(u), &["input_tokens_details", "cached_tokens"]).unwrap_or(0),
        cache_write: num(Some(u), &["input_tokens_details", "cache_write_tokens"]),
        reasoning: num(Some(u), &["output_tokens_details", "reasoning_tokens"]),
    }
}

/// Chat `reasoning_details` entries as reasoning blocks.
pub(crate) fn chat_details(list: &[Value]) -> Vec<Out> {
    list.iter()
        .map(|d| {
            let family = str_of(d, "format").map_or(Family::None, Family::parse);
            let text = str_of(d, "text").unwrap_or_default().to_owned();
            match str_of(d, "type") {
                Some("reasoning.encrypted") if family == Family::Anthropic => Out::Reasoning(OutReasoning {
                    text,
                    signature: None,
                    redacted: str_of(d, "data").map(str::to_owned),
                    family,
                }),
                Some("reasoning.encrypted") => Out::Reasoning(OutReasoning {
                    text,
                    signature: str_of(d, "data").map(str::to_owned),
                    redacted: None,
                    family,
                }),
                _ => Out::Reasoning(OutReasoning {
                    text,
                    signature: str_of(d, "signature")
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned),
                    redacted: None,
                    family,
                }),
            }
        })
        .collect()
}

fn chat(body: &Value, cx: &Cx<'_>) -> Result<Response, Bad> {
    let choices = body
        .get("choices")
        .and_then(Value::as_array)
        .ok_or(Bad::Unusable)?;
    let mut candidates = Vec::new();
    for ch in choices {
        let msg = ch.get("message").cloned().unwrap_or(Value::Null);
        let mut blocks = Vec::new();
        if let Some(Value::Array(details)) = msg.get("reasoning_details") {
            blocks.extend(chat_details(details));
        } else if let Some(t) = str_of(&msg, "reasoning_content").or_else(|| str_of(&msg, "reasoning"))
            && !t.is_empty()
        {
            blocks.push(Out::Reasoning(OutReasoning {
                text: t.to_owned(),
                signature: None,
                redacted: None,
                family: Family::None,
            }));
        }
        match msg.get("content") {
            Some(Value::String(s)) if !s.is_empty() => blocks.push(Out::Text(s.clone())),
            Some(Value::Array(parts)) => {
                for p in parts {
                    if let Some(t) = str_of(p, "text").filter(|t| !t.is_empty()) {
                        blocks.push(Out::Text(t.to_owned()));
                    }
                }
            }
            _ => {}
        }
        if let Some(r) = str_of(&msg, "refusal").filter(|r| !r.is_empty()) {
            blocks.push(Out::Refusal(r.to_owned()));
        }
        for tc in msg
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let f = tc.get("function");
            let sig = tc
                .pointer("/extra_content/google/thought_signature")
                .and_then(Value::as_str)
                .map(str::to_owned);
            blocks.push(call(
                cx,
                str_of(tc, "id").unwrap_or_default(),
                f.and_then(|f| str_of(f, "name")).unwrap_or_default(),
                args_of(f.and_then(|f| f.get("arguments"))),
                sig,
            ));
        }
        let stop = str_of(ch, "finish_reason").map_or(Stop::EndTurn, Stop::from_chat);
        let cites = msg
            .get("annotations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|a| from_annotation(a, 0))
            .collect();
        candidates.push(Candidate {
            blocks,
            stop,
            stop_sequence: None,
            cites,
            searches: Vec::new(),
        });
    }
    Ok(Response {
        id: str_of(body, "id").map(str::to_owned),
        created: body.get("created").and_then(Value::as_i64),
        candidates,
        usage: body.get("usage").filter(|u| u.is_object()).map(chat_usage),
    })
}

/// The citations and web searches of a Responses output, and the characters of its text read so far.
#[derive(Default)]
pub(crate) struct Grounding {
    pub(crate) cites: Vec<Cite>,
    pub(crate) searches: Vec<Search>,
    pub(crate) offset: usize,
}

/// Responses output items as blocks, with the citations of their text and the web searches; other
/// item types are reported as dropped.
pub(crate) fn responses_item(
    item: &Value,
    cx: &Cx<'_>,
    blocks: &mut Vec<Out>,
    grounding: &mut Grounding,
    w: &mut Warnings,
) {
    match str_of(item, "type") {
        Some("web_search_call") => grounding.searches.push(Search {
            id: str_of(item, "id").unwrap_or_default().to_owned(),
            query: item
                .pointer("/action/query")
                .and_then(Value::as_str)
                .map(str::to_owned),
        }),
        Some("message") => {
            for p in item
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                match str_of(p, "type") {
                    Some("output_text") => {
                        for a in p
                            .get("annotations")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                        {
                            grounding.cites.extend(from_annotation(a, grounding.offset));
                        }
                        if let Some(t) = str_of(p, "text").filter(|t| !t.is_empty()) {
                            grounding.offset += t.chars().count();
                            blocks.push(Out::Text(t.to_owned()));
                        }
                    }
                    Some("refusal") => {
                        blocks.push(Out::Refusal(str_of(p, "refusal").unwrap_or_default().to_owned()))
                    }
                    _ => {}
                }
            }
        }
        Some("function_call") => blocks.push(call(
            cx,
            str_of(item, "call_id").unwrap_or_default(),
            str_of(item, "name").unwrap_or_default(),
            args_of(item.get("arguments")),
            None,
        )),
        Some("reasoning") => {
            let text = item
                .get("summary")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|s| str_of(s, "text"))
                .collect::<Vec<_>>()
                .join("\n\n");
            blocks.push(Out::Reasoning(OutReasoning {
                text,
                signature: str_of(item, "encrypted_content")
                    .filter(|e| !e.is_empty())
                    .map(str::to_owned),
                redacted: None,
                family: Family::OpenAi,
            }));
        }
        Some(other) => w.response(Kind::Dropped, &format!("output[{other}]")),
        None => {}
    }
}

fn responses(body: &Value, cx: &Cx<'_>, w: &mut Warnings) -> Result<Response, Bad> {
    let output = body
        .get("output")
        .and_then(Value::as_array)
        .ok_or(Bad::Unusable)?;
    let mut blocks = Vec::new();
    let mut grounding = Grounding::default();
    for item in output {
        responses_item(item, cx, &mut blocks, &mut grounding, w);
    }
    let Grounding { cites, searches, .. } = grounding;
    let has_call = blocks.iter().any(|b| matches!(b, Out::Call(_)));
    let status = str_of(body, "status").unwrap_or("completed");
    if status == "failed" {
        return Err(Bad::Unusable);
    }
    let reason = body.get("incomplete_details").and_then(|d| str_of(d, "reason"));
    Ok(Response {
        id: str_of(body, "id").map(str::to_owned),
        created: body.get("created_at").and_then(Value::as_i64),
        candidates: vec![Candidate {
            blocks,
            stop: Stop::from_responses(status, reason, has_call),
            stop_sequence: None,
            cites,
            searches,
        }],
        usage: body.get("usage").filter(|u| u.is_object()).map(responses_usage),
    })
}
