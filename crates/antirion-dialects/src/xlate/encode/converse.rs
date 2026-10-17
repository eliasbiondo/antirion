//! Bedrock Converse bodies.

use serde_json::{Map, Value, json};

use super::media::Resolved;
use super::reason::Thinking;
use super::sampling::Structured;
use super::turns::NTurn;
use super::{Enc, JSON_TOOL, history_tool_names};
use crate::xlate::ir::{Block, Cache, Choice, Role};
use crate::xlate::media::{converse_document_format, converse_name, subtype};
use crate::xlate::util::{obj, put};
use crate::xlate::warn::{Kind, Src};

fn cache_point(c: &Cache) -> Value {
    if c.one_hour() {
        json!({"cachePoint": {"type": "default", "ttl": "1h"}})
    } else {
        json!({"cachePoint": {"type": "default"}})
    }
}

pub(super) fn encode(e: &mut Enc<'_>) -> Value {
    e.web_search();
    let req = e.req;
    let mut out = Map::new();
    let turns = e.block_turns();
    let mut docs = 0usize;
    let messages: Vec<Value> = turns.iter().map(|t| message(e, t, &mut docs)).collect();
    out.insert("messages".into(), Value::Array(messages));
    if !req.system.is_empty() {
        let mut system = Vec::new();
        for s in &req.system {
            system.push(json!({"text": s.text}));
            if let Some(c) = &s.cache {
                system.push(cache_point(c));
            }
        }
        out.insert("system".into(), Value::Array(system));
    }
    let structured = e.structured(false);
    let (tools, mut choice) = tool_config(e);
    let mut tools = tools;
    if let Structured::Emulated(schema) = &structured {
        tools.push(json!({"toolSpec": {"name": JSON_TOOL, "description": "Respond with the answer as this tool's input.",
                                       "inputSchema": {"json": schema}}}));
        choice = Some(json!({"tool": {"name": JSON_TOOL}}));
    }
    let claude = e.t.claude;
    let forced = choice
        .as_ref()
        .is_some_and(|c| c.get("any").is_some() || c.get("tool").is_some());
    let max_tokens = req.max_tokens.as_ref().map(|(v, _)| *v);
    let (mut thinking, _) = if claude {
        e.anthropic_thinking(max_tokens.unwrap_or(e.t.max_output_tokens.unwrap_or(e.t.default_max_tokens)))
    } else {
        if e.reasoning_asked() && !matches!(req.reason.mode, crate::xlate::ir::Mode::Off) {
            let src = req.reason.src.clone();
            e.warn(Kind::Dropped, &src);
        }
        (Thinking::Omitted, None)
    };
    if thinking.on() && (forced || e.unsigned_tool_loop()) {
        let src = req.reason.src.clone();
        e.warn(Kind::Dropped, &src);
        thinking = Thinking::Omitted;
    }
    if !tools.is_empty() {
        let mut config = Map::new();
        config.insert("tools".into(), Value::Array(tools));
        put(&mut config, "toolChoice", choice.unwrap_or(Value::Null));
        out.insert("toolConfig".into(), Value::Object(config));
    }
    let on = thinking.on();
    let mut inference = Map::new();
    if let Some(v) = max_tokens {
        inference.insert("maxTokens".into(), v.into());
    }
    let temperature = e.temperature(on && claude);
    let sent = temperature.is_some();
    put(&mut inference, "temperature", temperature.unwrap_or(Value::Null));
    put(
        &mut inference,
        "topP",
        e.top_p(on && claude, sent).unwrap_or(Value::Null),
    );
    if let Some(stop) = e.stop() {
        inference.insert("stopSequences".into(), stop.into());
    }
    if !inference.is_empty() {
        out.insert("inferenceConfig".into(), Value::Object(inference));
    }
    let mut additional = Map::new();
    if claude {
        put(&mut additional, "top_k", e.top_k(on).unwrap_or(Value::Null));
    } else {
        e.drop_field(req.top_k.as_ref());
    }
    put(&mut additional, "thinking", thinking.value(req.reason.summaries));
    if !additional.is_empty() {
        out.insert("additionalModelRequestFields".into(), Value::Object(additional));
    }
    for n in [
        &req.frequency_penalty,
        &req.presence_penalty,
        &req.seed,
        &req.logit_bias,
        &req.top_logprobs,
    ] {
        e.drop_field(n.as_ref());
    }
    if let Some((_, src)) = &req.end_user {
        e.warn(Kind::Ignored, src);
    }
    if let Some((false, src)) = &req.parallel {
        e.warn(Kind::Dropped, src);
    } else if let Some((true, src)) = &req.parallel {
        e.warn(Kind::Ignored, src);
    }
    Value::Object(out)
}

/// The tool specifications (with cache points) and the tool choice.
fn tool_config(e: &mut Enc<'_>) -> (Vec<Value>, Option<Value>) {
    let req = e.req;
    let mut tools = Vec::new();
    let mut allowed: Option<Vec<String>> = None;
    let choice = match &req.choice {
        Some((Choice::Auto, _)) => Some(json!({"auto": {}})),
        Some((Choice::Required, _)) => Some(json!({"any": {}})),
        Some((Choice::Named(n), _)) => Some(json!({"tool": {"name": e.names.up(n)}})),
        Some((Choice::None, src)) => {
            e.warn(Kind::Approximated, src);
            None
        }
        Some((Choice::Allowed { required, names }, src)) => {
            e.warn(Kind::Approximated, src);
            allowed = Some(names.clone());
            Some(if *required {
                json!({"any": {}})
            } else {
                json!({"auto": {}})
            })
        }
        None => None,
    };
    for t in &req.tools {
        if allowed.as_ref().is_some_and(|a| !a.contains(&t.name)) {
            continue;
        }
        let mut spec = Map::new();
        spec.insert("name".into(), e.names.up(&t.name).into());
        put(
            &mut spec,
            "description",
            t.description.clone().map_or(Value::Null, Value::from),
        );
        let schema = t
            .parameters
            .clone()
            .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
        spec.insert("inputSchema".into(), json!({"json": schema}));
        if let Some((_, src)) = &t.strict {
            e.warn(Kind::Ignored, src);
        }
        tools.push(json!({"toolSpec": spec}));
        if let Some(c) = &t.cache {
            tools.push(cache_point(c));
        }
    }
    if req.tools.is_empty() {
        let names = history_tool_names(req);
        if !names.is_empty() {
            let src = if req.tools_src.order().first() == Some(&u32::MAX) {
                Src::absent("tools")
            } else {
                req.tools_src.clone()
            };
            e.warn(Kind::Approximated, &src);
            for n in names {
                tools.push(json!({"toolSpec": {"name": e.names.up(&n), "inputSchema": {"json": {"type": "object"}}}}));
            }
        }
    }
    (tools, choice)
}

fn message(e: &mut Enc<'_>, t: &NTurn, docs: &mut usize) -> Value {
    let mut first = Vec::new();
    let mut content = Vec::new();
    for b in &t.blocks {
        let into_first = matches!(b, Block::Reasoning(_)) && t.role == Role::Assistant;
        let target = if into_first { &mut first } else { &mut content };
        block(e, b, docs, target);
    }
    first.extend(content);
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

fn push_cached(e: &Enc<'_>, out: &mut Vec<Value>, v: Value, cache: Option<&Cache>) {
    out.push(v);
    if let Some(c) = cache.filter(|c| e.cache_value(Some(c)).is_some()) {
        out.push(cache_point(c));
    }
}

fn block(e: &mut Enc<'_>, b: &Block, docs: &mut usize, out: &mut Vec<Value>) {
    match b {
        Block::Text { text, cache, .. } => push_cached(e, out, json!({"text": text}), cache.as_ref()),
        Block::Refusal { text } => out.push(json!({"text": text})),
        Block::Image {
            media,
            detail,
            cache,
            src,
            url_src,
        } => {
            if let Some((_, d)) = detail {
                e.warn(Kind::Ignored, d);
            }
            if let Some(Resolved::Bytes { mime, data }) = e.resolve(media, true, src, url_src) {
                push_cached(
                    e,
                    out,
                    json!({"image": {"format": subtype(&mime), "source": {"bytes": data}}}),
                    cache.as_ref(),
                );
            }
        }
        Block::Document {
            media,
            name,
            cache,
            src,
            url_src,
        } => {
            let index = *docs;
            *docs += 1;
            let (mime, data) = match e.resolve(media, false, src, url_src) {
                Some(Resolved::Bytes { mime, data }) => (mime, data),
                Some(Resolved::Text { mime, text }) => {
                    (mime, crate::xlate::util::b64_encode(text.as_bytes()))
                }
                _ => return,
            };
            let doc = json!({"document": {"format": converse_document_format(&mime),
                                          "name": converse_name(name.as_ref().map(|n| n.0.as_str()), index),
                                          "source": {"bytes": data}}});
            push_cached(e, out, doc, cache.as_ref());
        }
        Block::Audio { format, src, .. } => {
            e.audio_ok(format, src);
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
                json!({"toolUse": {"toolUseId": e.id(&c.id), "name": e.names.up(&c.name), "input": input}});
            push_cached(e, out, v, c.cache.as_ref());
        }
        Block::ToolResult(r) => {
            let mut content = Vec::new();
            for x in &r.content {
                match x {
                    Block::Text { text, .. } => content.push(json!({"text": text})),
                    other => block(e, other, docs, &mut content),
                }
            }
            let status = if r.is_error.as_ref().is_some_and(|(err, _)| *err) {
                "error"
            } else {
                "success"
            };
            let v = json!({"toolResult": {"toolUseId": e.id(&r.id), "content": content, "status": status}});
            push_cached(e, out, v, r.cache.as_ref());
        }
        Block::Reasoning(r) => {
            if let Some(data) = &r.redacted {
                out.push(json!({"reasoningContent": {"redactedContent": data}}));
            } else if let Some(sig) = &r.signature {
                out.push(json!({"reasoningContent": {"reasoningText": {"text": r.text, "signature": sig}}}));
            }
        }
    }
}
