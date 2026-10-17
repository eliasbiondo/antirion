//! Chat Completions bodies, for Anthropic and Responses clients.

use serde_json::{Map, Value, json};

use super::Enc;
use super::media::Resolved;
use super::sampling::Structured;
use crate::xlate::ir::{Block, Call, Choice, Role};
use crate::xlate::util::{compact, put};
use crate::xlate::warn::Kind;

/// The arguments text of a call: the client's own text when it sent one.
pub(super) fn args_text(c: &Call) -> String {
    match (&c.raw, &c.args) {
        (Some(raw), _) => raw.clone(),
        (None, Some(a)) => compact(a),
        (None, None) => "{}".to_owned(),
    }
}

pub(super) fn encode(e: &mut Enc<'_>) -> Value {
    let req = e.req;
    let mut out = Map::new();
    out.insert("model".into(), Value::from(e.t.model.as_str()));
    out.insert("messages".into(), Value::Array(messages(e)));
    let tools = tools(e);
    if !tools.is_empty() {
        out.insert("tools".into(), Value::Array(tools));
    }
    if let Some(w) = e.web_search() {
        out.insert("web_search_options".into(), super::hosted::chat_options(w));
    }
    put(&mut out, "tool_choice", choice(e).unwrap_or(Value::Null));
    if let Some((p, _)) = &req.parallel {
        out.insert("parallel_tool_calls".into(), (*p).into());
    }
    if let Some((v, _)) = &req.max_tokens {
        out.insert(e.t.chat_max_field.into(), (*v).into());
    }
    put(
        &mut out,
        "temperature",
        e.temperature(false).unwrap_or(Value::Null),
    );
    let sent = out.contains_key("temperature");
    put(&mut out, "top_p", e.top_p(false, sent).unwrap_or(Value::Null));
    e.drop_field(req.top_k.as_ref());
    if let Some(stop) = e.stop() {
        out.insert("stop".into(), stop.into());
    }
    for (k, n) in [
        ("frequency_penalty", &req.frequency_penalty),
        ("presence_penalty", &req.presence_penalty),
        ("seed", &req.seed),
        ("logit_bias", &req.logit_bias),
        ("logprobs", &req.logprobs),
        ("top_logprobs", &req.top_logprobs),
    ] {
        if let Some(n) = n {
            out.insert(k.into(), n.value.clone());
        }
    }
    if let Some((n, _)) = &req.n {
        out.insert("n".into(), (*n).into());
    }
    match e.structured(true) {
        Structured::Native(Some(schema)) => {
            let mut js = Map::new();
            js.insert("name".into(), e.format_name().into());
            js.insert("schema".into(), schema);
            if let Some(s) = e.format_strict() {
                js.insert("strict".into(), s.into());
            }
            out.insert(
                "response_format".into(),
                json!({"type": "json_schema", "json_schema": js}),
            );
        }
        Structured::Native(None) => {
            out.insert("response_format".into(), json!({"type": "json_object"}));
        }
        _ => {}
    }
    put(
        &mut out,
        "reasoning_effort",
        e.effort().map_or(Value::Null, Value::from),
    );
    if let Some((user, _)) = &req.end_user {
        out.insert("user".into(), user.as_str().into());
    }
    if e.openai_client() {
        for i in &req.inert {
            match i.name {
                "metadata" | "store" | "service_tier" | "prompt_cache_key" => {
                    out.insert(i.name.into(), i.value.clone());
                }
                "text.verbosity" => {
                    out.insert("verbosity".into(), i.value.clone());
                }
                _ => {}
            }
        }
    }
    if req.stream {
        out.insert("stream".into(), true.into());
        if e.t.stream_usage {
            out.insert("stream_options".into(), json!({"include_usage": true}));
        }
    }
    Value::Object(out)
}

pub(super) fn tools(e: &mut Enc<'_>) -> Vec<Value> {
    let req = e.req;
    let mut out = Vec::new();
    for t in &req.tools {
        let mut f = Map::new();
        f.insert("name".into(), e.names.up(&t.name).into());
        put(
            &mut f,
            "description",
            t.description.clone().map_or(Value::Null, Value::from),
        );
        f.insert(
            "parameters".into(),
            t.parameters
                .clone()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
        );
        if let Some((s, _)) = &t.strict {
            f.insert("strict".into(), (*s).into());
        }
        cache_marker(e, &mut f, t.cache.as_ref());
        out.push(json!({"type": "function", "function": f}));
    }
    out
}

fn choice(e: &mut Enc<'_>) -> Option<Value> {
    let (choice, _) = e.req.choice.as_ref()?;
    Some(match choice {
        Choice::Auto => "auto".into(),
        Choice::None => "none".into(),
        Choice::Required => "required".into(),
        Choice::Named(n) => json!({"type": "function", "function": {"name": e.names.up(n)}}),
        Choice::Allowed { required, names } => {
            let tools: Vec<Value> = names
                .iter()
                .map(|n| json!({"type": "function", "function": {"name": e.names.up(n)}}))
                .collect();
            json!({"type": "allowed_tools", "allowed_tools": {"mode": if *required { "required" } else { "auto" }, "tools": tools}})
        }
    })
}

/// A cache marker kept in its Chat extension position, or reported as ignored.
fn cache_marker(
    e: &mut Enc<'_>,
    o: &mut Map<String, Value>,
    cache: Option<&crate::xlate::ir::Cache>,
) -> bool {
    match cache {
        Some(c) if e.t.accepts_cache_control => {
            o.insert("cache_control".into(), c.value.clone());
            true
        }
        other => {
            e.drop_cache(other);
            false
        }
    }
}

pub(super) fn messages(e: &mut Enc<'_>) -> Vec<Value> {
    let req = e.req;
    let mut out = Vec::new();
    for s in &req.system {
        let role = if s.developer && e.t.developer_role {
            "developer"
        } else {
            "system"
        };
        let mut part = Map::new();
        part.insert("type".into(), "text".into());
        part.insert("text".into(), s.text.as_str().into());
        let content = if cache_marker(e, &mut part, s.cache.as_ref()) {
            Value::Array(vec![Value::Object(part)])
        } else {
            Value::from(s.text.as_str())
        };
        out.push(json!({"role": role, "content": content}));
    }
    let mut deferred: Vec<Value> = Vec::new();
    for (role, blocks, _) in e.plain_turns() {
        if role != Role::Tool && !deferred.is_empty() {
            out.push(json!({"role": "user", "content": std::mem::take(&mut deferred)}));
        }
        match role {
            Role::Tool => {
                for b in &blocks {
                    if let Block::ToolResult(r) = b {
                        out.push(tool_message(e, r, &mut deferred));
                    }
                }
            }
            Role::Assistant => out.push(assistant(e, &blocks)),
            Role::System | Role::Developer => {
                let role = if role == Role::Developer && e.t.developer_role {
                    "developer"
                } else {
                    "system"
                };
                out.push(json!({"role": role, "content": content(e, &blocks)}));
            }
            Role::User => out.push(json!({"role": "user", "content": content(e, &blocks)})),
        }
    }
    if !deferred.is_empty() {
        out.push(json!({"role": "user", "content": deferred}));
    }
    out
}

fn tool_message(e: &mut Enc<'_>, r: &crate::xlate::ir::ToolResult, deferred: &mut Vec<Value>) -> Value {
    if let Some((_, src)) = &r.is_error {
        e.warn(Kind::Ignored, src);
    }
    let (texts, media): (Vec<&Block>, Vec<&Block>) =
        r.content.iter().partition(|b| matches!(b, Block::Text { .. }));
    if !media.is_empty() {
        e.warn(Kind::Approximated, &r.content_src);
        deferred.push(json!({"type": "text", "text": format!("[content of tool result {}]", r.id)}));
        for b in media {
            deferred.extend(part(e, b));
        }
    }
    let texts: Vec<Block> = texts.into_iter().cloned().collect();
    let mut msg = Map::new();
    msg.insert("role".into(), "tool".into());
    msg.insert("tool_call_id".into(), r.id.as_str().into());
    msg.insert("content".into(), content(e, &texts));
    cache_marker(e, &mut msg, r.cache.as_ref());
    Value::Object(msg)
}

fn assistant(e: &mut Enc<'_>, blocks: &[Block]) -> Value {
    let mut msg = Map::new();
    msg.insert("role".into(), "assistant".into());
    let texts: Vec<&String> = blocks
        .iter()
        .filter_map(|b| match b {
            Block::Text { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    let calls: Vec<&Call> = blocks
        .iter()
        .filter_map(|b| match b {
            Block::ToolCall(c) => Some(c),
            _ => None,
        })
        .collect();
    let content = match texts.as_slice() {
        [] if !calls.is_empty() => Value::Null,
        [] => Value::from(""),
        [one] => Value::from(one.as_str()),
        many => Value::Array(many.iter().map(|t| json!({"type": "text", "text": t})).collect()),
    };
    msg.insert("content".into(), content);
    for b in blocks {
        if let Block::Refusal { text } = b {
            msg.insert("refusal".into(), text.as_str().into());
        }
        if let Block::Text { cache, .. } = b {
            e.drop_cache(cache.as_ref());
        }
    }
    if !calls.is_empty() {
        let list: Vec<Value> = calls
            .iter()
            .map(|c| {
                let mut tc = Map::new();
                tc.insert("id".into(), c.id.as_str().into());
                tc.insert("type".into(), "function".into());
                tc.insert(
                    "function".into(),
                    json!({"name": e.names.up(&c.name), "arguments": args_text(c)}),
                );
                cache_marker(e, &mut tc, c.cache.as_ref());
                Value::Object(tc)
            })
            .collect();
        msg.insert("tool_calls".into(), Value::Array(list));
    }
    Value::Object(msg)
}

/// User, tool and system content: a string for one plain text part, else parts.
fn content(e: &mut Enc<'_>, blocks: &[Block]) -> Value {
    if let [Block::Text { text, cache, .. }] = blocks
        && !(cache.is_some() && e.t.accepts_cache_control)
    {
        e.drop_cache(cache.as_ref());
        return Value::from(text.as_str());
    }
    Value::Array(blocks.iter().filter_map(|b| part(e, b)).collect())
}

fn part(e: &mut Enc<'_>, b: &Block) -> Option<Value> {
    let mut o = Map::new();
    let cache = match b {
        Block::Text { text, cache, .. } => {
            o.insert("type".into(), "text".into());
            o.insert("text".into(), text.as_str().into());
            cache.as_ref()
        }
        Block::Refusal { text } => {
            o.insert("type".into(), "text".into());
            o.insert("text".into(), text.as_str().into());
            None
        }
        Block::Image {
            media,
            detail,
            cache,
            src,
            url_src,
        } => {
            let url = match e.resolve(media, true, src, url_src)? {
                Resolved::Bytes { mime, data } => format!("data:{mime};base64,{data}"),
                Resolved::Url { url, .. } => url,
                Resolved::Text { .. } => return None,
            };
            let mut img = Map::new();
            img.insert("url".into(), url.into());
            if let Some((d, _)) = detail {
                img.insert("detail".into(), d.as_str().into());
            }
            o.insert("type".into(), "image_url".into());
            o.insert("image_url".into(), Value::Object(img));
            cache.as_ref()
        }
        Block::Document {
            media,
            name,
            cache,
            src,
            url_src,
        } => {
            let mut f = Map::new();
            match e.resolve(media, false, src, url_src)? {
                Resolved::Bytes { mime, data } => {
                    f.insert("file_data".into(), format!("data:{mime};base64,{data}").into());
                }
                Resolved::Text { mime, text } => {
                    f.insert(
                        "file_data".into(),
                        format!(
                            "data:{mime};base64,{}",
                            crate::xlate::util::b64_encode(text.as_bytes())
                        )
                        .into(),
                    );
                }
                Resolved::Url { url, .. } => {
                    f.insert("file_data".into(), url.into());
                }
            }
            if let Some((n, _)) = name {
                f.insert("filename".into(), n.as_str().into());
            }
            o.insert("type".into(), "file".into());
            o.insert("file".into(), Value::Object(f));
            cache.as_ref()
        }
        Block::Audio { data, format, src } => {
            if !e.audio_ok(format, src) {
                return None;
            }
            o.insert("type".into(), "input_audio".into());
            o.insert("input_audio".into(), json!({"data": data, "format": format}));
            None
        }
        Block::ToolCall(_) | Block::ToolResult(_) | Block::Reasoning(_) => return None,
    };
    cache_marker(e, &mut o, cache);
    Some(Value::Object(o))
}
