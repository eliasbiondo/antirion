//! Chat Completions requests, with the extensions OpenAI-SDK clients use to reach other providers.

use serde_json::Value;

use super::{Dec, Invalid, cache_of, document_media, image_media, reasoning, reasoning_details};
use crate::xlate::ir::{
    Block, Call, Choice, Effort, Family, Format, Hosted, Media, Mode, Role, SysBlock, Tool, ToolResult, Turn,
};
use crate::xlate::util::{Obj, parse_args, str_of};
use crate::xlate::warn::Src;

const TOP: &[&str] = &[
    "model",
    "messages",
    "frequency_penalty",
    "logit_bias",
    "logprobs",
    "top_logprobs",
    "max_tokens",
    "max_completion_tokens",
    "n",
    "presence_penalty",
    "response_format",
    "seed",
    "service_tier",
    "stop",
    "stream",
    "stream_options",
    "temperature",
    "top_p",
    "tools",
    "tool_choice",
    "parallel_tool_calls",
    "user",
    "safety_identifier",
    "modalities",
    "audio",
    "prediction",
    "store",
    "metadata",
    "reasoning_effort",
    "prompt_cache_key",
    "verbosity",
    "web_search_options",
    "top_k",
    "thinking",
    "reasoning",
];

const MESSAGE: &[&str] = &[
    "role",
    "content",
    "name",
    "tool_calls",
    "tool_call_id",
    "refusal",
    "audio",
    "function_call",
    "reasoning_content",
    "reasoning_details",
    "cache_control",
];

pub(super) fn decode(d: &mut Dec, obj: &Obj) -> Result<(), Invalid> {
    let root = Src::root();
    d.unknown_members(obj, &root, TOP, true);
    d.req.stream = obj.get("stream") == Some(&Value::Bool(true));
    if let Some(Value::Array(msgs)) = obj.get("messages") {
        let at = root.key(obj, "messages");
        d.req.messages_src = at.clone();
        messages(d, msgs, &at)?;
    }
    if let Some(Value::Array(tools)) = obj.get("tools") {
        let at = root.key(obj, "tools");
        d.req.tools_src = at.clone();
        decode_tools(d, tools, &at);
    }
    if let Some(c) = obj.get("tool_choice") {
        let src = root.key(obj, "tool_choice");
        d.req.choice = choice(c).map(|c| (c, src));
    }
    if let Some(Value::Bool(b)) = obj.get("parallel_tool_calls") {
        d.req.parallel = Some((*b, root.key(obj, "parallel_tool_calls")));
    }
    let max = ["max_tokens", "max_completion_tokens"]
        .into_iter()
        .filter_map(|n| obj.get(n).and_then(Value::as_u64).map(|v| (v, root.key(obj, n))))
        .min_by_key(|(v, _)| *v);
    d.req.max_tokens = max;
    d.req.temperature = Dec::num(obj, &root, "temperature");
    d.req.top_p = Dec::num(obj, &root, "top_p");
    d.req.top_k = Dec::num(obj, &root, "top_k");
    d.req.frequency_penalty = Dec::num(obj, &root, "frequency_penalty");
    d.req.presence_penalty = Dec::num(obj, &root, "presence_penalty");
    d.req.seed = Dec::num(obj, &root, "seed");
    d.req.logit_bias = Dec::num(obj, &root, "logit_bias");
    d.req.logprobs = Dec::num(obj, &root, "logprobs").filter(|n| n.value == Value::Bool(true));
    d.req.top_logprobs = Dec::num(obj, &root, "top_logprobs");
    d.req.n = obj
        .get("n")
        .and_then(Value::as_u64)
        .map(|n| (n, root.key(obj, "n")));
    d.req.stop = match obj.get("stop") {
        Some(Value::String(s)) => Some((vec![s.clone()], root.key(obj, "stop"))),
        Some(Value::Array(a)) => Some((
            a.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
            root.key(obj, "stop"),
        )),
        _ => None,
    };
    if let Some(f) = obj.get("response_format") {
        d.req.format = response_format(f).map(|f| (f, root.key(obj, "response_format")));
    }
    reasoning_controls(d, obj, &root);
    for name in ["user", "safety_identifier"] {
        if d.req.end_user.is_none()
            && let Some(u) = obj.get(name).and_then(Value::as_str)
        {
            d.req.end_user = Some((u.to_owned(), root.key(obj, name)));
        }
    }
    for name in [
        "metadata",
        "store",
        "service_tier",
        "prompt_cache_key",
        "prediction",
        "verbosity",
    ] {
        d.inert(obj, &root, name);
    }
    d.req.store = obj.get("store").and_then(Value::as_bool);
    let audio_out = obj
        .get("modalities")
        .and_then(Value::as_array)
        .is_some_and(|m| m.iter().any(|x| x.as_str() == Some("audio")));
    if audio_out {
        d.unsupported(
            root.key(obj, "modalities"),
            "Audio output can only be produced by a Chat Completions deployment.",
        );
    }
    if obj.get("audio").is_some_and(|a| !a.is_null()) {
        d.unsupported(
            root.key(obj, "audio"),
            "Audio output can only be produced by a Chat Completions deployment.",
        );
    }
    if let Some(w) = obj.get("web_search_options").filter(|w| !w.is_null()) {
        d.req.web_search = Some(super::web_search(w, root.key(obj, "web_search_options")));
    }
    Ok(())
}

fn reasoning_controls(d: &mut Dec, obj: &Obj, root: &Src) {
    let r = &mut d.req.reason;
    if let Some(e) = obj.get("reasoning_effort").and_then(Value::as_str) {
        r.src = root.key(obj, "reasoning_effort");
        set_effort(r, e);
    }
    if let Some(Value::Object(ext)) = obj.get("reasoning") {
        let at = root.key(obj, "reasoning");
        r.src = at.clone();
        if let Some(e) = ext.get("effort").and_then(Value::as_str) {
            set_effort(r, e);
        }
        if let Some(b) = ext.get("max_tokens").and_then(Value::as_u64) {
            r.mode = Mode::On;
            r.budget = Some(b);
        }
        if let Some(Value::Bool(x)) = ext.get("exclude") {
            r.include_thoughts = Some(!x);
            r.summaries = !x;
        }
    }
    if let Some(Value::Object(t)) = obj.get("thinking") {
        r.src = root.key(obj, "thinking");
        super::anthropic::thinking(r, t);
    }
}

pub(super) fn set_effort(r: &mut crate::xlate::ir::Reason, e: &str) {
    match Effort::parse(e) {
        Some(Effort::None) => r.mode = Mode::Off,
        Some(effort) => {
            if r.mode != Mode::Adaptive {
                r.mode = Mode::On;
            }
            r.effort = Some(effort);
        }
        None => {}
    }
}

fn response_format(f: &Value) -> Option<Format> {
    match str_of(f, "type") {
        Some("json_object") => Some(Format::JsonObject),
        Some("json_schema") => {
            let js = f.get("json_schema")?;
            Some(Format::JsonSchema {
                name: str_of(js, "name").map(str::to_owned),
                schema: js
                    .get("schema")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({"type": "object"})),
                strict: js.get("strict").and_then(Value::as_bool),
            })
        }
        _ => None,
    }
}

fn choice(c: &Value) -> Option<Choice> {
    match c {
        Value::String(s) => match s.as_str() {
            "auto" => Some(Choice::Auto),
            "none" => Some(Choice::None),
            "required" => Some(Choice::Required),
            _ => None,
        },
        Value::Object(o) => match o.get("type").and_then(Value::as_str) {
            Some("function") => o
                .get("function")
                .and_then(|f| str_of(f, "name"))
                .map(|n| Choice::Named(n.to_owned())),
            Some("allowed_tools") => {
                let a = o.get("allowed_tools")?;
                let names = a
                    .get("tools")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|t| {
                        t.get("function")
                            .and_then(|f| str_of(f, "name"))
                            .or_else(|| str_of(t, "name"))
                    })
                    .map(str::to_owned)
                    .collect();
                Some(Choice::Allowed {
                    required: str_of(a, "mode") == Some("required"),
                    names,
                })
            }
            _ => Some(Choice::Auto),
        },
        _ => None,
    }
}

fn decode_tools(d: &mut Dec, tools: &[Value], at: &Src) {
    for (i, t) in tools.iter().enumerate() {
        let src = at.idx(i);
        let Some(o) = t.as_object() else { continue };
        let kind = o.get("type").and_then(Value::as_str).unwrap_or("function");
        if kind != "function" {
            d.req.hosted.push(Hosted {
                kind: kind.to_owned(),
                unsupported: false,
                src,
            });
            continue;
        }
        d.unknown_members(o, &src, &["type", "function"], false);
        let Some(f) = o.get("function").and_then(Value::as_object) else {
            continue;
        };
        let fsrc = src.key(o, "function");
        d.unknown_members(
            f,
            &fsrc,
            &["name", "description", "parameters", "strict", "cache_control"],
            false,
        );
        d.req.tools.push(Tool {
            name: f
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            description: f.get("description").and_then(Value::as_str).map(str::to_owned),
            parameters: f.get("parameters").filter(|p| !p.is_null()).cloned(),
            strict: f
                .get("strict")
                .and_then(Value::as_bool)
                .map(|s| (s, fsrc.key(f, "strict"))),
            cache: cache_of(f, &fsrc),
            custom: None,
            params_src: fsrc.key(f, "parameters"),
            src,
        });
    }
}

fn role_of(s: Option<&str>) -> Role {
    match s {
        Some("system") => Role::System,
        Some("developer") => Role::Developer,
        Some("assistant") => Role::Assistant,
        Some("tool" | "function") => Role::Tool,
        _ => Role::User,
    }
}

fn messages(d: &mut Dec, msgs: &[Value], at: &Src) -> Result<(), Invalid> {
    let mut leading = true;
    for (i, m) in msgs.iter().enumerate() {
        let Some(o) = m.as_object() else { continue };
        let src = at.idx(i);
        d.unknown_members(o, &src, MESSAGE, false);
        if o.contains_key("name") {
            d.req.ignored.push(src.key(o, "name"));
        }
        let role = role_of(o.get("role").and_then(Value::as_str));
        let role_src = src.key(o, "role");
        let cache = cache_of(o, &src);
        let content_src = src.key(o, "content");
        if matches!(role, Role::System | Role::Developer) && leading {
            let blocks = content(d, o.get("content"), &content_src)?;
            let n = d.req.system.len();
            for b in blocks {
                if let Block::Text { text, cache, src } = b {
                    d.req.system.push(SysBlock {
                        text,
                        cache,
                        developer: role == Role::Developer,
                        src,
                    });
                }
            }
            if let (Some(c), Some(last)) = (cache, d.req.system.get_mut(n..).and_then(|s| s.last_mut())) {
                last.cache.get_or_insert(c);
            }
            continue;
        }
        leading = false;
        let blocks = match role {
            Role::Tool => {
                let content = content(d, o.get("content"), &content_src)?;
                vec![Block::ToolResult(ToolResult {
                    id: o
                        .get("tool_call_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    content,
                    is_error: None,
                    cache: cache.clone(),
                    id_src: src.key(o, "tool_call_id"),
                    src: src.clone(),
                    content_src,
                })]
            }
            Role::Assistant => assistant(d, o, &src)?,
            _ => content(d, o.get("content"), &content_src)?,
        };
        d.req.turns.push(Turn {
            role,
            blocks,
            cache: if role == Role::Tool { None } else { cache },
            src,
            role_src,
        });
    }
    Ok(())
}

fn assistant(d: &mut Dec, o: &Obj, src: &Src) -> Result<Vec<Block>, Invalid> {
    let mut blocks = Vec::new();
    if let Some(Value::Array(details)) = o.get("reasoning_details") {
        blocks.extend(reasoning_details(details, &src.key(o, "reasoning_details")));
    } else if let Some(text) = o.get("reasoning_content").and_then(Value::as_str) {
        blocks.push(reasoning(
            text.to_owned(),
            None,
            None,
            Family::None,
            src.key(o, "reasoning_content"),
        ));
    }
    blocks.extend(content(d, o.get("content"), &src.key(o, "content"))?);
    if let Some(r) = o.get("refusal").and_then(Value::as_str) {
        blocks.push(Block::Refusal { text: r.to_owned() });
    }
    if let Some(Value::Array(calls)) = o.get("tool_calls") {
        let at = src.key(o, "tool_calls");
        for (i, c) in calls.iter().enumerate() {
            let Some(co) = c.as_object() else { continue };
            let csrc = at.idx(i);
            d.unknown_members(
                co,
                &csrc,
                &["id", "type", "function", "cache_control", "extra_content"],
                false,
            );
            let f = co.get("function");
            let raw = f
                .and_then(|f| str_of(f, "arguments"))
                .unwrap_or_default()
                .to_owned();
            let fsrc = csrc.key(co, "function");
            blocks.push(Block::ToolCall(Call {
                id: co
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                name: f.and_then(|f| str_of(f, "name")).unwrap_or_default().to_owned(),
                args: parse_args(&raw),
                raw: Some(raw),
                signature: co
                    .get("extra_content")
                    .and_then(|e| e.get("google"))
                    .and_then(|g| str_of(g, "thought_signature"))
                    .map(str::to_owned),
                cache: cache_of(co, &csrc),
                args_src: match f.and_then(Value::as_object) {
                    Some(fo) => fsrc.key(fo, "arguments"),
                    None => fsrc.clone(),
                },
                src: csrc,
            }));
        }
    }
    Ok(blocks)
}

/// A message's content: a string, `null`, or an array of parts.
fn content(d: &mut Dec, v: Option<&Value>, at: &Src) -> Result<Vec<Block>, Invalid> {
    match v {
        Some(Value::String(s)) => Ok(vec![Block::Text {
            text: s.clone(),
            cache: None,
            src: at.clone(),
        }]),
        Some(Value::Array(parts)) => {
            let mut out = Vec::new();
            for (i, p) in parts.iter().enumerate() {
                if let Some(b) = part(d, p, &at.idx(i))? {
                    out.push(b);
                }
            }
            Ok(out)
        }
        _ => Ok(Vec::new()),
    }
}

fn part(d: &mut Dec, p: &Value, src: &Src) -> Result<Option<Block>, Invalid> {
    let Some(o) = p.as_object() else {
        return Ok(None);
    };
    let cache = cache_of(o, src);
    let block = match o.get("type").and_then(Value::as_str) {
        Some("text") => {
            d.unknown_members(o, src, &["type", "text", "cache_control"], false);
            Block::Text {
                text: o
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                cache,
                src: src.clone(),
            }
        }
        Some("refusal") => Block::Refusal {
            text: o
                .get("refusal")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        },
        Some("image_url") => {
            d.unknown_members(o, src, &["type", "image_url", "cache_control"], false);
            let img = o.get("image_url");
            let isrc = src.key(o, "image_url");
            let io = img.and_then(Value::as_object);
            let url_src = io.map_or_else(|| isrc.key_at(0, "url"), |io| isrc.key(io, "url"));
            let url = match img {
                Some(Value::String(_)) => img,
                _ => img.and_then(|i| i.get("url")),
            };
            Block::Image {
                media: image_media(url, &url_src)?,
                detail: io.and_then(|io| {
                    io.get("detail")
                        .and_then(Value::as_str)
                        .map(|x| (x.to_owned(), isrc.key(io, "detail")))
                }),
                cache,
                src: src.clone(),
                url_src,
            }
        }
        Some("file") => {
            d.unknown_members(o, src, &["type", "file", "cache_control"], false);
            let fsrc = src.key(o, "file");
            let Some(f) = o.get("file").and_then(Value::as_object) else {
                return Ok(None);
            };
            let (media, url_src) = if let Some(data) = f.get("file_data").and_then(Value::as_str) {
                let s = fsrc.key(f, "file_data");
                (document_media(data, &s)?, s)
            } else if let Some(id) = f.get("file_id").and_then(Value::as_str) {
                (Media::FileId(id.to_owned()), fsrc.key(f, "file_id"))
            } else {
                return Err(super::invalid(&fsrc, "needs file_data or file_id."));
            };
            Block::Document {
                media,
                name: f
                    .get("filename")
                    .and_then(Value::as_str)
                    .map(|n| (n.to_owned(), fsrc.key(f, "filename"))),
                cache,
                src: src.clone(),
                url_src,
            }
        }
        Some("input_audio") => {
            let a = o.get("input_audio");
            Block::Audio {
                data: a.and_then(|a| str_of(a, "data")).unwrap_or_default().to_owned(),
                format: a.and_then(|a| str_of(a, "format")).unwrap_or_default().to_owned(),
                src: src.clone(),
            }
        }
        _ => {
            d.req.unknown.push(crate::xlate::ir::Unknown {
                name: "type".into(),
                value: p.clone(),
                top_level: false,
                src: src.clone(),
            });
            return Ok(None);
        }
    };
    Ok(Some(block))
}
