//! Anthropic Messages bodies (any transport).

use serde_json::{Map, Value, json};

use super::media::Resolved;
use super::reason::Thinking;
use super::sampling::Structured;
use super::turns::NTurn;
use super::{Enc, JSON_TOOL};
use crate::xlate::ir::{Block, Choice, Role};
use crate::xlate::util::{obj, put, text_payload};
use crate::xlate::warn::Kind;

pub(super) fn encode(e: &mut Enc<'_>) -> Value {
    e.plan_cache();
    let req = e.req;
    let mut out = Map::new();
    let max_tokens = e.anthropic_max_tokens();
    out.insert("model".into(), Value::from(e.t.model.as_str()));
    out.insert("max_tokens".into(), max_tokens.into());
    if let Some(system) = system(e) {
        out.insert("system".into(), system);
    }
    let turns = e.block_turns();
    let messages: Vec<Value> = turns.iter().map(|t| message(e, t)).collect();
    out.insert("messages".into(), Value::Array(messages));
    let structured = e.structured(e.t.native_structured);
    let mut tools = tools(e);
    let mut choice = tool_choice(e, &mut tools);
    if let Some(w) = e.web_search() {
        tools.push(super::hosted::anthropic_tool(w));
    }
    let mut output_format = None;
    match &structured {
        Structured::Native(schema) => {
            output_format = Some(json!({"type": "json_schema", "schema": schema}));
        }
        Structured::Emulated(schema) => {
            tools.push(json!({"name": JSON_TOOL, "description": "Respond with the answer as this tool's input.", "input_schema": schema}));
            choice = Some(json!({"type": "tool", "name": JSON_TOOL}));
        }
        Structured::None => {}
    }
    let forced = choice
        .as_ref()
        .and_then(|c| c.get("type"))
        .and_then(Value::as_str)
        .is_some_and(|t| t == "any" || t == "tool");
    let (mut thinking, effort) = e.anthropic_thinking(max_tokens);
    if thinking.on() && (forced || e.unsigned_tool_loop()) {
        let src = req.reason.src.clone();
        e.warn(Kind::Dropped, &src);
        thinking = Thinking::Omitted;
    }
    if !tools.is_empty() {
        out.insert("tools".into(), Value::Array(tools));
    }
    put(&mut out, "tool_choice", choice.unwrap_or(Value::Null));
    if let Some(stop) = e.stop() {
        out.insert("stop_sequences".into(), stop.into());
    }
    let on = thinking.on();
    let temperature = e.temperature(on);
    let sent = temperature.is_some();
    put(&mut out, "temperature", temperature.unwrap_or(Value::Null));
    put(&mut out, "top_p", e.top_p(on, sent).unwrap_or(Value::Null));
    put(&mut out, "top_k", e.top_k(on).unwrap_or(Value::Null));
    for n in [
        &req.frequency_penalty,
        &req.presence_penalty,
        &req.seed,
        &req.logit_bias,
        &req.top_logprobs,
    ] {
        e.drop_field(n.as_ref());
    }
    if let Some((user, _)) = &req.end_user {
        out.insert("metadata".into(), json!({"user_id": user}));
    }
    put(&mut out, "thinking", thinking.value(req.reason.summaries));
    let mut config = Map::new();
    if let Some(format) = output_format {
        config.insert("format".into(), format);
    }
    if let Some(effort) = effort.filter(|_| !matches!(thinking, Thinking::Omitted | Thinking::Disabled)) {
        config.insert("effort".into(), effort.into());
    }
    if !config.is_empty() {
        out.insert("output_config".into(), Value::Object(config));
    }
    if req.stream {
        out.insert("stream".into(), true.into());
    }
    if let Some(c) = &req.auto_cache {
        out.insert("cache_control".into(), c.value.clone());
    }
    Value::Object(out)
}

fn system(e: &mut Enc<'_>) -> Option<Value> {
    let blocks = &e.req.system;
    if blocks.is_empty() {
        return None;
    }
    if let [only] = blocks.as_slice()
        && e.cache_value(only.cache.as_ref()).is_none()
    {
        return Some(Value::from(only.text.as_str()));
    }
    Some(Value::Array(
        blocks
            .iter()
            .map(|b| {
                let mut o = Map::new();
                o.insert("type".into(), "text".into());
                o.insert("text".into(), b.text.as_str().into());
                put(
                    &mut o,
                    "cache_control",
                    e.cache_value(b.cache.as_ref()).unwrap_or(Value::Null),
                );
                Value::Object(o)
            })
            .collect(),
    ))
}

fn tools(e: &mut Enc<'_>) -> Vec<Value> {
    let req = e.req;
    let mut out = Vec::new();
    for t in &req.tools {
        let mut o = Map::new();
        o.insert("name".into(), e.names.up(&t.name).into());
        put(
            &mut o,
            "description",
            t.description.clone().map_or(Value::Null, Value::from),
        );
        o.insert(
            "input_schema".into(),
            t.parameters
                .clone()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
        );
        if let Some((strict, src)) = &t.strict {
            if e.t.native_structured {
                o.insert("strict".into(), (*strict).into());
            } else {
                e.warn(Kind::Ignored, src);
            }
        }
        put(
            &mut o,
            "cache_control",
            e.cache_value(t.cache.as_ref()).unwrap_or(Value::Null),
        );
        out.push(Value::Object(o));
    }
    out
}

/// `tool_choice`, reducing the tool list for allowed tools.
fn tool_choice(e: &mut Enc<'_>, tools: &mut Vec<Value>) -> Option<Value> {
    let req = e.req;
    let parallel_off = req.parallel.as_ref().is_some_and(|(p, _)| !p);
    let mut choice = match &req.choice {
        Some((Choice::Auto, _)) => Some(json!({"type": "auto"})),
        Some((Choice::None, _)) => Some(json!({"type": "none"})),
        Some((Choice::Required, _)) => Some(json!({"type": "any"})),
        Some((Choice::Named(n), _)) => Some(json!({"type": "tool", "name": e.names.up(n)})),
        Some((Choice::Allowed { required, names }, src)) => {
            let sent: Vec<String> = names.iter().map(|n| e.names.up(n)).collect();
            tools.retain(|t| {
                t.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| sent.iter().any(|s| s == n))
            });
            e.warn(Kind::Approximated, src);
            Some(json!({"type": if *required { "any" } else { "auto" }}))
        }
        None => None,
    };
    if parallel_off {
        let c = choice.get_or_insert_with(|| json!({"type": "auto"}));
        if let Some(o) = c.as_object_mut()
            && o.get("type").and_then(Value::as_str) != Some("none")
        {
            o.insert("disable_parallel_tool_use".into(), true.into());
        }
    }
    choice
}

fn message(e: &mut Enc<'_>, t: &NTurn) -> Value {
    let mut first = Vec::new();
    let mut rest = Vec::new();
    for b in &t.blocks {
        let is_reasoning = matches!(b, Block::Reasoning(_));
        if let Some(v) = block(e, b) {
            if is_reasoning && t.role == Role::Assistant {
                first.push(v);
            } else {
                rest.push(v);
            }
        }
    }
    first.extend(rest);
    obj([
        (
            "role",
            if t.role == Role::Assistant {
                "assistant"
            } else {
                "user"
            }
            .into(),
        ),
        ("content", Value::Array(first)),
    ])
}

fn with_cache(e: &Enc<'_>, mut v: Value, cache: Option<&crate::xlate::ir::Cache>) -> Value {
    if let (Some(c), Some(o)) = (e.cache_value(cache), v.as_object_mut()) {
        o.insert("cache_control".into(), c);
    }
    v
}

pub(super) fn block(e: &mut Enc<'_>, b: &Block) -> Option<Value> {
    match b {
        Block::Text { text, cache, .. } => Some(with_cache(
            e,
            json!({"type": "text", "text": text}),
            cache.as_ref(),
        )),
        Block::Refusal { text } => Some(json!({"type": "text", "text": text})),
        Block::Image {
            media,
            detail,
            cache,
            src,
            url_src,
        } => {
            if let Some((_, dsrc)) = detail {
                e.warn(Kind::Ignored, dsrc);
            }
            let source = match e.resolve(media, true, src, url_src)? {
                Resolved::Bytes { mime, data } => json!({"type": "base64", "media_type": mime, "data": data}),
                Resolved::Url { url, .. } => json!({"type": "url", "url": url}),
                Resolved::Text { .. } => return None,
            };
            Some(with_cache(
                e,
                json!({"type": "image", "source": source}),
                cache.as_ref(),
            ))
        }
        Block::Document {
            media,
            name,
            cache,
            src,
            url_src,
        } => {
            let source = match e.resolve(media, false, src, url_src)? {
                Resolved::Bytes { mime, data } if mime == "text/plain" => {
                    json!({"type": "text", "media_type": "text/plain", "data": text_payload(&data).unwrap_or_default()})
                }
                Resolved::Bytes { mime, data } => json!({"type": "base64", "media_type": mime, "data": data}),
                Resolved::Url { url, .. } => json!({"type": "url", "url": url}),
                Resolved::Text { mime, text } => json!({"type": "text", "media_type": mime, "data": text}),
            };
            let mut o = Map::new();
            o.insert("type".into(), "document".into());
            o.insert("source".into(), source);
            if let Some((n, _)) = name {
                o.insert("title".into(), n.as_str().into());
            }
            Some(with_cache(e, Value::Object(o), cache.as_ref()))
        }
        Block::Audio { format, src, .. } => {
            e.audio_ok(format, src);
            None
        }
        Block::ToolCall(c) => {
            let input = match &c.args {
                Some(a) => a.clone(),
                None => {
                    e.warn(Kind::Approximated, &c.args_src);
                    json!({})
                }
            };
            let v =
                json!({"type": "tool_use", "id": e.id(&c.id), "name": e.names.up(&c.name), "input": input});
            Some(with_cache(e, v, c.cache.as_ref()))
        }
        Block::ToolResult(r) => {
            let content = result_content(e, &r.content);
            let mut o = Map::new();
            o.insert("type".into(), "tool_result".into());
            o.insert("tool_use_id".into(), e.id(&r.id).into());
            o.insert("content".into(), content);
            if let Some((err, _)) = &r.is_error {
                o.insert("is_error".into(), (*err).into());
            }
            Some(with_cache(e, Value::Object(o), r.cache.as_ref()))
        }
        Block::Reasoning(r) => {
            if let Some(data) = &r.redacted {
                Some(json!({"type": "redacted_thinking", "data": data}))
            } else {
                r.signature
                    .as_ref()
                    .map(|sig| json!({"type": "thinking", "thinking": r.text, "signature": sig}))
            }
        }
    }
}

fn result_content(e: &mut Enc<'_>, blocks: &[Block]) -> Value {
    if let [
        Block::Text {
            text, cache: None, ..
        },
    ] = blocks
    {
        return Value::from(text.as_str());
    }
    Value::Array(blocks.iter().filter_map(|b| block(e, b)).collect())
}
