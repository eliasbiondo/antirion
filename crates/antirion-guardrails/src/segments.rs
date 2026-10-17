//! Segments: the JSON strings of a request or response body that checks inspect, with their kind,
//! origin and location, extracted per dialect in body order, and written back in place.

use serde_json::Value;

use crate::def::{Origin, SegKind};

/// The member transforms put on objects they insert, so their text counts as operator content.
/// Stripped before the body leaves the gateway.
pub const OPERATOR_MARK: &str = "\u{1}antirion_operator";

/// The body grammar segments are read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Chat,
    Completions,
    Responses,
    Anthropic,
    Gemini,
    Converse,
    Embeddings,
    Images,
    Rerank,
    Transcription,
}

impl Dialect {
    /// Embeddings, image generation, rerank and transcription: no conversation, no tools.
    #[must_use]
    pub const fn is_flat(self) -> bool {
        matches!(
            self,
            Dialect::Embeddings | Dialect::Images | Dialect::Rerank | Dialect::Transcription
        )
    }
}

/// Where a segment's text lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Loc {
    /// A string at a JSON pointer.
    Ptr(String),
    /// A string leaf inside a JSON document carried as a string (tool-call arguments).
    Leaf { outer: String, inner: String },
    /// A tool definition: `name + "\n" + description`.
    ToolDef { name: String, desc: String },
}

/// One inspected string.
#[derive(Clone, Debug)]
pub struct Segment {
    pub kind: SegKind,
    pub origin: Origin,
    pub loc: Loc,
    pub text: String,
    /// The index of the message it belongs to, for `scope.messages: last_user`.
    pub message: Option<usize>,
}

impl Segment {
    /// The JSON pointer reported to webhooks.
    #[must_use]
    pub fn path(&self) -> String {
        match &self.loc {
            Loc::Ptr(p) => p.clone(),
            Loc::Leaf { outer, inner } => format!("{outer}{inner}"),
            Loc::ToolDef { desc, .. } => desc.clone(),
        }
    }
}

fn esc(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

fn marked(v: &Value) -> bool {
    v.get(OPERATOR_MARK).is_some()
}

struct Out<'a> {
    segs: Vec<Segment>,
    /// Top-level string members a transform wrote whole.
    operator_ptrs: &'a [String],
}

impl Out<'_> {
    fn push(&mut self, kind: SegKind, ptr: String, text: &str, operator: bool, message: Option<usize>) {
        let origin = if operator || self.operator_ptrs.contains(&ptr) {
            Origin::Operator
        } else {
            Origin::Client
        };
        self.segs.push(Segment {
            kind,
            origin,
            loc: Loc::Ptr(ptr),
            text: text.to_owned(),
            message,
        });
    }

    /// Every string leaf of `v`, under `ptr`.
    fn leaves(&mut self, kind: SegKind, v: &Value, ptr: &str, operator: bool, message: Option<usize>) {
        match v {
            Value::String(s) => self.push(kind, ptr.to_owned(), s, operator, message),
            Value::Array(a) => {
                for (i, x) in a.iter().enumerate() {
                    self.leaves(kind, x, &format!("{ptr}/{i}"), operator, message);
                }
            }
            Value::Object(o) => {
                for (k, x) in o {
                    self.leaves(kind, x, &format!("{ptr}/{}", esc(k)), operator, message);
                }
            }
            _ => {}
        }
    }

    /// Tool-call arguments: each string leaf when they parse as JSON, otherwise the whole string.
    fn arguments(&mut self, kind: SegKind, v: &Value, ptr: String, operator: bool, message: Option<usize>) {
        let Some(s) = v.as_str() else {
            return;
        };
        match serde_json::from_str::<Value>(s) {
            Ok(doc @ (Value::Object(_) | Value::Array(_))) => {
                let mut inner = Out {
                    segs: Vec::new(),
                    operator_ptrs: &[],
                };
                inner.leaves(kind, &doc, "", operator, message);
                for mut seg in inner.segs {
                    if let Loc::Ptr(p) = seg.loc {
                        seg.loc = Loc::Leaf {
                            outer: ptr.clone(),
                            inner: p,
                        };
                    }
                    if operator {
                        seg.origin = Origin::Operator;
                    }
                    self.segs.push(seg);
                }
            }
            Ok(Value::String(_)) | Err(_) => self.push(kind, ptr, s, operator, message),
            Ok(_) => {}
        }
    }

    fn tool_def(
        &mut self,
        name_ptr: String,
        desc_ptr: String,
        name: &str,
        desc: Option<&str>,
        operator: bool,
    ) {
        self.segs.push(Segment {
            kind: SegKind::ToolDefinition,
            origin: if operator {
                Origin::Operator
            } else {
                Origin::Client
            },
            loc: Loc::ToolDef {
                name: name_ptr,
                desc: desc_ptr,
            },
            text: format!("{name}\n{}", desc.unwrap_or_default()),
            message: None,
        });
    }

    /// Text parts of a content array (`text` members of text-like parts).
    fn parts(&mut self, kind: SegKind, parts: &[Value], base: &str, operator: bool, message: Option<usize>) {
        for (j, p) in parts.iter().enumerate() {
            let t = p.get("type").and_then(Value::as_str);
            if matches!(t, None | Some("text" | "input_text" | "output_text"))
                && let Some(s) = p.get("text").and_then(Value::as_str)
            {
                self.push(
                    kind,
                    format!("{base}/{j}/text"),
                    s,
                    operator || marked(p),
                    message,
                );
            }
        }
    }

    fn content(
        &mut self,
        kind: SegKind,
        v: Option<&Value>,
        base: String,
        operator: bool,
        message: Option<usize>,
    ) {
        match v {
            Some(Value::String(s)) => self.push(kind, base, s, operator, message),
            Some(Value::Array(a)) => self.parts(kind, a, &base, operator, message),
            _ => {}
        }
    }
}

fn arr<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key).and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn role_kind(role: Option<&str>) -> Option<SegKind> {
    Some(match role? {
        "system" | "developer" => SegKind::System,
        "user" => SegKind::User,
        "assistant" | "model" => SegKind::Assistant,
        "tool" | "function" => SegKind::Tool,
        _ => return None,
    })
}

/// The input segments of a client body, and the index of its last user message.
#[must_use]
pub fn input_segments(body: &Value, d: Dialect, operator_ptrs: &[String]) -> (Vec<Segment>, Option<usize>) {
    let mut out = Out {
        segs: Vec::new(),
        operator_ptrs,
    };
    let mut last_user = None;
    if d.is_flat() {
        return (crate::endpoints::input(body, d), None);
    }
    match d {
        Dialect::Chat => chat_input(body, &mut out, &mut last_user),
        Dialect::Completions => match body.get("prompt") {
            Some(Value::String(s)) => {
                out.push(SegKind::User, "/prompt".into(), s, false, Some(0));
                last_user = Some(0);
            }
            Some(Value::Array(a)) => {
                for (i, s) in a.iter().enumerate() {
                    if let Some(s) = s.as_str() {
                        out.push(SegKind::User, format!("/prompt/{i}"), s, false, Some(i));
                        last_user = Some(i);
                    }
                }
            }
            _ => {}
        },
        Dialect::Responses => responses_input(body, &mut out, &mut last_user),
        Dialect::Anthropic => anthropic_input(body, &mut out, &mut last_user),
        Dialect::Gemini => gemini_input(body, &mut out, &mut last_user),
        Dialect::Converse => converse_input(body, &mut out, &mut last_user),
        Dialect::Embeddings | Dialect::Images | Dialect::Rerank | Dialect::Transcription => {}
    }
    (out.segs, last_user)
}

fn chat_input(body: &Value, out: &mut Out<'_>, last_user: &mut Option<usize>) {
    for (i, m) in arr(body, "messages").iter().enumerate() {
        let Some(kind) = role_kind(str_of(m, "role")) else {
            continue;
        };
        if kind == SegKind::User {
            *last_user = Some(i);
        }
        let op = marked(m);
        let base = format!("/messages/{i}");
        out.content(kind, m.get("content"), format!("{base}/content"), op, Some(i));
        if kind == SegKind::Assistant {
            if let Some(r) = str_of(m, "refusal") {
                out.push(kind, format!("{base}/refusal"), r, op, Some(i));
            }
            for (k, c) in arr(m, "tool_calls").iter().enumerate() {
                if let Some(a) = c.get("function").and_then(|f| f.get("arguments")) {
                    out.arguments(
                        kind,
                        a,
                        format!("{base}/tool_calls/{k}/function/arguments"),
                        op,
                        Some(i),
                    );
                }
            }
        }
    }
    for (t, tool) in arr(body, "tools").iter().enumerate() {
        let f = tool.get("function").unwrap_or(tool);
        if let Some(name) = str_of(f, "name") {
            let base = if tool.get("function").is_some() {
                format!("/tools/{t}/function")
            } else {
                format!("/tools/{t}")
            };
            out.tool_def(
                format!("{base}/name"),
                format!("{base}/description"),
                name,
                str_of(f, "description"),
                marked(tool),
            );
        }
    }
}

fn responses_input(body: &Value, out: &mut Out<'_>, last_user: &mut Option<usize>) {
    if let Some(s) = str_of(body, "instructions") {
        out.push(SegKind::System, "/instructions".into(), s, false, None);
    }
    match body.get("input") {
        Some(Value::String(s)) => {
            out.push(SegKind::User, "/input".into(), s, false, Some(0));
            *last_user = Some(0);
        }
        Some(Value::Array(items)) => {
            for (i, item) in items.iter().enumerate() {
                let op = marked(item);
                let base = format!("/input/{i}");
                match str_of(item, "type") {
                    None | Some("message") => {
                        let Some(kind) = role_kind(str_of(item, "role")) else {
                            continue;
                        };
                        if kind == SegKind::User {
                            *last_user = Some(i);
                        }
                        out.content(kind, item.get("content"), format!("{base}/content"), op, Some(i));
                    }
                    Some("function_call") => {
                        if let Some(a) = item.get("arguments") {
                            out.arguments(SegKind::Assistant, a, format!("{base}/arguments"), op, Some(i));
                        }
                    }
                    Some("function_call_output") => {
                        out.content(
                            SegKind::Tool,
                            item.get("output"),
                            format!("{base}/output"),
                            op,
                            Some(i),
                        );
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    for (t, tool) in arr(body, "tools").iter().enumerate() {
        if let Some(name) = str_of(tool, "name") {
            out.tool_def(
                format!("/tools/{t}/name"),
                format!("/tools/{t}/description"),
                name,
                str_of(tool, "description"),
                marked(tool),
            );
        }
    }
}

fn anthropic_blocks(kind: SegKind, blocks: &[Value], base: &str, out: &mut Out<'_>, op: bool, i: usize) {
    for (j, b) in blocks.iter().enumerate() {
        let at = format!("{base}/{j}");
        let op = op || marked(b);
        match str_of(b, "type") {
            Some("text") => {
                if let Some(s) = str_of(b, "text") {
                    out.push(kind, format!("{at}/text"), s, op, Some(i));
                }
            }
            Some("tool_use") => {
                if let Some(input) = b.get("input") {
                    out.leaves(SegKind::Assistant, input, &format!("{at}/input"), op, Some(i));
                }
            }
            Some("tool_result") => match b.get("content") {
                Some(Value::String(s)) => out.push(SegKind::Tool, format!("{at}/content"), s, op, Some(i)),
                Some(Value::Array(a)) => out.parts(SegKind::Tool, a, &format!("{at}/content"), op, Some(i)),
                _ => {}
            },
            _ => {}
        }
    }
}

fn anthropic_input(body: &Value, out: &mut Out<'_>, last_user: &mut Option<usize>) {
    match body.get("system") {
        Some(Value::String(s)) => out.push(SegKind::System, "/system".into(), s, false, None),
        Some(Value::Array(a)) => out.parts(SegKind::System, a, "/system", false, None),
        _ => {}
    }
    for (i, m) in arr(body, "messages").iter().enumerate() {
        let Some(kind) = role_kind(str_of(m, "role")) else {
            continue;
        };
        if kind == SegKind::User {
            *last_user = Some(i);
        }
        let op = marked(m);
        let base = format!("/messages/{i}/content");
        match m.get("content") {
            Some(Value::String(s)) => out.push(kind, base, s, op, Some(i)),
            Some(Value::Array(a)) => anthropic_blocks(kind, a, &base, out, op, i),
            _ => {}
        }
    }
    for (t, tool) in arr(body, "tools").iter().enumerate() {
        if let Some(name) = str_of(tool, "name") {
            out.tool_def(
                format!("/tools/{t}/name"),
                format!("/tools/{t}/description"),
                name,
                str_of(tool, "description"),
                marked(tool),
            );
        }
    }
}

fn gemini_input(body: &Value, out: &mut Out<'_>, last_user: &mut Option<usize>) {
    for key in ["systemInstruction", "system_instruction"] {
        if let Some(si) = body.get(key) {
            out.parts(
                SegKind::System,
                arr(si, "parts"),
                &format!("/{key}/parts"),
                false,
                None,
            );
        }
    }
    for (i, c) in arr(body, "contents").iter().enumerate() {
        let kind = role_kind(str_of(c, "role").or(Some("user"))).unwrap_or(SegKind::User);
        if kind == SegKind::User {
            *last_user = Some(i);
        }
        for (j, p) in arr(c, "parts").iter().enumerate() {
            let at = format!("/contents/{i}/parts/{j}");
            if let Some(s) = str_of(p, "text") {
                out.push(kind, format!("{at}/text"), s, false, Some(i));
            }
            if let Some(a) = p.get("functionCall").and_then(|f| f.get("args")) {
                out.leaves(
                    SegKind::Assistant,
                    a,
                    &format!("{at}/functionCall/args"),
                    false,
                    Some(i),
                );
            }
            if let Some(r) = p.get("functionResponse").and_then(|f| f.get("response")) {
                out.leaves(
                    SegKind::Tool,
                    r,
                    &format!("{at}/functionResponse/response"),
                    false,
                    Some(i),
                );
            }
        }
    }
    for (t, tool) in arr(body, "tools").iter().enumerate() {
        for (k, f) in arr(tool, "functionDeclarations").iter().enumerate() {
            if let Some(name) = str_of(f, "name") {
                let base = format!("/tools/{t}/functionDeclarations/{k}");
                out.tool_def(
                    format!("{base}/name"),
                    format!("{base}/description"),
                    name,
                    str_of(f, "description"),
                    false,
                );
            }
        }
    }
}

fn converse_input(body: &Value, out: &mut Out<'_>, last_user: &mut Option<usize>) {
    for (j, b) in arr(body, "system").iter().enumerate() {
        if let Some(s) = str_of(b, "text") {
            out.push(SegKind::System, format!("/system/{j}/text"), s, false, None);
        }
    }
    for (i, m) in arr(body, "messages").iter().enumerate() {
        let Some(kind) = role_kind(str_of(m, "role")) else {
            continue;
        };
        if kind == SegKind::User {
            *last_user = Some(i);
        }
        for (j, b) in arr(m, "content").iter().enumerate() {
            let at = format!("/messages/{i}/content/{j}");
            if let Some(s) = str_of(b, "text") {
                out.push(kind, format!("{at}/text"), s, false, Some(i));
            }
            if let Some(input) = b.get("toolUse").and_then(|t| t.get("input")) {
                out.leaves(
                    SegKind::Assistant,
                    input,
                    &format!("{at}/toolUse/input"),
                    false,
                    Some(i),
                );
            }
            if let Some(r) = b.get("toolResult") {
                for (k, c) in arr(r, "content").iter().enumerate() {
                    let cat = format!("{at}/toolResult/content/{k}");
                    if let Some(s) = str_of(c, "text") {
                        out.push(SegKind::Tool, format!("{cat}/text"), s, false, Some(i));
                    }
                    if let Some(j) = c.get("json") {
                        out.leaves(SegKind::Tool, j, &format!("{cat}/json"), false, Some(i));
                    }
                }
            }
        }
    }
    let tools = body.get("toolConfig").map_or(&[][..], |t| arr(t, "tools"));
    for (t, tool) in tools.iter().enumerate() {
        if let Some(spec) = tool.get("toolSpec")
            && let Some(name) = str_of(spec, "name")
        {
            let base = format!("/toolConfig/tools/{t}/toolSpec");
            out.tool_def(
                format!("{base}/name"),
                format!("{base}/description"),
                name,
                str_of(spec, "description"),
                false,
            );
        }
    }
}

/// The output segments of a response body in the client's dialect, for every choice.
#[must_use]
pub fn output_segments(body: &Value, d: Dialect) -> Vec<Segment> {
    let mut out = Out {
        segs: Vec::new(),
        operator_ptrs: &[],
    };
    let model = |out: &mut Out<'_>, kind: SegKind, ptr: String, s: &str| {
        out.segs.push(Segment {
            kind,
            origin: Origin::Model,
            loc: Loc::Ptr(ptr),
            text: s.to_owned(),
            message: None,
        });
    };
    if d.is_flat() {
        return crate::endpoints::output(body, d);
    }
    match d {
        Dialect::Chat | Dialect::Completions => {
            for (c, choice) in arr(body, "choices").iter().enumerate() {
                if let Some(s) = str_of(choice, "text") {
                    model(&mut out, SegKind::Content, format!("/choices/{c}/text"), s);
                }
                let Some(m) = choice.get("message") else { continue };
                let base = format!("/choices/{c}/message");
                match m.get("content") {
                    Some(Value::String(s)) => model(&mut out, SegKind::Content, format!("{base}/content"), s),
                    Some(Value::Array(a)) => {
                        out.parts(SegKind::Content, a, &format!("{base}/content"), false, None)
                    }
                    _ => {}
                }
                for field in ["reasoning_content", "reasoning"] {
                    if let Some(s) = str_of(m, field) {
                        model(&mut out, SegKind::Reasoning, format!("{base}/{field}"), s);
                    }
                }
                if let Some(s) = str_of(m, "refusal") {
                    model(&mut out, SegKind::Refusal, format!("{base}/refusal"), s);
                }
                for (k, call) in arr(m, "tool_calls").iter().enumerate() {
                    if let Some(a) = call.get("function").and_then(|f| f.get("arguments")) {
                        out.arguments(
                            SegKind::ToolArguments,
                            a,
                            format!("{base}/tool_calls/{k}/function/arguments"),
                            false,
                            None,
                        );
                    }
                }
            }
        }
        Dialect::Anthropic => {
            for (j, b) in arr(body, "content").iter().enumerate() {
                match str_of(b, "type") {
                    Some("text") => {
                        if let Some(s) = str_of(b, "text") {
                            model(&mut out, SegKind::Content, format!("/content/{j}/text"), s);
                        }
                    }
                    Some("thinking") => {
                        if let Some(s) = str_of(b, "thinking") {
                            model(&mut out, SegKind::Reasoning, format!("/content/{j}/thinking"), s);
                        }
                    }
                    Some("tool_use") => {
                        if let Some(input) = b.get("input") {
                            out.leaves(
                                SegKind::ToolArguments,
                                input,
                                &format!("/content/{j}/input"),
                                false,
                                None,
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
        Dialect::Responses => {
            for (i, item) in arr(body, "output").iter().enumerate() {
                let base = format!("/output/{i}");
                match str_of(item, "type") {
                    Some("message") => {
                        for (j, p) in arr(item, "content").iter().enumerate() {
                            match str_of(p, "type") {
                                Some("output_text") => {
                                    if let Some(s) = str_of(p, "text") {
                                        model(
                                            &mut out,
                                            SegKind::Content,
                                            format!("{base}/content/{j}/text"),
                                            s,
                                        );
                                    }
                                }
                                Some("refusal") => {
                                    if let Some(s) = str_of(p, "refusal") {
                                        model(
                                            &mut out,
                                            SegKind::Refusal,
                                            format!("{base}/content/{j}/refusal"),
                                            s,
                                        );
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    Some("reasoning") => {
                        for (k, s) in arr(item, "summary").iter().enumerate() {
                            if let Some(t) = str_of(s, "text") {
                                model(
                                    &mut out,
                                    SegKind::Reasoning,
                                    format!("{base}/summary/{k}/text"),
                                    t,
                                );
                            }
                        }
                    }
                    Some("function_call") => {
                        if let Some(a) = item.get("arguments") {
                            out.arguments(
                                SegKind::ToolArguments,
                                a,
                                format!("{base}/arguments"),
                                false,
                                None,
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
        Dialect::Gemini => {
            for (c, cand) in arr(body, "candidates").iter().enumerate() {
                let Some(content) = cand.get("content") else {
                    continue;
                };
                for (j, p) in arr(content, "parts").iter().enumerate() {
                    let at = format!("/candidates/{c}/content/parts/{j}");
                    if let Some(s) = str_of(p, "text") {
                        let kind = if p.get("thought").and_then(Value::as_bool) == Some(true) {
                            SegKind::Reasoning
                        } else {
                            SegKind::Content
                        };
                        model(&mut out, kind, format!("{at}/text"), s);
                    }
                    if let Some(a) = p.get("functionCall").and_then(|f| f.get("args")) {
                        out.leaves(
                            SegKind::ToolArguments,
                            a,
                            &format!("{at}/functionCall/args"),
                            false,
                            None,
                        );
                    }
                }
            }
        }
        Dialect::Converse => {
            let content = body
                .pointer("/output/message/content")
                .and_then(Value::as_array)
                .map_or(&[][..], Vec::as_slice);
            for (j, b) in content.iter().enumerate() {
                let at = format!("/output/message/content/{j}");
                if let Some(s) = str_of(b, "text") {
                    model(&mut out, SegKind::Content, format!("{at}/text"), s);
                }
                if let Some(s) = b
                    .pointer("/reasoningContent/reasoningText/text")
                    .and_then(Value::as_str)
                {
                    model(
                        &mut out,
                        SegKind::Reasoning,
                        format!("{at}/reasoningContent/reasoningText/text"),
                        s,
                    );
                }
                if let Some(input) = b.get("toolUse").and_then(|t| t.get("input")) {
                    out.leaves(
                        SegKind::ToolArguments,
                        input,
                        &format!("{at}/toolUse/input"),
                        false,
                        None,
                    );
                }
            }
        }
        Dialect::Embeddings | Dialect::Images | Dialect::Rerank | Dialect::Transcription => {}
    }
    for s in &mut out.segs {
        s.origin = Origin::Model;
    }
    out.segs
}

fn set_ptr(body: &mut Value, ptr: &str, text: &str) {
    if ptr.is_empty() {
        *body = Value::String(text.to_owned());
    } else if let Some(slot) = body.pointer_mut(ptr) {
        *slot = Value::String(text.to_owned());
    }
}

/// Writes a segment's (changed) text back at its location.
pub fn write_back(body: &mut Value, seg: &Segment) {
    match &seg.loc {
        Loc::Ptr(p) => set_ptr(body, p, &seg.text),
        Loc::Leaf { outer, inner } => {
            let Some(raw) = body.pointer(outer).and_then(Value::as_str) else {
                return;
            };
            let Ok(mut doc) = serde_json::from_str::<Value>(raw) else {
                return;
            };
            set_ptr(&mut doc, inner, &seg.text);
            if let Ok(s) = serde_json::to_string(&doc) {
                set_ptr(body, outer, &s);
            }
        }
        Loc::ToolDef { name, desc } => {
            let Some(n) = body.pointer(name).and_then(Value::as_str).map(str::to_owned) else {
                return;
            };
            if let Some(rest) = seg.text.strip_prefix(&format!("{n}\n")) {
                if rest.is_empty() && body.pointer(desc).is_none() {
                    return;
                }
                if let Some(slot) = body.pointer_mut(desc) {
                    *slot = Value::String(rest.to_owned());
                } else if let Some((parent, _)) = desc.rsplit_once('/')
                    && let Some(Value::Object(o)) = body.pointer_mut(parent)
                {
                    o.insert("description".into(), Value::String(rest.to_owned()));
                }
            }
        }
    }
}

/// Removes the operator marks transforms left on inserted objects.
pub fn strip_marks(v: &mut Value) {
    match v {
        Value::Object(o) => {
            o.remove(OPERATOR_MARK);
            o.values_mut().for_each(strip_marks);
        }
        Value::Array(a) => a.iter_mut().for_each(strip_marks),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn kinds(segs: &[Segment]) -> Vec<(SegKind, String, String)> {
        segs.iter().map(|s| (s.kind, s.path(), s.text.clone())).collect()
    }

    #[test]
    fn chat_input_covers_the_whole_conversation() {
        let body = json!({"messages": [
            {"role": "system", "content": "s"},
            {"role": "user", "content": [{"type": "text", "text": "u"}, {"type": "image_url", "image_url": {"url": "https://x/secret"}}]},
            {"role": "assistant", "content": null, "refusal": "r", "tool_calls": [{"id": "c", "type": "function", "function": {"name": "f", "arguments": "{\"q\": [\"leaf\"]}"}}]},
            {"role": "tool", "tool_call_id": "c", "content": "t"}],
            "tools": [{"type": "function", "function": {"name": "f", "description": "d"}}]});
        let (segs, last) = input_segments(&body, Dialect::Chat, &[]);
        assert_eq!(last, Some(1));
        assert_eq!(
            kinds(&segs),
            vec![
                (SegKind::System, "/messages/0/content".into(), "s".into()),
                (SegKind::User, "/messages/1/content/0/text".into(), "u".into()),
                (SegKind::Assistant, "/messages/2/refusal".into(), "r".into()),
                (
                    SegKind::Assistant,
                    "/messages/2/tool_calls/0/function/arguments/q/0".into(),
                    "leaf".into()
                ),
                (SegKind::Tool, "/messages/3/content".into(), "t".into()),
                (
                    SegKind::ToolDefinition,
                    "/tools/0/function/description".into(),
                    "f\nd".into()
                ),
            ]
        );
    }

    #[test]
    fn writing_back_a_leaf_keeps_the_arguments_json() {
        let mut body = json!({"choices": [{"message": {"tool_calls": [{"function": {"arguments": "{\"x\":{\"y\":[\"secret\"]}}"}}]}}]});
        let mut segs = output_segments(&body, Dialect::Chat);
        assert_eq!(segs.len(), 1);
        segs[0].text = "[X]".into();
        write_back(&mut body, &segs[0]);
        assert_eq!(
            body["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
            "{\"x\":{\"y\":[\"[X]\"]}}"
        );
    }

    #[test]
    fn anthropic_paths_are_pointers() {
        let body = json!({"system": "sys", "messages": [{"role": "user", "content": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]}]});
        let (segs, _) = input_segments(&body, Dialect::Anthropic, &[]);
        let paths: Vec<String> = segs.iter().map(Segment::path).collect();
        assert_eq!(
            paths,
            [
                "/system",
                "/messages/0/content/0/text",
                "/messages/0/content/1/text"
            ]
        );
    }

    #[test]
    fn marked_objects_are_operator_content() {
        let body = json!({"messages": [{"role": "system", "content": "op", OPERATOR_MARK: true}, {"role": "user", "content": "u"}]});
        let (segs, _) = input_segments(&body, Dialect::Chat, &[]);
        assert_eq!(segs[0].origin, Origin::Operator);
        assert_eq!(segs[1].origin, Origin::Client);
        let mut b = body.clone();
        strip_marks(&mut b);
        assert!(b["messages"][0].get(OPERATOR_MARK).is_none());
    }
}
