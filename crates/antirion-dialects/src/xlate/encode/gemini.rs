//! Gemini `generateContent` bodies.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use super::Enc;
use super::media::Resolved;
use super::sampling::Structured;
use super::turns::NTurn;
use crate::xlate::ir::{Block, Choice, Family, Role};
use crate::xlate::schema::to_subset;
use crate::xlate::util::{parse_args, put};
use crate::xlate::warn::Kind;

/// The value Google documents for skipping thought-signature validation.
const PLACEHOLDER: &str = "skip_thought_signature_validator";

pub(super) fn encode(e: &mut Enc<'_>) -> Value {
    let req = e.req;
    let mut out = Map::new();
    let turns = e.block_turns();
    let call_names: HashMap<&str, &str> = req.calls().map(|c| (c.id.as_str(), c.name.as_str())).collect();
    let current = turns
        .iter()
        .rposition(|t| t.role == Role::User && t.blocks.iter().any(|b| !matches!(b, Block::ToolResult(_))))
        .map_or(0, |i| i + 1);
    let mut contents = Vec::new();
    for (i, t) in turns.iter().enumerate() {
        let parts = if t.role == Role::Assistant {
            model_parts(e, t, i >= current)
        } else {
            user_parts(e, t, &call_names)
        };
        contents
            .push(json!({"role": if t.role == Role::Assistant { "model" } else { "user" }, "parts": parts}));
    }
    out.insert("contents".into(), Value::Array(contents));
    if !req.system.is_empty() {
        let parts: Vec<Value> = req.system.iter().map(|s| json!({"text": s.text})).collect();
        for s in &req.system {
            e.drop_cache(s.cache.as_ref());
        }
        out.insert("systemInstruction".into(), json!({"parts": parts}));
    }
    let declared = declarations(e);
    let mut tools = Vec::new();
    if !declared.is_empty() {
        tools.push(json!({"functionDeclarations": declared}));
    }
    if e.web_search().is_some() {
        tools.push(super::hosted::gemini_tool());
    }
    if !tools.is_empty() {
        out.insert("tools".into(), Value::Array(tools));
    }
    if let Some(config) = tool_config(e) {
        out.insert("toolConfig".into(), json!({"functionCallingConfig": config}));
    }
    match &req.parallel {
        Some((false, src)) => e.warn(Kind::Dropped, src),
        Some((true, src)) => e.warn(Kind::Ignored, src),
        None => {}
    }
    let mut g = Map::new();
    put(&mut g, "temperature", e.temperature(false).unwrap_or(Value::Null));
    put(&mut g, "topP", e.top_p(false, false).unwrap_or(Value::Null));
    put(&mut g, "topK", e.top_k(false).unwrap_or(Value::Null));
    if let Some((v, _)) = &req.max_tokens {
        g.insert("maxOutputTokens".into(), (*v).into());
    }
    if let Some(stop) = e.stop() {
        g.insert("stopSequences".into(), stop.into());
    }
    if let Some((n, _)) = &req.n
        && *n > 1
    {
        g.insert("candidateCount".into(), (*n).into());
    }
    for (key, n) in [
        ("frequencyPenalty", &req.frequency_penalty),
        ("presencePenalty", &req.presence_penalty),
        ("seed", &req.seed),
    ] {
        if let Some(n) = n {
            g.insert(key.into(), n.value.clone());
        }
    }
    for n in [&req.logit_bias, &req.logprobs, &req.top_logprobs] {
        e.drop_field(n.as_ref());
    }
    match e.structured(true) {
        Structured::Native(schema) => {
            g.insert("responseMimeType".into(), "application/json".into());
            if let Some(s) = schema {
                if e.t.native_structured {
                    g.insert("responseJsonSchema".into(), s);
                } else if let Some((_, src)) = req.format.clone() {
                    match to_subset(&s) {
                        Ok((converted, changed)) => {
                            if changed {
                                e.warn(Kind::Approximated, &src);
                            }
                            g.insert("responseSchema".into(), converted);
                        }
                        Err(_) => e.refuse(
                            &src,
                            "The response schema cannot be expressed for this deployment.",
                        ),
                    }
                }
            }
        }
        Structured::Emulated(_) | Structured::None => {}
    }
    put(
        &mut g,
        "thinkingConfig",
        e.gemini_thinking().unwrap_or(Value::Null),
    );
    if !g.is_empty() {
        out.insert("generationConfig".into(), Value::Object(g));
    }
    if let Some((_, src)) = &req.end_user {
        e.warn(Kind::Ignored, src);
    }
    Value::Object(out)
}

fn declarations(e: &mut Enc<'_>) -> Vec<Value> {
    let req = e.req;
    let mut out = Vec::new();
    for t in &req.tools {
        let mut d = Map::new();
        d.insert("name".into(), e.names.up(&t.name).into());
        put(
            &mut d,
            "description",
            t.description.clone().map_or(Value::Null, Value::from),
        );
        let schema = t
            .parameters
            .clone()
            .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
        if e.t.native_structured {
            d.insert("parametersJsonSchema".into(), schema);
        } else {
            match to_subset(&schema) {
                Ok((converted, changed)) => {
                    if changed {
                        e.warn(Kind::Approximated, &t.params_src);
                    }
                    d.insert("parameters".into(), converted);
                }
                Err(_) => e.refuse(
                    &t.params_src,
                    "The tool schema cannot be expressed for this deployment.",
                ),
            }
        }
        if let Some((_, src)) = &t.strict {
            e.warn(Kind::Ignored, src);
        }
        e.drop_cache(t.cache.as_ref());
        out.push(Value::Object(d));
    }
    out
}

fn tool_config(e: &mut Enc<'_>) -> Option<Value> {
    let (choice, _) = e.req.choice.as_ref()?;
    Some(match choice {
        Choice::Auto => json!({"mode": "AUTO"}),
        Choice::None => json!({"mode": "NONE"}),
        Choice::Required => json!({"mode": "ANY"}),
        Choice::Named(n) => json!({"mode": "ANY", "allowedFunctionNames": [e.names.up(n)]}),
        Choice::Allowed { required, names } => {
            let names: Vec<String> = names.iter().map(|n| e.names.up(n)).collect();
            json!({"mode": if *required { "ANY" } else { "AUTO" }, "allowedFunctionNames": names})
        }
    })
}

fn model_parts(e: &mut Enc<'_>, t: &NTurn, current: bool) -> Vec<Value> {
    let mut parts = Vec::new();
    let mut pending: Option<String> = None;
    let mut first_call = true;
    for b in &t.blocks {
        match b {
            Block::Reasoning(r) => {
                if r.family == Family::Gemini {
                    pending = r.signature.clone();
                }
            }
            Block::ToolCall(c) => {
                let args = c.args.clone().unwrap_or_else(|| {
                    e.warn(Kind::Approximated, &c.args_src);
                    json!({})
                });
                let mut part = Map::new();
                part.insert(
                    "functionCall".into(),
                    json!({"id": c.id, "name": e.names.up(&c.name), "args": args}),
                );
                let sig = c.signature.clone().or_else(|| pending.take());
                match sig {
                    Some(s) => {
                        part.insert("thoughtSignature".into(), s.into());
                    }
                    None if current && first_call && e.t.thought_signatures => {
                        e.warn(Kind::Approximated, &c.src);
                        part.insert("thoughtSignature".into(), PLACEHOLDER.into());
                    }
                    None => {}
                }
                first_call = false;
                parts.push(Value::Object(part));
            }
            other => {
                if let Some(mut p) = part(e, other) {
                    if let (Some(s), Some(o)) = (pending.take(), p.as_object_mut()) {
                        o.insert("thoughtSignature".into(), s.into());
                    }
                    parts.push(p);
                }
            }
        }
    }
    parts
}

fn user_parts(e: &mut Enc<'_>, t: &NTurn, call_names: &HashMap<&str, &str>) -> Vec<Value> {
    let mut parts = Vec::new();
    let mut extra = Vec::new();
    for b in &t.blocks {
        match b {
            Block::ToolResult(r) => {
                let text: String = r
                    .content
                    .iter()
                    .filter_map(|x| match x {
                        Block::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .concat();
                let response = match parse_args(&text) {
                    Some(v) if !text.trim().is_empty() => v,
                    _ => json!({"content": text}),
                };
                let name = call_names
                    .get(r.id.as_str())
                    .map_or_else(String::new, |n| e.names.up(n));
                if let Some((_, src)) = &r.is_error {
                    e.warn(Kind::Ignored, src);
                }
                e.drop_cache(r.cache.as_ref());
                parts.push(json!({"functionResponse": {"id": r.id, "name": name, "response": response}}));
                for x in r.content.iter().filter(|x| !matches!(x, Block::Text { .. })) {
                    extra.extend(part(e, x));
                }
            }
            other => parts.extend(part(e, other)),
        }
    }
    parts.extend(extra);
    parts
}

fn part(e: &mut Enc<'_>, b: &Block) -> Option<Value> {
    match b {
        Block::Text { text, cache, .. } => {
            e.drop_cache(cache.as_ref());
            Some(json!({"text": text}))
        }
        Block::Refusal { text } => Some(json!({"text": text})),
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
            e.drop_cache(cache.as_ref());
            Some(match e.resolve(media, true, src, url_src)? {
                Resolved::Bytes { mime, data } => json!({"inlineData": {"mimeType": mime, "data": data}}),
                Resolved::Url { url, mime } => {
                    json!({"fileData": {"mimeType": mime.unwrap_or_else(|| "image/png".into()), "fileUri": url}})
                }
                Resolved::Text { text, .. } => json!({"text": text}),
            })
        }
        Block::Document {
            media,
            name,
            cache,
            src,
            url_src,
        } => {
            if let Some((_, n)) = name {
                e.warn(Kind::Ignored, n);
            }
            e.drop_cache(cache.as_ref());
            Some(match e.resolve(media, false, src, url_src)? {
                Resolved::Bytes { mime, data } => json!({"inlineData": {"mimeType": mime, "data": data}}),
                Resolved::Url { url, .. } => {
                    json!({"fileData": {"mimeType": "application/pdf", "fileUri": url}})
                }
                Resolved::Text { text, .. } => json!({"text": text}),
            })
        }
        Block::Audio { data, format, src } => e
            .audio_ok(format, src)
            .then(|| json!({"inlineData": {"mimeType": format!("audio/{format}"), "data": data}})),
        Block::ToolCall(_) | Block::ToolResult(_) | Block::Reasoning(_) => None,
    }
}
