//! Bedrock Converse requests. The body has no model and no stream flag: both come from the path.
//! Bytes are base64 strings, as in the JSON form of the API; a `cachePoint` block marks the block
//! before it.

use serde_json::Value;

use super::{Dec, Invalid, check_base64, reasoning};
use crate::xlate::ir::{
    Block, Cache, Call, Choice, Family, Hosted, Media, Role, SysBlock, Tool, ToolResult, Turn,
};
use crate::xlate::media::image_mime;
use crate::xlate::util::{Obj, compact, str_of};
use crate::xlate::warn::Src;

const TOP: &[&str] = &[
    "messages",
    "system",
    "inferenceConfig",
    "toolConfig",
    "guardrailConfig",
    "additionalModelRequestFields",
    "promptVariables",
    "additionalModelResponseFieldPaths",
    "requestMetadata",
    "performanceConfig",
    "modelId",
];

pub(super) fn decode(d: &mut Dec, obj: &Obj) -> Result<(), Invalid> {
    let root = Src::root();
    d.unknown_members(obj, &root, TOP, true);
    if let Some(Value::Array(system)) = obj.get("system") {
        let at = root.key(obj, "system");
        for (i, b) in system.iter().enumerate() {
            let src = at.idx(i);
            let Some(o) = b.as_object() else { continue };
            if let Some(t) = o.get("text").and_then(Value::as_str) {
                d.req.system.push(SysBlock {
                    text: t.to_owned(),
                    cache: None,
                    developer: false,
                    src,
                });
            } else if o.contains_key("cachePoint") {
                let cache = cache_point(o, &src);
                if let Some(last) = d.req.system.last_mut() {
                    last.cache = Some(cache);
                }
            } else {
                d.req.ignored.push(src);
            }
        }
    }
    if let Some(Value::Array(msgs)) = obj.get("messages") {
        let at = root.key(obj, "messages");
        d.req.messages_src = at.clone();
        for (i, m) in msgs.iter().enumerate() {
            message(d, m, &at.idx(i))?;
        }
    }
    if let Some(Value::Object(tc)) = obj.get("toolConfig") {
        tool_config(d, tc, &root.key(obj, "toolConfig"));
    }
    if let Some(Value::Object(ic)) = obj.get("inferenceConfig") {
        let at = root.key(obj, "inferenceConfig");
        d.unknown_members(
            ic,
            &at,
            &["maxTokens", "temperature", "topP", "stopSequences"],
            false,
        );
        d.req.max_tokens = ic
            .get("maxTokens")
            .and_then(Value::as_u64)
            .map(|v| (v, at.key(ic, "maxTokens")));
        d.req.temperature = Dec::num(ic, &at, "temperature");
        d.req.top_p = Dec::num(ic, &at, "topP");
        if let Some(Value::Array(a)) = ic.get("stopSequences") {
            d.req.stop = Some((
                a.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
                at.key(ic, "stopSequences"),
            ));
        }
    }
    if let Some(Value::Object(extra)) = obj.get("additionalModelRequestFields") {
        let at = root.key(obj, "additionalModelRequestFields");
        for (k, v) in extra {
            match k.as_str() {
                "top_k" => d.req.top_k = Dec::num(extra, &at, "top_k"),
                "thinking" | "reasoning_config" => {
                    if let Value::Object(t) = v {
                        d.req.reason.src = at.key(extra, k);
                        super::anthropic::thinking(&mut d.req.reason, t);
                    }
                }
                _ => d.req.unknown.push(crate::xlate::ir::Unknown {
                    name: k.clone(),
                    value: v.clone(),
                    top_level: true,
                    src: at.key(extra, k),
                }),
            }
        }
    }
    if obj.get("guardrailConfig").is_some_and(|v| !v.is_null()) {
        d.req.hosted.push(Hosted {
            kind: "guardrailConfig".into(),
            unsupported: false,
            src: root.key(obj, "guardrailConfig"),
        });
    }
    for name in [
        "promptVariables",
        "additionalModelResponseFieldPaths",
        "requestMetadata",
        "performanceConfig",
        "modelId",
    ] {
        if obj.get(name).is_some_and(|v| !v.is_null()) {
            d.req.ignored.push(root.key(obj, name));
        }
    }
    Ok(())
}

fn cache_point(o: &Obj, src: &Src) -> Cache {
    let ttl = o
        .get("cachePoint")
        .and_then(|c| c.get("ttl"))
        .and_then(Value::as_str);
    let value = match ttl {
        Some(t) => serde_json::json!({"type": "ephemeral", "ttl": t}),
        None => serde_json::json!({"type": "ephemeral"}),
    };
    Cache {
        value,
        src: src.key(o, "cachePoint"),
    }
}

fn tool_config(d: &mut Dec, tc: &Obj, at: &Src) {
    d.unknown_members(tc, at, &["tools", "toolChoice"], false);
    if let Some(Value::Array(tools)) = tc.get("tools") {
        let tat = at.key(tc, "tools");
        d.req.tools_src = tat.clone();
        for (i, t) in tools.iter().enumerate() {
            let src = tat.idx(i);
            let Some(o) = t.as_object() else { continue };
            if o.contains_key("cachePoint") {
                let cache = cache_point(o, &src);
                if let Some(last) = d.req.tools.last_mut() {
                    last.cache = Some(cache);
                }
                continue;
            }
            let Some(Value::Object(spec)) = o.get("toolSpec") else {
                d.req.ignored.push(src);
                continue;
            };
            let ssrc = src.key(o, "toolSpec");
            d.unknown_members(
                spec,
                &ssrc,
                &["name", "description", "inputSchema", "strict"],
                false,
            );
            let schema_src = ssrc.key(spec, "inputSchema");
            d.req.tools.push(Tool {
                name: spec
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                description: spec.get("description").and_then(Value::as_str).map(str::to_owned),
                parameters: spec
                    .get("inputSchema")
                    .and_then(|s| s.get("json"))
                    .filter(|s| !s.is_null())
                    .cloned(),
                strict: None,
                cache: None,
                custom: None,
                params_src: schema_src,
                src: ssrc,
            });
        }
    }
    if let Some(Value::Object(c)) = tc.get("toolChoice") {
        let src = at.key(tc, "toolChoice");
        let choice = if c.contains_key("any") {
            Choice::Required
        } else if let Some(t) = c.get("tool") {
            Choice::Named(str_of(t, "name").unwrap_or_default().to_owned())
        } else {
            Choice::Auto
        };
        d.req.choice = Some((choice, src));
    }
}

fn message(d: &mut Dec, m: &Value, src: &Src) -> Result<(), Invalid> {
    let Some(o) = m.as_object() else { return Ok(()) };
    d.unknown_members(o, src, &["role", "content"], false);
    let role = if str_of(m, "role") == Some("assistant") {
        Role::Assistant
    } else {
        Role::User
    };
    let role_src = src.key(o, "role");
    let content_src = src.key(o, "content");
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
    let items = o.get("content").and_then(Value::as_array);
    for (i, b) in items.into_iter().flatten().enumerate() {
        let bsrc = content_src.idx(i);
        let Some(bo) = b.as_object() else { continue };
        if bo.contains_key("cachePoint") {
            let cache = cache_point(bo, &bsrc);
            let last = current
                .last_mut()
                .or_else(|| d.req.turns.last_mut().and_then(|t| t.blocks.last_mut()));
            if let Some(b) = last {
                set_cache(b, cache);
            }
            continue;
        }
        let Some(block) = block(d, bo, &bsrc)? else {
            continue;
        };
        if matches!(block, Block::ToolResult(_)) && role == Role::User {
            flush(d, &mut current);
            d.req.turns.push(Turn {
                role: Role::Tool,
                blocks: vec![block],
                cache: None,
                src: src.clone(),
                role_src: role_src.clone(),
            });
        } else {
            current.push(block);
        }
    }
    if items.is_none_or(Vec::is_empty) {
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

fn set_cache(b: &mut Block, cache: Cache) {
    match b {
        Block::Text { cache: c, .. } | Block::Image { cache: c, .. } | Block::Document { cache: c, .. } => {
            *c = Some(cache);
        }
        Block::ToolCall(call) => call.cache = Some(cache),
        Block::ToolResult(r) => r.cache = Some(cache),
        _ => {}
    }
}

/// A Converse image or document `source`: base64 bytes, or an S3 location no other dialect can read.
fn source(d: &mut Dec, o: &Obj, at: &Src, mime: String) -> Result<Option<Media>, Invalid> {
    let ssrc = at.key(o, "source");
    let Some(Value::Object(so)) = o.get("source") else {
        return Ok(None);
    };
    if let Some(bytes) = so.get("bytes").and_then(Value::as_str) {
        check_base64(bytes, &ssrc.key(so, "bytes"))?;
        return Ok(Some(Media::Base64 {
            mime,
            data: bytes.to_owned(),
        }));
    }
    if let Some(text) = so.get("text").and_then(Value::as_str) {
        return Ok(Some(Media::Text {
            mime: "text/plain".into(),
            text: text.to_owned(),
        }));
    }
    d.unsupported(
        ssrc,
        "An S3 location can only be read by a Bedrock Converse deployment.",
    );
    Ok(None)
}

fn document_mime(format: &str) -> String {
    match format {
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "csv" => "text/csv",
        "html" => "text/html",
        "md" => "text/markdown",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        other => return format!("application/{other}"),
    }
    .to_owned()
}

fn block(d: &mut Dec, o: &Obj, src: &Src) -> Result<Option<Block>, Invalid> {
    if let Some(t) = o.get("text").and_then(Value::as_str) {
        return Ok(Some(Block::Text {
            text: t.to_owned(),
            cache: None,
            src: src.key(o, "text"),
        }));
    }
    if let Some(Value::Object(img)) = o.get("image") {
        let isrc = src.key(o, "image");
        let format = img.get("format").and_then(Value::as_str).unwrap_or("png");
        let Some(media) = source(d, img, &isrc, format!("image/{format}"))? else {
            return Ok(None);
        };
        let media = match media {
            Media::Base64 { mime, data } => Media::Base64 {
                mime: image_mime(Some(&mime), &data),
                data,
            },
            other => other,
        };
        return Ok(Some(Block::Image {
            media,
            detail: None,
            cache: None,
            url_src: isrc.key(img, "source"),
            src: isrc,
        }));
    }
    if let Some(Value::Object(doc)) = o.get("document") {
        let dsrc = src.key(o, "document");
        let format = doc.get("format").and_then(Value::as_str).unwrap_or("pdf");
        let Some(media) = source(d, doc, &dsrc, document_mime(format))? else {
            return Ok(None);
        };
        return Ok(Some(Block::Document {
            media,
            name: doc
                .get("name")
                .and_then(Value::as_str)
                .map(|n| (n.to_owned(), dsrc.key(doc, "name"))),
            cache: None,
            url_src: dsrc.key(doc, "source"),
            src: dsrc,
        }));
    }
    if let Some(Value::Object(tu)) = o.get("toolUse") {
        let tsrc = src.key(o, "toolUse");
        let input = tu
            .get("input")
            .cloned()
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
        return Ok(Some(Block::ToolCall(Call {
            id: tu
                .get("toolUseId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            name: tu
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            args: input.is_object().then_some(input),
            raw: None,
            signature: None,
            cache: None,
            args_src: tsrc.key(tu, "input"),
            src: tsrc,
        })));
    }
    if let Some(Value::Object(tr)) = o.get("toolResult") {
        let rsrc = src.key(o, "toolResult");
        let csrc = rsrc.key(tr, "content");
        let mut content = Vec::new();
        for (i, x) in tr
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let xsrc = csrc.idx(i);
            let Some(xo) = x.as_object() else { continue };
            if let Some(j) = xo.get("json") {
                content.push(Block::Text {
                    text: compact(j),
                    cache: None,
                    src: xsrc.key(xo, "json"),
                });
            } else if let Some(b) = block(d, xo, &xsrc)? {
                content.push(b);
            }
        }
        let status = tr.get("status").and_then(Value::as_str);
        return Ok(Some(Block::ToolResult(ToolResult {
            id: tr
                .get("toolUseId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            content,
            is_error: status.map(|s| (s == "error", rsrc.key(tr, "status"))),
            cache: None,
            id_src: rsrc.key(tr, "toolUseId"),
            src: rsrc,
            content_src: csrc,
        })));
    }
    if let Some(Value::Object(rc)) = o.get("reasoningContent") {
        let rsrc = src.key(o, "reasoningContent");
        let text = rc.get("reasoningText");
        return Ok(Some(reasoning(
            text.and_then(|t| str_of(t, "text"))
                .unwrap_or_default()
                .to_owned(),
            text.and_then(|t| str_of(t, "signature")),
            rc.get("redactedContent").and_then(Value::as_str),
            Family::Anthropic,
            rsrc,
        )));
    }
    if o.contains_key("video") {
        d.unsupported(
            src.key(o, "video"),
            "Video input can only be read by a Bedrock Converse deployment.",
        );
        return Ok(None);
    }
    d.req.ignored.push(src.clone());
    Ok(None)
}
