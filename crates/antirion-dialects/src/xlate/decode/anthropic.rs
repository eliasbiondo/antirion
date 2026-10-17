//! Anthropic Messages requests.

use serde_json::Value;

use super::{Dec, Invalid, cache_of, check_base64, invalid, reasoning};
use crate::xlate::ir::{
    Block, Call, Choice, Family, Format, Hosted, Media, Mode, Reason, Role, SysBlock, Tool, ToolResult, Turn,
};
use crate::xlate::media::image_mime;
use crate::xlate::util::{Obj, str_of};
use crate::xlate::warn::Src;

const TOP: &[&str] = &[
    "model",
    "messages",
    "max_tokens",
    "system",
    "stop_sequences",
    "stream",
    "temperature",
    "top_p",
    "top_k",
    "tools",
    "tool_choice",
    "metadata",
    "thinking",
    "service_tier",
    "output_config",
    "cache_control",
    "container",
    "mcp_servers",
    "context_management",
];

/// Anthropic `thinking` (also the Chat extension of that shape).
pub(super) fn thinking(r: &mut Reason, t: &Obj) {
    match t.get("type").and_then(Value::as_str) {
        Some("enabled") => {
            r.mode = Mode::On;
            r.budget = t.get("budget_tokens").and_then(Value::as_u64);
            r.include_thoughts = Some(true);
        }
        Some("adaptive") => {
            r.mode = Mode::Adaptive;
            r.include_thoughts = Some(true);
        }
        Some("disabled") => r.mode = Mode::Off,
        _ => {}
    }
}

const DEFINED_CLIENT_TOOLS: [&str; 4] = ["bash_", "text_editor_", "computer_", "memory_"];

pub(super) fn decode(d: &mut Dec, obj: &Obj) -> Result<(), Invalid> {
    let root = Src::root();
    d.unknown_members(obj, &root, TOP, true);
    d.req.stream = obj.get("stream") == Some(&Value::Bool(true));
    match obj.get("system") {
        Some(Value::String(s)) => d.req.system.push(SysBlock {
            text: s.clone(),
            cache: None,
            developer: false,
            src: root.key(obj, "system"),
        }),
        Some(Value::Array(blocks)) => {
            let at = root.key(obj, "system");
            for (i, b) in blocks.iter().enumerate() {
                let src = at.idx(i);
                if let Some(o) = b.as_object() {
                    d.unknown_members(o, &src, &["type", "text", "cache_control", "citations"], false);
                    d.req.system.push(SysBlock {
                        text: o
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        cache: cache_of(o, &src),
                        developer: false,
                        src,
                    });
                }
            }
        }
        _ => {}
    }
    if let Some(Value::Array(msgs)) = obj.get("messages") {
        let at = root.key(obj, "messages");
        d.req.messages_src = at.clone();
        for (i, m) in msgs.iter().enumerate() {
            message(d, m, &at.idx(i))?;
        }
    }
    if let Some(Value::Array(tools)) = obj.get("tools") {
        let at = root.key(obj, "tools");
        d.req.tools_src = at.clone();
        for (i, t) in tools.iter().enumerate() {
            tool(d, t, &at.idx(i));
        }
    }
    if let Some(Value::Object(c)) = obj.get("tool_choice") {
        let at = root.key(obj, "tool_choice");
        let ch = match c.get("type").and_then(Value::as_str) {
            Some("any") => Choice::Required,
            Some("none") => Choice::None,
            Some("tool") => Choice::Named(
                c.get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            ),
            _ => Choice::Auto,
        };
        d.req.choice = Some((ch, at.clone()));
        if let Some(Value::Bool(true)) = c.get("disable_parallel_tool_use") {
            d.req.parallel = Some((false, at.key(c, "disable_parallel_tool_use")));
        }
    }
    d.req.max_tokens = obj
        .get("max_tokens")
        .and_then(Value::as_u64)
        .map(|v| (v, root.key(obj, "max_tokens")));
    d.req.temperature = Dec::num(obj, &root, "temperature");
    d.req.top_p = Dec::num(obj, &root, "top_p");
    d.req.top_k = Dec::num(obj, &root, "top_k");
    if let Some(Value::Array(a)) = obj.get("stop_sequences") {
        d.req.stop = Some((
            a.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
            root.key(obj, "stop_sequences"),
        ));
    }
    if let Some(Value::Object(m)) = obj.get("metadata") {
        let at = root.key(obj, "metadata");
        if let Some(u) = m.get("user_id").and_then(Value::as_str) {
            d.req.end_user = Some((u.to_owned(), at.key(m, "user_id")));
        }
        for (k, v) in m {
            if k != "user_id" {
                d.req.inert.push(crate::xlate::ir::Inert {
                    name: "metadata",
                    value: v.clone(),
                    src: at.key(m, k),
                });
            }
        }
    }
    if let Some(Value::Object(t)) = obj.get("thinking") {
        d.req.reason.src = root.key(obj, "thinking");
        thinking(&mut d.req.reason, t);
    }
    if let Some(Value::Object(oc)) = obj.get("output_config") {
        let at = root.key(obj, "output_config");
        if let Some(f) = oc
            .get("format")
            .filter(|f| str_of(f, "type") == Some("json_schema"))
        {
            d.req.format = Some((
                Format::JsonSchema {
                    name: None,
                    schema: f
                        .get("schema")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({"type": "object"})),
                    strict: None,
                },
                at.key(oc, "format"),
            ));
        }
        if let Some(e) = oc.get("effort").and_then(Value::as_str) {
            if d.req.reason.mode == Mode::Unspecified {
                d.req.reason.src = at.key(oc, "effort");
            }
            super::chat::set_effort(&mut d.req.reason, e);
        }
    }
    d.inert(obj, &root, "service_tier");
    d.req.auto_cache = cache_of(obj, &root);
    for name in ["container", "mcp_servers", "context_management"] {
        if obj.get(name).is_some_and(|v| !v.is_null()) {
            d.unsupported(
                root.key(obj, name),
                "This Anthropic feature is only available on Anthropic deployments.",
            );
        }
    }
    Ok(())
}

fn tool(d: &mut Dec, t: &Value, src: &Src) {
    let Some(o) = t.as_object() else { return };
    let kind = o.get("type").and_then(Value::as_str).unwrap_or("custom");
    if kind.starts_with("web_search_") {
        d.req.web_search = Some(super::web_search(t, src.clone()));
        return;
    }
    if let Some((default_name, description, schema)) = crate::xlate::defined::function_form(kind) {
        d.req.tools.push(Tool {
            name: o
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(default_name)
                .to_owned(),
            description: Some(description.to_owned()),
            parameters: Some(schema),
            strict: None,
            cache: cache_of(o, src),
            custom: None,
            params_src: src.clone(),
            src: src.clone(),
        });
        return;
    }
    if kind != "custom" {
        let unsupported = DEFINED_CLIENT_TOOLS.iter().any(|p| kind.starts_with(p));
        d.req.hosted.push(Hosted {
            kind: kind.to_owned(),
            unsupported,
            src: src.clone(),
        });
        return;
    }
    d.unknown_members(
        o,
        src,
        &[
            "type",
            "name",
            "description",
            "input_schema",
            "cache_control",
            "strict",
        ],
        false,
    );
    d.req.tools.push(Tool {
        name: o
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        description: o.get("description").and_then(Value::as_str).map(str::to_owned),
        parameters: o.get("input_schema").filter(|s| !s.is_null()).cloned(),
        strict: o
            .get("strict")
            .and_then(Value::as_bool)
            .map(|s| (s, src.key(o, "strict"))),
        cache: cache_of(o, src),
        custom: None,
        params_src: src.key(o, "input_schema"),
        src: src.clone(),
    });
}

fn message(d: &mut Dec, m: &Value, src: &Src) -> Result<(), Invalid> {
    let Some(o) = m.as_object() else { return Ok(()) };
    d.unknown_members(o, src, &["role", "content"], false);
    let role = if o.get("role").and_then(Value::as_str) == Some("assistant") {
        Role::Assistant
    } else {
        Role::User
    };
    let role_src = src.key(o, "role");
    let content_src = src.key(o, "content");
    let blocks: Vec<(Block, bool)> = match o.get("content") {
        Some(Value::String(s)) => vec![(
            Block::Text {
                text: s.clone(),
                cache: None,
                src: content_src,
            },
            false,
        )],
        Some(Value::Array(items)) => {
            let mut out = Vec::new();
            for (i, b) in items.iter().enumerate() {
                if let Some(x) = block(d, b, &content_src.idx(i))? {
                    let is_result = matches!(x, Block::ToolResult(_));
                    out.push((x, is_result));
                }
            }
            out
        }
        _ => Vec::new(),
    };
    let mut current: Vec<Block> = Vec::new();
    let flush = |d: &mut Dec, current: &mut Vec<Block>| {
        if !current.is_empty() {
            d.req.turns.push(Turn {
                role,
                blocks: std::mem::take(current),
                cache: None,
                src: src.clone(),
                role_src: role_src.clone(),
            });
        }
    };
    let empty = blocks.is_empty();
    for (b, is_result) in blocks {
        if is_result && role == Role::User {
            flush(d, &mut current);
            d.req.turns.push(Turn {
                role: Role::Tool,
                blocks: vec![b],
                cache: None,
                src: src.clone(),
                role_src: role_src.clone(),
            });
        } else {
            current.push(b);
        }
    }
    if empty {
        d.req.turns.push(Turn {
            role,
            blocks: Vec::new(),
            cache: None,
            src: src.clone(),
            role_src: role_src.clone(),
        });
    }
    flush(d, &mut current);
    Ok(())
}

/// An image or document `source`.
fn source(s: Option<&Value>, at: &Src, image: bool) -> Result<Media, Invalid> {
    let Some(so) = s.and_then(Value::as_object) else {
        return Err(invalid(at, "must be an object."));
    };
    match so.get("type").and_then(Value::as_str) {
        Some("base64") => {
            let data = so.get("data").and_then(Value::as_str).unwrap_or_default();
            let dsrc = at.key(so, "data");
            check_base64(data, &dsrc)?;
            let declared = so.get("media_type").and_then(Value::as_str);
            let mime = if image {
                image_mime(declared, data)
            } else {
                declared.unwrap_or("application/pdf").to_owned()
            };
            Ok(Media::Base64 {
                mime,
                data: data.to_owned(),
            })
        }
        Some("url") => {
            let url = so.get("url").and_then(Value::as_str).filter(|u| !u.is_empty());
            url.map(|u| Media::Url(u.to_owned()))
                .ok_or_else(|| invalid(&at.key(so, "url"), "must be a non-empty URL."))
        }
        Some("text") => Ok(Media::Text {
            mime: so
                .get("media_type")
                .and_then(Value::as_str)
                .unwrap_or("text/plain")
                .to_owned(),
            text: so
                .get("data")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        }),
        Some("file") => Ok(Media::FileId(
            so.get("file_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        )),
        _ => Err(invalid(at, "is not a supported source.")),
    }
}

fn block(d: &mut Dec, b: &Value, src: &Src) -> Result<Option<Block>, Invalid> {
    let Some(o) = b.as_object() else { return Ok(None) };
    let cache = cache_of(o, src);
    let out = match o.get("type").and_then(Value::as_str) {
        Some("text") => {
            d.unknown_members(o, src, &["type", "text", "cache_control", "citations"], false);
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
        Some("image") => {
            d.unknown_members(o, src, &["type", "source", "cache_control"], false);
            let ssrc = src.key(o, "source");
            Block::Image {
                media: source(o.get("source"), &ssrc, true)?,
                detail: None,
                cache,
                src: src.clone(),
                url_src: ssrc,
            }
        }
        Some("document") => {
            d.unknown_members(
                o,
                src,
                &["type", "source", "title", "context", "citations", "cache_control"],
                false,
            );
            let ssrc = src.key(o, "source");
            Block::Document {
                media: source(o.get("source"), &ssrc, false)?,
                name: o
                    .get("title")
                    .and_then(Value::as_str)
                    .map(|t| (t.to_owned(), src.key(o, "title"))),
                cache,
                src: src.clone(),
                url_src: ssrc,
            }
        }
        Some("tool_use") => {
            d.unknown_members(o, src, &["type", "id", "name", "input", "cache_control"], false);
            let input = o
                .get("input")
                .cloned()
                .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
            Block::ToolCall(Call {
                id: o.get("id").and_then(Value::as_str).unwrap_or_default().to_owned(),
                name: o
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                args: input.is_object().then_some(input),
                raw: None,
                signature: None,
                cache,
                args_src: src.key(o, "input"),
                src: src.clone(),
            })
        }
        Some("tool_result") => {
            d.unknown_members(
                o,
                src,
                &["type", "tool_use_id", "content", "is_error", "cache_control"],
                false,
            );
            let csrc = src.key(o, "content");
            let content = match o.get("content") {
                Some(Value::String(s)) => vec![Block::Text {
                    text: s.clone(),
                    cache: None,
                    src: csrc.clone(),
                }],
                Some(Value::Array(items)) => {
                    let mut out = Vec::new();
                    for (i, x) in items.iter().enumerate() {
                        if let Some(b) = block(d, x, &csrc.idx(i))? {
                            out.push(b);
                        }
                    }
                    out
                }
                _ => Vec::new(),
            };
            Block::ToolResult(ToolResult {
                id: o
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                content,
                is_error: o
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .map(|e| (e, src.key(o, "is_error"))),
                cache,
                id_src: src.key(o, "tool_use_id"),
                src: src.clone(),
                content_src: csrc,
            })
        }
        Some("thinking") => reasoning(
            o.get("thinking")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            o.get("signature").and_then(Value::as_str),
            None,
            Family::Anthropic,
            src.clone(),
        ),
        Some("redacted_thinking") => reasoning(
            String::new(),
            None,
            o.get("data").and_then(Value::as_str),
            Family::Anthropic,
            src.clone(),
        ),
        _ => {
            d.req.ignored.push(src.clone());
            return Ok(None);
        }
    };
    Ok(Some(out))
}
