//! Gemini `generateContent` requests. The body has no model and no stream flag: both come from the
//! path. Calls without an id get one from their position, and a function response without an id
//! answers the earliest unanswered call of its name.

use serde_json::Value;

use super::{Dec, Invalid, check_base64, invalid, reasoning};
use crate::xlate::envelope;
use crate::xlate::ir::{
    Block, Call, Choice, Effort, Family, Format, Hosted, Media, Mode, Role, SysBlock, Tool, ToolResult, Turn,
    WebSearch,
};
use crate::xlate::media::image_mime;
use crate::xlate::schema::from_subset;
use crate::xlate::util::{Obj, compact, str_of};
use crate::xlate::warn::Src;

const TOP: &[&str] = &[
    "contents",
    "systemInstruction",
    "tools",
    "toolConfig",
    "generationConfig",
    "safetySettings",
    "cachedContent",
    "labels",
];

const GENERATION: &[&str] = &[
    "temperature",
    "topP",
    "topK",
    "maxOutputTokens",
    "stopSequences",
    "candidateCount",
    "presencePenalty",
    "frequencyPenalty",
    "seed",
    "responseMimeType",
    "responseSchema",
    "responseJsonSchema",
    "responseLogprobs",
    "logprobs",
    "thinkingConfig",
    "responseModalities",
    "mediaResolution",
    "speechConfig",
];

/// Files API references are provider file ids (usable only by Gemini deployments).
const FILES_API: &str = "https://generativelanguage.googleapis.com/";

pub(super) fn decode(d: &mut Dec, obj: &Obj) -> Result<(), Invalid> {
    let root = Src::root();
    d.unknown_members(obj, &root, TOP, true);
    if let Some(Value::Object(si)) = obj.get("systemInstruction") {
        let at = root.key(obj, "systemInstruction");
        let parts_src = at.key(si, "parts");
        for (i, p) in si
            .get("parts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            if let Some(t) = str_of(p, "text") {
                d.req.system.push(SysBlock {
                    text: t.to_owned(),
                    cache: None,
                    developer: false,
                    src: parts_src.idx(i),
                });
            }
        }
    }
    if let Some(Value::Array(contents)) = obj.get("contents") {
        let at = root.key(obj, "contents");
        d.req.messages_src = at.clone();
        contents_of(d, contents, &at)?;
    }
    if let Some(Value::Array(tools)) = obj.get("tools") {
        let at = root.key(obj, "tools");
        d.req.tools_src = at.clone();
        tools_of(d, tools, &at);
    }
    if let Some(Value::Object(tc)) = obj.get("toolConfig") {
        let at = root.key(obj, "toolConfig");
        if let Some(Value::Object(fc)) = tc.get("functionCallingConfig") {
            let src = at.key(tc, "functionCallingConfig");
            let names: Vec<String> = fc
                .get("allowedFunctionNames")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            let choice = match fc.get("mode").and_then(Value::as_str) {
                Some("NONE") => Choice::None,
                Some("ANY" | "VALIDATED") if names.len() == 1 => {
                    Choice::Named(names.into_iter().next().unwrap_or_default())
                }
                Some("ANY" | "VALIDATED") if !names.is_empty() => Choice::Allowed {
                    required: true,
                    names,
                },
                Some("ANY" | "VALIDATED") => Choice::Required,
                _ if !names.is_empty() => Choice::Allowed {
                    required: false,
                    names,
                },
                _ => Choice::Auto,
            };
            d.req.choice = Some((choice, src));
        }
    }
    if let Some(Value::Object(g)) = obj.get("generationConfig") {
        generation(d, g, &root.key(obj, "generationConfig"));
    }
    if obj.get("safetySettings").is_some_and(|v| !v.is_null()) {
        d.req.hosted.push(Hosted {
            kind: "safetySettings".into(),
            unsupported: false,
            src: root.key(obj, "safetySettings"),
        });
    }
    if obj.get("cachedContent").is_some_and(|v| !v.is_null()) {
        d.unsupported(
            root.key(obj, "cachedContent"),
            "A Gemini cached content reference can only be used by a Gemini deployment.",
        );
    }
    if obj.get("labels").is_some_and(|v| !v.is_null()) {
        d.req.ignored.push(root.key(obj, "labels"));
    }
    Ok(())
}

fn generation(d: &mut Dec, g: &Obj, at: &Src) {
    d.unknown_members(g, at, GENERATION, false);
    d.req.temperature = Dec::num(g, at, "temperature");
    d.req.top_p = Dec::num(g, at, "topP");
    d.req.top_k = Dec::num(g, at, "topK");
    d.req.presence_penalty = Dec::num(g, at, "presencePenalty");
    d.req.frequency_penalty = Dec::num(g, at, "frequencyPenalty");
    d.req.seed = Dec::num(g, at, "seed");
    d.req.max_tokens = g
        .get("maxOutputTokens")
        .and_then(Value::as_u64)
        .map(|v| (v, at.key(g, "maxOutputTokens")));
    d.req.n = g
        .get("candidateCount")
        .and_then(Value::as_u64)
        .map(|v| (v, at.key(g, "candidateCount")));
    if let Some(Value::Array(a)) = g.get("stopSequences") {
        d.req.stop = Some((
            a.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
            at.key(g, "stopSequences"),
        ));
    }
    if g.get("responseLogprobs") == Some(&Value::Bool(true)) {
        d.req.logprobs = Dec::num(g, at, "responseLogprobs");
        d.req.top_logprobs = Dec::num(g, at, "logprobs");
    }
    if g.get("responseMimeType").and_then(Value::as_str) == Some("application/json") {
        let (schema, field) = match (g.get("responseJsonSchema"), g.get("responseSchema")) {
            (Some(s), _) if !s.is_null() => (Some(s.clone()), "responseJsonSchema"),
            (_, Some(s)) if !s.is_null() => (Some(from_subset(s)), "responseSchema"),
            _ => (None, "responseMimeType"),
        };
        let format = match schema {
            Some(schema) => Format::JsonSchema {
                name: None,
                schema,
                strict: None,
            },
            None => Format::JsonObject,
        };
        d.req.format = Some((format, at.key(g, field)));
    }
    if let Some(Value::Object(t)) = g.get("thinkingConfig") {
        let src = at.key(g, "thinkingConfig");
        let r = &mut d.req.reason;
        r.src = src;
        match t.get("thinkingBudget").and_then(Value::as_i64) {
            Some(0) => r.mode = Mode::Off,
            Some(-1) => r.mode = Mode::Adaptive,
            Some(n) if n > 0 => {
                r.mode = Mode::On;
                r.budget = u64::try_from(n).ok();
            }
            _ => {}
        }
        if let Some(level) = t
            .get("thinkingLevel")
            .and_then(Value::as_str)
            .and_then(|l| Effort::parse(&l.to_ascii_lowercase()))
        {
            if r.mode == Mode::Unspecified {
                r.mode = Mode::On;
            }
            r.effort = Some(level);
        }
        if let Some(Value::Bool(b)) = t.get("includeThoughts") {
            r.include_thoughts = Some(*b);
            r.summaries = *b;
        }
    }
    let other_modality = g
        .get("responseModalities")
        .and_then(Value::as_array)
        .is_some_and(|m| {
            m.iter()
                .any(|x| x.as_str().is_some_and(|s| !s.eq_ignore_ascii_case("text")))
        });
    if other_modality {
        d.unsupported(
            at.key(g, "responseModalities"),
            "Audio or image output can only be produced by a Gemini deployment.",
        );
    }
    if g.get("speechConfig").is_some_and(|v| !v.is_null()) {
        d.unsupported(
            at.key(g, "speechConfig"),
            "Audio output can only be produced by a Gemini deployment.",
        );
    }
    if g.get("mediaResolution").is_some_and(|v| !v.is_null()) {
        d.req.ignored.push(at.key(g, "mediaResolution"));
    }
}

fn tools_of(d: &mut Dec, tools: &[Value], at: &Src) {
    for (i, t) in tools.iter().enumerate() {
        let src = at.idx(i);
        let Some(o) = t.as_object() else { continue };
        for (k, v) in o {
            let ksrc = src.key(o, k);
            if matches!(k.as_str(), "googleSearch" | "googleSearchRetrieval") {
                d.req.web_search = Some(WebSearch::at(ksrc));
                continue;
            }
            if k != "functionDeclarations" {
                d.req.hosted.push(Hosted {
                    kind: k.clone(),
                    unsupported: false,
                    src: ksrc,
                });
                continue;
            }
            for (j, f) in v.as_array().into_iter().flatten().enumerate() {
                let Some(fo) = f.as_object() else { continue };
                let fsrc = ksrc.idx(j);
                d.unknown_members(
                    fo,
                    &fsrc,
                    &[
                        "name",
                        "description",
                        "parameters",
                        "parametersJsonSchema",
                        "response",
                        "responseJsonSchema",
                        "behavior",
                    ],
                    false,
                );
                let (parameters, field) = match (fo.get("parametersJsonSchema"), fo.get("parameters")) {
                    (Some(p), _) if !p.is_null() => (Some(p.clone()), "parametersJsonSchema"),
                    (_, Some(p)) if !p.is_null() => (Some(from_subset(p)), "parameters"),
                    _ => (None, "parameters"),
                };
                d.req.tools.push(Tool {
                    name: str_of(f, "name").unwrap_or_default().to_owned(),
                    description: str_of(f, "description").map(str::to_owned),
                    parameters,
                    strict: None,
                    cache: None,
                    custom: None,
                    params_src: fsrc.key(fo, field),
                    src: fsrc,
                });
            }
        }
    }
}

/// A call no function response has answered yet.
struct Open {
    id: String,
    name: String,
}

fn contents_of(d: &mut Dec, contents: &[Value], at: &Src) -> Result<(), Invalid> {
    let mut open: Vec<Open> = Vec::new();
    for (i, c) in contents.iter().enumerate() {
        let src = at.idx(i);
        let Some(o) = c.as_object() else { continue };
        d.unknown_members(o, &src, &["role", "parts"], false);
        let model = str_of(c, "role") == Some("model");
        let role_src = src.key(o, "role");
        let parts_src = src.key(o, "parts");
        let mut current: Vec<Block> = Vec::new();
        let flush = |d: &mut Dec, current: &mut Vec<Block>| {
            if !current.is_empty() {
                d.req.turns.push(Turn {
                    role: if model { Role::Assistant } else { Role::User },
                    blocks: std::mem::take(current),
                    cache: None,
                    src: src.clone(),
                    role_src: role_src.clone(),
                });
            }
        };
        for (j, p) in o
            .get("parts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let psrc = parts_src.idx(j);
            let Some(po) = p.as_object() else { continue };
            if let Some(Value::Object(fr)) = po.get("functionResponse") {
                flush(d, &mut current);
                let frsrc = psrc.key(po, "functionResponse");
                let name = fr.get("name").and_then(Value::as_str).unwrap_or_default();
                let id = match fr.get("id").and_then(Value::as_str).filter(|x| !x.is_empty()) {
                    Some(id) => {
                        open.retain(|c| c.id != id);
                        id.to_owned()
                    }
                    None => answer(&mut open, name),
                };
                let id_src = if fr.contains_key("id") {
                    frsrc.key(fr, "id")
                } else {
                    frsrc.clone()
                };
                let rsrc = frsrc.key(fr, "response");
                let text = match fr.get("response") {
                    Some(Value::Object(r)) if r.len() == 1 => match r.values().next() {
                        Some(Value::String(s)) if r.contains_key("content") || r.contains_key("output") => {
                            s.clone()
                        }
                        _ => compact(&Value::Object(r.clone())),
                    },
                    Some(other) => compact(other),
                    None => String::new(),
                };
                d.req.turns.push(Turn {
                    role: Role::Tool,
                    blocks: vec![Block::ToolResult(ToolResult {
                        id,
                        content: vec![Block::Text {
                            text,
                            cache: None,
                            src: rsrc.clone(),
                        }],
                        is_error: None,
                        cache: None,
                        id_src,
                        src: psrc.clone(),
                        content_src: rsrc,
                    })],
                    cache: None,
                    src: src.clone(),
                    role_src: role_src.clone(),
                });
                continue;
            }
            part(d, po, &psrc, (i, j), model, &mut current, &mut open)?;
        }
        flush(d, &mut current);
    }
    Ok(())
}

/// The id a function response without one answers: the earliest unanswered call of its name, else
/// the earliest unanswered call.
fn answer(open: &mut Vec<Open>, name: &str) -> String {
    let at = open
        .iter()
        .position(|c| c.name == name)
        .or_else(|| (!open.is_empty()).then_some(0));
    match at {
        Some(i) => open.remove(i).id,
        None => String::new(),
    }
}

/// The next thought part of a streamed thought joins the previous one: its text is appended and the
/// signature that ends the thought (on its last part) is the block's.
fn merge_thought(prev: &mut crate::xlate::ir::Reasoning, next: crate::xlate::ir::Reasoning) {
    prev.text.push_str(&next.text);
    if next.signature.is_some() || next.redacted.is_some() {
        prev.signature = next.signature;
        prev.redacted = next.redacted;
        prev.family = next.family;
        prev.provider = next.provider;
    }
}

/// A thought signature on a part: a raw Gemini payload, or the envelope of another family.
fn signature_block(sig: &str, src: &Src) -> Block {
    reasoning(String::new(), Some(sig), None, Family::Gemini, src.clone())
}

fn part(
    d: &mut Dec,
    po: &Obj,
    psrc: &Src,
    (i, j): (usize, usize),
    model: bool,
    current: &mut Vec<Block>,
    open: &mut Vec<Open>,
) -> Result<(), Invalid> {
    let sig = po
        .get("thoughtSignature")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let sig_src = psrc.key(po, "thoughtSignature");
    if let Some(Value::Object(fc)) = po.get("functionCall") {
        let fsrc = psrc.key(po, "functionCall");
        let id = fc
            .get("id")
            .and_then(Value::as_str)
            .filter(|x| !x.is_empty())
            .map_or_else(|| format!("call_{i}_{j}"), str::to_owned);
        let name = fc
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let args = fc
            .get("args")
            .cloned()
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
        let signature = match sig {
            Some(s) if envelope::is_wrapped(s) => {
                current.push(signature_block(s, &sig_src));
                None
            }
            Some(s) => Some(s.to_owned()),
            None => None,
        };
        open.push(Open {
            id: id.clone(),
            name: name.clone(),
        });
        current.push(Block::ToolCall(Call {
            id,
            name,
            args: args.is_object().then_some(args),
            raw: None,
            signature,
            cache: None,
            args_src: fsrc.key(fc, "args"),
            src: fsrc,
        }));
        return Ok(());
    }
    if let Some(text) = po.get("text").and_then(Value::as_str) {
        if po.get("thought") == Some(&Value::Bool(true)) {
            let block = reasoning(text.to_owned(), sig, None, Family::Gemini, psrc.clone());
            match (current.last_mut(), block) {
                (Some(Block::Reasoning(prev)), Block::Reasoning(next))
                    if prev.signature.is_none() && prev.redacted.is_none() =>
                {
                    merge_thought(prev, next);
                }
                (_, block) => current.push(block),
            }
            return Ok(());
        }
        if let (Some(s), true) = (sig, model) {
            current.push(signature_block(s, &sig_src));
        }
        current.push(Block::Text {
            text: text.to_owned(),
            cache: None,
            src: psrc.clone(),
        });
        return Ok(());
    }
    if let Some(Value::Object(inline)) = po.get("inlineData") {
        let isrc = psrc.key(po, "inlineData");
        let dsrc = isrc.key(inline, "data");
        let data = inline.get("data").and_then(Value::as_str).unwrap_or_default();
        check_base64(data, &dsrc)?;
        let mime = inline.get("mimeType").and_then(Value::as_str).unwrap_or_default();
        current.push(media_block(
            mime,
            Media::Base64 {
                mime: mime.to_owned(),
                data: data.to_owned(),
            },
            psrc,
            dsrc,
            data,
        ));
        return Ok(());
    }
    if let Some(Value::Object(file)) = po.get("fileData") {
        let fsrc = psrc.key(po, "fileData");
        let usrc = fsrc.key(file, "fileUri");
        let uri = file
            .get("fileUri")
            .and_then(Value::as_str)
            .filter(|u| !u.is_empty());
        let Some(uri) = uri else {
            return Err(invalid(&usrc, "must be a non-empty URI."));
        };
        let mime = file.get("mimeType").and_then(Value::as_str).unwrap_or_default();
        let media = if uri.starts_with(FILES_API) || !uri.starts_with("http") {
            Media::FileId(uri.to_owned())
        } else {
            Media::Url(uri.to_owned())
        };
        current.push(media_block(mime, media, psrc, usrc, ""));
        return Ok(());
    }
    d.req.ignored.push(psrc.clone());
    Ok(())
}

/// An inline or referenced part by its media type: an image, audio, or a document.
fn media_block(mime: &str, media: Media, src: &Src, url_src: Src, data: &str) -> Block {
    if mime.starts_with("image/") {
        let media = match media {
            Media::Base64 { mime, data } => Media::Base64 {
                mime: image_mime(Some(&mime), &data),
                data,
            },
            other => other,
        };
        return Block::Image {
            media,
            detail: None,
            cache: None,
            src: src.clone(),
            url_src,
        };
    }
    if let (Some(format), Media::Base64 { .. }) = (mime.strip_prefix("audio/"), &media) {
        return Block::Audio {
            data: data.to_owned(),
            format: format.to_owned(),
            src: src.clone(),
        };
    }
    let media = match media {
        Media::Base64 { data, .. } if mime.is_empty() => Media::Base64 {
            mime: "application/pdf".into(),
            data,
        },
        other => other,
    };
    Block::Document {
        media,
        name: None,
        cache: None,
        src: src.clone(),
        url_src,
    }
}
