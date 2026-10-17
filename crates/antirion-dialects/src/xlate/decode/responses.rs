//! OpenAI Responses requests.

use serde_json::Value;

use super::{Dec, Invalid, document_media, image_media, reasoning};
use crate::xlate::ir::{
    Block, Call, Choice, Family, Format, Hosted, Media, Mode, Role, SysBlock, Tool, ToolResult, Turn,
};
use crate::xlate::util::{Obj, parse_args, str_of};
use crate::xlate::warn::Src;

const TOP: &[&str] = &[
    "model",
    "input",
    "instructions",
    "max_output_tokens",
    "temperature",
    "top_p",
    "tools",
    "tool_choice",
    "parallel_tool_calls",
    "text",
    "reasoning",
    "store",
    "metadata",
    "include",
    "previous_response_id",
    "conversation",
    "prompt",
    "background",
    "truncation",
    "user",
    "safety_identifier",
    "service_tier",
    "prompt_cache_key",
    "stream",
    "stream_options",
    "top_logprobs",
    "max_tool_calls",
    "context_management",
    "top_k",
];

pub(super) fn decode(d: &mut Dec, obj: &Obj) -> Result<(), Invalid> {
    let root = Src::root();
    d.unknown_members(obj, &root, TOP, true);
    d.req.stream = obj.get("stream") == Some(&Value::Bool(true));
    if let Some(i) = obj.get("instructions").and_then(Value::as_str) {
        d.req.system.push(SysBlock {
            text: i.to_owned(),
            cache: None,
            developer: false,
            src: root.key(obj, "instructions"),
        });
    }
    let at = root.key(obj, "input");
    d.req.messages_src = at.clone();
    match obj.get("input") {
        Some(Value::String(s)) => d.req.turns.push(Turn {
            role: Role::User,
            blocks: vec![Block::Text {
                text: s.clone(),
                cache: None,
                src: at.clone(),
            }],
            cache: None,
            src: at.clone(),
            role_src: at.clone(),
        }),
        Some(Value::Array(items)) => items_of(d, items, &at)?,
        _ => {}
    }
    if let Some(Value::Array(tools)) = obj.get("tools") {
        let at = root.key(obj, "tools");
        d.req.tools_src = at.clone();
        for (i, t) in tools.iter().enumerate() {
            let src = at.idx(i);
            let Some(o) = t.as_object() else { continue };
            let kind = o.get("type").and_then(Value::as_str).unwrap_or("function");
            if kind.starts_with("web_search") {
                d.req.web_search = Some(super::web_search(t, src));
                continue;
            }
            if kind == "custom" {
                d.unknown_members(o, &src, &["type", "name", "description", "format"], false);
                let grammar = o
                    .get("format")
                    .filter(|f| str_of(f, "type").is_some_and(|t| t != "text"))
                    .map(|_| src.key(o, "format"));
                d.req.tools.push(Tool {
                    name: o
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    description: o.get("description").and_then(Value::as_str).map(str::to_owned),
                    parameters: Some(custom_schema()),
                    strict: None,
                    cache: None,
                    custom: Some(grammar),
                    params_src: src.clone(),
                    src,
                });
                continue;
            }
            if kind != "function" {
                d.req.hosted.push(Hosted {
                    kind: kind.to_owned(),
                    unsupported: false,
                    src,
                });
                continue;
            }
            d.unknown_members(
                o,
                &src,
                &["type", "name", "description", "parameters", "strict"],
                false,
            );
            d.req.tools.push(Tool {
                name: o
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                description: o.get("description").and_then(Value::as_str).map(str::to_owned),
                parameters: o.get("parameters").filter(|p| !p.is_null()).cloned(),
                strict: o
                    .get("strict")
                    .and_then(Value::as_bool)
                    .map(|s| (s, src.key(o, "strict"))),
                cache: None,
                custom: None,
                params_src: src.key(o, "parameters"),
                src,
            });
        }
    }
    if let Some(c) = obj.get("tool_choice") {
        let src = root.key(obj, "tool_choice");
        let ch = match c {
            Value::String(s) if s == "none" => Some(Choice::None),
            Value::String(s) if s == "required" => Some(Choice::Required),
            Value::String(_) => Some(Choice::Auto),
            Value::Object(o) => match o.get("type").and_then(Value::as_str) {
                Some("function") => o
                    .get("name")
                    .and_then(Value::as_str)
                    .map(|n| Choice::Named(n.to_owned())),
                Some("allowed_tools") => Some(Choice::Allowed {
                    required: o.get("mode").and_then(Value::as_str) == Some("required"),
                    names: o
                        .get("tools")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|t| str_of(t, "name"))
                        .map(str::to_owned)
                        .collect(),
                }),
                _ => Some(Choice::Auto),
            },
            _ => None,
        };
        d.req.choice = ch.map(|c| (c, src));
    }
    if let Some(Value::Bool(b)) = obj.get("parallel_tool_calls") {
        d.req.parallel = Some((*b, root.key(obj, "parallel_tool_calls")));
    }
    d.req.max_tokens = obj
        .get("max_output_tokens")
        .and_then(Value::as_u64)
        .map(|v| (v, root.key(obj, "max_output_tokens")));
    d.req.temperature = Dec::num(obj, &root, "temperature");
    d.req.top_p = Dec::num(obj, &root, "top_p");
    d.req.top_k = Dec::num(obj, &root, "top_k");
    d.req.top_logprobs = Dec::num(obj, &root, "top_logprobs");
    if let Some(Value::Object(t)) = obj.get("text") {
        let at = root.key(obj, "text");
        if let Some(f) = t.get("format") {
            let fsrc = at.key(t, "format");
            let format = match str_of(f, "type") {
                Some("json_object") => Some(Format::JsonObject),
                Some("json_schema") => Some(Format::JsonSchema {
                    name: str_of(f, "name").map(str::to_owned),
                    schema: f
                        .get("schema")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({"type": "object"})),
                    strict: f.get("strict").and_then(Value::as_bool),
                }),
                _ => None,
            };
            d.req.format = format.map(|f| (f, fsrc));
        }
        if let Some(v) = t.get("verbosity").filter(|v| !v.is_null()) {
            d.req.inert.push(crate::xlate::ir::Inert {
                name: "text.verbosity",
                value: v.clone(),
                src: at.key(t, "verbosity"),
            });
        }
    }
    if let Some(Value::Object(r)) = obj.get("reasoning") {
        let at = root.key(obj, "reasoning");
        if let Some(e) = r.get("effort").and_then(Value::as_str) {
            d.req.reason.src = at.key(r, "effort");
            super::chat::set_effort(&mut d.req.reason, e);
        }
        if r.get("summary")
            .or_else(|| r.get("generate_summary"))
            .and_then(Value::as_str)
            .is_some()
        {
            d.req.reason.include_thoughts = Some(true);
            d.req.reason.summaries = true;
            if d.req.reason.mode == Mode::Unspecified {
                d.req.reason.src = at.clone();
            }
        }
    }
    for name in ["safety_identifier", "user"] {
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
        "truncation",
        "include",
        "max_tool_calls",
    ] {
        d.inert(obj, &root, name);
    }
    d.req.store = obj.get("store").and_then(Value::as_bool);
    d.req.include_encrypted = obj.get("include").and_then(Value::as_array).is_some_and(|a| {
        a.iter()
            .any(|x| x.as_str() == Some("reasoning.encrypted_content"))
    });
    for name in ["conversation", "prompt", "background", "context_management"] {
        if obj
            .get(name)
            .is_some_and(|v| !v.is_null() && v != &Value::Bool(false))
        {
            d.unsupported(
                root.key(obj, name),
                "This Responses field is served only by a native Responses deployment.",
            );
        }
    }
    Ok(())
}

fn role_of(s: Option<&str>) -> Role {
    match s {
        Some("system") => Role::System,
        Some("developer") => Role::Developer,
        Some("assistant") => Role::Assistant,
        _ => Role::User,
    }
}

fn items_of(d: &mut Dec, items: &[Value], at: &Src) -> Result<(), Invalid> {
    let mut leading = true;
    let mut pending_reasoning: Vec<Block> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let src = at.idx(i);
        let Some(o) = item.as_object() else { continue };
        match o.get("type").and_then(Value::as_str).unwrap_or("message") {
            "message" => {
                d.unknown_members(o, &src, &["type", "role", "content", "id", "status"], false);
                let role = role_of(o.get("role").and_then(Value::as_str));
                let csrc = src.key(o, "content");
                let blocks = content(d, o.get("content"), &csrc)?;
                if matches!(role, Role::System | Role::Developer) && leading {
                    for b in blocks {
                        if let Block::Text { text, src, .. } = b {
                            d.req.system.push(SysBlock {
                                text,
                                cache: None,
                                developer: role == Role::Developer,
                                src,
                            });
                        }
                    }
                    continue;
                }
                leading = false;
                let mut all = std::mem::take(&mut pending_reasoning);
                if role != Role::Assistant {
                    flush_reasoning(d, &mut all, &src);
                }
                all.extend(blocks);
                push(d, role, all, &src, src.key(o, "role"));
            }
            "function_call" => {
                leading = false;
                d.unknown_members(
                    o,
                    &src,
                    &["type", "call_id", "name", "arguments", "id", "status"],
                    false,
                );
                let raw = o
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let mut blocks = std::mem::take(&mut pending_reasoning);
                blocks.push(Block::ToolCall(Call {
                    id: o
                        .get("call_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    name: o
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    args: parse_args(&raw),
                    raw: Some(raw),
                    signature: None,
                    cache: None,
                    args_src: src.key(o, "arguments"),
                    src: src.clone(),
                }));
                push(d, Role::Assistant, blocks, &src, src.clone());
            }
            "custom_tool_call" => {
                leading = false;
                d.unknown_members(
                    o,
                    &src,
                    &["type", "call_id", "name", "input", "id", "status"],
                    false,
                );
                let input = o.get("input").and_then(Value::as_str).unwrap_or_default();
                let mut blocks = std::mem::take(&mut pending_reasoning);
                blocks.push(Block::ToolCall(Call {
                    id: o
                        .get("call_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    name: o
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    args: Some(serde_json::json!({"input": input})),
                    raw: None,
                    signature: None,
                    cache: None,
                    args_src: src.key(o, "input"),
                    src: src.clone(),
                }));
                push(d, Role::Assistant, blocks, &src, src.clone());
            }
            "function_call_output" | "custom_tool_call_output" => {
                leading = false;
                d.unknown_members(o, &src, &["type", "call_id", "output", "id", "status"], false);
                let osrc = src.key(o, "output");
                let content = content(d, o.get("output"), &osrc)?;
                let mut pending = std::mem::take(&mut pending_reasoning);
                flush_reasoning(d, &mut pending, &src);
                d.req.turns.push(Turn {
                    role: Role::Tool,
                    blocks: vec![Block::ToolResult(ToolResult {
                        id: o
                            .get("call_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        content,
                        is_error: None,
                        cache: None,
                        id_src: src.key(o, "call_id"),
                        src: src.clone(),
                        content_src: osrc,
                    })],
                    cache: None,
                    src: src.clone(),
                    role_src: src,
                });
            }
            "reasoning" => {
                leading = false;
                d.unknown_members(
                    o,
                    &src,
                    &["type", "id", "summary", "encrypted_content", "content", "status"],
                    false,
                );
                let text = o
                    .get("summary")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|s| str_of(s, "text"))
                    .collect::<Vec<_>>()
                    .join("\n\n");
                pending_reasoning.push(reasoning(
                    text,
                    o.get("encrypted_content").and_then(Value::as_str),
                    None,
                    Family::OpenAi,
                    src,
                ));
            }
            _ => d.req.ignored.push(src),
        }
    }
    let mut rest = pending_reasoning;
    flush_reasoning(d, &mut rest, at);
    Ok(())
}

/// Reasoning items not followed by an assistant item: kept in an assistant turn of their own.
fn flush_reasoning(d: &mut Dec, pending: &mut Vec<Block>, src: &Src) {
    if !pending.is_empty() {
        push(d, Role::Assistant, std::mem::take(pending), src, src.clone());
    }
}

/// Appends blocks as a turn; consecutive assistant items (a message, function calls) form one turn.
fn push(d: &mut Dec, role: Role, blocks: Vec<Block>, src: &Src, role_src: Src) {
    if role == Role::Assistant
        && let Some(last) = d.req.turns.last_mut()
        && last.role == Role::Assistant
    {
        last.blocks.extend(blocks);
        return;
    }
    d.req.turns.push(Turn {
        role,
        blocks,
        cache: None,
        src: src.clone(),
        role_src,
    });
}

fn content(d: &mut Dec, v: Option<&Value>, at: &Src) -> Result<Vec<Block>, Invalid> {
    let parts = match v {
        Some(Value::String(s)) => {
            return Ok(vec![Block::Text {
                text: s.clone(),
                cache: None,
                src: at.clone(),
            }]);
        }
        Some(Value::Array(p)) => p,
        _ => return Ok(Vec::new()),
    };
    let mut out = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        let src = at.idx(i);
        let Some(o) = p.as_object() else { continue };
        match o.get("type").and_then(Value::as_str) {
            Some("input_text" | "output_text" | "text") => {
                d.unknown_members(o, &src, &["type", "text", "annotations", "logprobs"], false);
                out.push(Block::Text {
                    text: o
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    cache: None,
                    src,
                });
            }
            Some("refusal") => out.push(Block::Refusal {
                text: o
                    .get("refusal")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            }),
            Some("input_image") => {
                d.unknown_members(o, &src, &["type", "image_url", "detail", "file_id"], false);
                let url_src = src.key(o, "image_url");
                let media = match o.get("file_id").and_then(Value::as_str) {
                    Some(id) if o.get("image_url").is_none() => Media::FileId(id.to_owned()),
                    _ => image_media(o.get("image_url"), &url_src)?,
                };
                out.push(Block::Image {
                    media,
                    detail: o
                        .get("detail")
                        .and_then(Value::as_str)
                        .filter(|x| *x != "auto")
                        .map(|x| (x.to_owned(), src.key(o, "detail"))),
                    cache: None,
                    src,
                    url_src,
                });
            }
            Some("input_file") => {
                d.unknown_members(
                    o,
                    &src,
                    &["type", "file_data", "file_url", "file_id", "filename"],
                    false,
                );
                let (media, url_src) = if let Some(data) = o.get("file_data").and_then(Value::as_str) {
                    let s = src.key(o, "file_data");
                    (document_media(data, &s)?, s)
                } else if let Some(url) = o.get("file_url").and_then(Value::as_str) {
                    (Media::Url(url.to_owned()), src.key(o, "file_url"))
                } else if let Some(id) = o.get("file_id").and_then(Value::as_str) {
                    (Media::FileId(id.to_owned()), src.key(o, "file_id"))
                } else {
                    return Err(super::invalid(&src, "needs file_data, file_url or file_id."));
                };
                out.push(Block::Document {
                    media,
                    name: o
                        .get("filename")
                        .and_then(Value::as_str)
                        .map(|n| (n.to_owned(), src.key(o, "filename"))),
                    cache: None,
                    src,
                    url_src,
                });
            }
            _ => d.req.ignored.push(src),
        }
    }
    Ok(out)
}

/// The input schema of a custom tool's function form: one required string `input`.
pub(crate) fn custom_schema() -> Value {
    serde_json::json!({"type": "object", "properties": {"input": {"type": "string"}}, "required": ["input"]})
}
