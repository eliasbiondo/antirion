//! OpenAI Responses bodies, for Chat and Anthropic clients.

use serde_json::{Map, Value, json};

use super::Enc;
use super::chat::args_text;
use super::media::Resolved;
use super::sampling::Structured;
use crate::xlate::ir::{Block, Choice, Family, Role};
use crate::xlate::util::put;
use crate::xlate::warn::Kind;

/// The smallest `max_output_tokens` Responses accepts.
const MIN_OUTPUT: u64 = 16;

pub(super) fn encode(e: &mut Enc<'_>) -> Value {
    let req = e.req;
    let mut out = Map::new();
    out.insert("model".into(), Value::from(e.t.model.as_str()));
    if !req.system.is_empty() {
        let joined: Vec<&str> = req.system.iter().map(|s| s.text.as_str()).collect();
        for s in &req.system {
            e.drop_cache(s.cache.as_ref());
        }
        out.insert("instructions".into(), joined.join("\n\n").into());
    }
    out.insert("input".into(), Value::Array(items(e)));
    let mut tools = tools(e);
    if let Some(w) = e.web_search() {
        tools.push(super::hosted::responses_tool(w));
    }
    if !tools.is_empty() {
        out.insert("tools".into(), Value::Array(tools));
    }
    put(&mut out, "tool_choice", choice(e).unwrap_or(Value::Null));
    if let Some((p, _)) = &req.parallel {
        out.insert("parallel_tool_calls".into(), (*p).into());
    }
    if let Some((v, src)) = &req.max_tokens {
        let v = if *v < MIN_OUTPUT {
            e.warn(Kind::Clamped, src);
            MIN_OUTPUT
        } else {
            *v
        };
        out.insert("max_output_tokens".into(), v.into());
    }
    put(
        &mut out,
        "temperature",
        e.temperature(false).unwrap_or(Value::Null),
    );
    let sent = out.contains_key("temperature");
    put(&mut out, "top_p", e.top_p(false, sent).unwrap_or(Value::Null));
    e.stop();
    for n in [
        &req.top_k,
        &req.frequency_penalty,
        &req.presence_penalty,
        &req.seed,
        &req.logit_bias,
    ] {
        e.drop_field(n.as_ref());
    }
    if let Some(n) = &req.top_logprobs {
        out.insert("top_logprobs".into(), n.value.clone());
    }
    let mut text = Map::new();
    match e.structured(true) {
        Structured::Native(Some(schema)) => {
            let mut f = Map::new();
            f.insert("type".into(), "json_schema".into());
            f.insert("name".into(), e.format_name().into());
            f.insert("schema".into(), schema);
            if let Some(s) = e.format_strict() {
                f.insert("strict".into(), s.into());
            }
            text.insert("format".into(), Value::Object(f));
        }
        Structured::Native(None) => {
            text.insert("format".into(), json!({"type": "json_object"}));
        }
        _ => {}
    }
    let mut reasoning = Map::new();
    put(
        &mut reasoning,
        "effort",
        e.effort().map_or(Value::Null, Value::from),
    );
    if req.reason.summaries && req.reason.include_thoughts == Some(true) {
        reasoning.insert("summary".into(), "auto".into());
    }
    if !reasoning.is_empty() {
        out.insert("reasoning".into(), Value::Object(reasoning));
    }
    if let Some((user, _)) = &req.end_user {
        out.insert("safety_identifier".into(), user.as_str().into());
    }
    let mut store = false;
    if e.openai_client() {
        for i in &req.inert {
            match i.name {
                "metadata" | "service_tier" | "prompt_cache_key" => {
                    out.insert(i.name.into(), i.value.clone());
                }
                "store" => store = i.value == Value::Bool(true),
                "verbosity" => {
                    text.insert("verbosity".into(), i.value.clone());
                }
                _ => {}
            }
        }
    }
    if !text.is_empty() {
        out.insert("text".into(), Value::Object(text));
    }
    out.insert("store".into(), store.into());
    if e.t.family == Family::OpenAi {
        out.insert("include".into(), json!(["reasoning.encrypted_content"]));
    }
    if req.stream {
        out.insert("stream".into(), true.into());
    }
    Value::Object(out)
}

fn tools(e: &mut Enc<'_>) -> Vec<Value> {
    let req = e.req;
    let mut out = Vec::new();
    for t in &req.tools {
        let mut o = Map::new();
        o.insert("type".into(), "function".into());
        o.insert("name".into(), e.names.up(&t.name).into());
        put(
            &mut o,
            "description",
            t.description.clone().map_or(Value::Null, Value::from),
        );
        o.insert(
            "parameters".into(),
            t.parameters
                .clone()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
        );
        if let Some((s, _)) = &t.strict {
            o.insert("strict".into(), (*s).into());
        }
        e.drop_cache(t.cache.as_ref());
        out.push(Value::Object(o));
    }
    out
}

fn choice(e: &mut Enc<'_>) -> Option<Value> {
    let (choice, _) = e.req.choice.as_ref()?;
    Some(match choice {
        Choice::Auto => "auto".into(),
        Choice::None => "none".into(),
        Choice::Required => "required".into(),
        Choice::Named(n) => json!({"type": "function", "name": e.names.up(n)}),
        Choice::Allowed { required, names } => {
            let tools: Vec<Value> = names
                .iter()
                .map(|n| json!({"type": "function", "name": e.names.up(n)}))
                .collect();
            json!({"type": "allowed_tools", "mode": if *required { "required" } else { "auto" }, "tools": tools})
        }
    })
}

fn items(e: &mut Enc<'_>) -> Vec<Value> {
    let mut out = Vec::new();
    for (role, blocks, _) in e.plain_turns() {
        match role {
            Role::Tool => {
                for b in &blocks {
                    if let Block::ToolResult(r) = b {
                        if let Some((_, s)) = &r.is_error {
                            e.warn(Kind::Ignored, s);
                        }
                        e.drop_cache(r.cache.as_ref());
                        let output = match r.content.as_slice() {
                            [Block::Text { text, .. }] => Value::from(text.as_str()),
                            many => Value::Array(many.iter().filter_map(|b| part(e, b, false)).collect()),
                        };
                        out.push(json!({"type": "function_call_output", "call_id": r.id, "output": output}));
                    }
                }
            }
            Role::Assistant => assistant(e, &blocks, &mut out),
            other => {
                let role = match other {
                    Role::System => "system",
                    Role::Developer => "developer",
                    _ => "user",
                };
                let content: Vec<Value> = blocks.iter().filter_map(|b| part(e, b, false)).collect();
                out.push(json!({"type": "message", "role": role, "content": content}));
            }
        }
    }
    out
}

fn assistant(e: &mut Enc<'_>, blocks: &[Block], out: &mut Vec<Value>) {
    let mut message: Vec<Value> = Vec::new();
    let flush = |message: &mut Vec<Value>, out: &mut Vec<Value>| {
        if !message.is_empty() {
            out.push(json!({"type": "message", "role": "assistant", "content": std::mem::take(message)}));
        }
    };
    for b in blocks {
        match b {
            Block::Reasoning(r) => {
                flush(&mut message, out);
                let summary: Vec<Value> = if r.text.is_empty() {
                    Vec::new()
                } else {
                    vec![json!({"type": "summary_text", "text": r.text})]
                };
                let mut item = Map::new();
                item.insert("type".into(), "reasoning".into());
                item.insert("summary".into(), Value::Array(summary));
                if let Some(enc) = r.signature.as_ref().or(r.redacted.as_ref()) {
                    item.insert("encrypted_content".into(), enc.as_str().into());
                }
                out.push(Value::Object(item));
            }
            Block::ToolCall(c) => {
                flush(&mut message, out);
                out.push(
                    json!({"type": "function_call", "call_id": c.id, "name": e.names.up(&c.name),
                                "arguments": args_text(c)}),
                );
            }
            other => message.extend(part(e, other, true)),
        }
    }
    flush(&mut message, out);
}

fn part(e: &mut Enc<'_>, b: &Block, output: bool) -> Option<Value> {
    match b {
        Block::Text { text, cache, .. } => {
            e.drop_cache(cache.as_ref());
            let kind = if output { "output_text" } else { "input_text" };
            Some(json!({"type": kind, "text": text}))
        }
        Block::Refusal { text } => Some(json!({"type": "refusal", "refusal": text})),
        Block::Image {
            media,
            detail,
            cache,
            src,
            url_src,
        } => {
            e.drop_cache(cache.as_ref());
            let url = match e.resolve(media, true, src, url_src)? {
                Resolved::Bytes { mime, data } => format!("data:{mime};base64,{data}"),
                Resolved::Url { url, .. } => url,
                Resolved::Text { .. } => return None,
            };
            let mut o = Map::new();
            o.insert("type".into(), "input_image".into());
            o.insert("image_url".into(), url.into());
            if let Some((d, _)) = detail {
                o.insert("detail".into(), d.as_str().into());
            }
            Some(Value::Object(o))
        }
        Block::Document {
            media,
            name,
            cache,
            src,
            url_src,
        } => {
            e.drop_cache(cache.as_ref());
            let mut o = Map::new();
            o.insert("type".into(), "input_file".into());
            match e.resolve(media, false, src, url_src)? {
                Resolved::Bytes { mime, data } => {
                    o.insert("file_data".into(), format!("data:{mime};base64,{data}").into());
                }
                Resolved::Url { url, .. } => {
                    o.insert("file_url".into(), url.into());
                }
                Resolved::Text { mime, text } => {
                    o.insert(
                        "file_data".into(),
                        format!(
                            "data:{mime};base64,{}",
                            crate::xlate::util::b64_encode(text.as_bytes())
                        )
                        .into(),
                    );
                }
            }
            if let Some((n, _)) = name {
                o.insert("filename".into(), n.as_str().into());
            }
            Some(Value::Object(o))
        }
        Block::Audio { format, src, .. } => {
            e.audio_ok(format, src);
            None
        }
        Block::ToolCall(_) | Block::ToolResult(_) | Block::Reasoning(_) => None,
    }
}
