//! Output guardrails on a stream, between the shaper (client-dialect events) and the writer.
//!
//! Text deltas are tracked per segment stream (choice, kind, tool call) and queued with the
//! non-text events in their original order. Window mode releases each stream's text up to its
//! release point with redactions applied, re-chunking deltas as needed; buffer mode releases
//! nothing until the whole output passed the stage; observe and passthrough relay events as they
//! come and check the assembled output at the end.

use std::collections::VecDeque;
use std::sync::Arc;

use antirion_guardrails::runner::{Evaluation, Verdict};
use antirion_guardrails::window::{Redaction, Step, render};
use antirion_guardrails::{Category, GuardStage, SegKind};
use antirion_runtime::Runtime;
use bytes::Bytes;
use serde_json::{Value, json};

use super::encode::ClientDialect;
use super::render::{chat_frame, converse_frame, gemini_frame, named};
use crate::guard::{OutMode, RequestGuard, Trail, record};
use crate::reqlog::Draft;

mod native;
mod text;

/// Why a guarded stream stops.
#[derive(Debug)]
pub(crate) enum Stop {
    /// A guardrail blocked, or failed under `on_error: block`.
    Guardrail(Box<Evaluation>),
    /// Buffer mode exceeded `max_buffer`.
    Overflow,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Key {
    choice: u64,
    kind: SegKind,
    tool: Option<u64>,
}

#[derive(Debug)]
struct Stream {
    key: Key,
    text: String,
    released: usize,
    target: usize,
    redactions: Vec<Redaction>,
    /// The checked text, when the stage changed it (buffer mode).
    replaced: Option<String>,
    replaced_sent: bool,
}

#[derive(Debug)]
struct Template {
    event: Option<String>,
    json: Value,
    pointer: &'static str,
}

#[derive(Debug)]
enum Item {
    Raw(Bytes, Option<(Option<String>, Value)>),
    Text {
        stream: usize,
        end: usize,
        template: Template,
    },
}

/// The output guardrails of one stream.
#[derive(Debug)]
pub(crate) struct OutGuard {
    mode: OutMode,
    client: ClientDialect,
    guard: Arc<RequestGuard>,
    rt: Arc<Runtime>,
    log: Draft,
    attempts: u16,
    streams: Vec<Stream>,
    queue: VecDeque<Item>,
    /// Text was released to the client: the response committed.
    released: bool,
    buffered: u64,
    seq: Option<i64>,
    /// Chat: the first chunk, for the shape of synthesized chunks; choices still open.
    chat_base: Option<Value>,
    open_choices: Vec<u64>,
    /// Anthropic: the content block started and not stopped.
    open_block: Option<u64>,
    /// Responses: the response object of `response.created`.
    response: Option<Value>,
}

fn parse_frame(b: &[u8]) -> Option<(Option<String>, Value)> {
    let text = std::str::from_utf8(b).ok()?;
    let mut event = None;
    let mut data = String::new();
    for line in text.lines() {
        if let Some(e) = line.strip_prefix("event:") {
            event = Some(e.trim().to_owned());
        } else if let Some(d) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(d.strip_prefix(' ').unwrap_or(d));
        }
    }
    let v: Value = serde_json::from_str(&data).ok()?;
    v.is_object().then_some((event, v))
}

fn non_empty(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str).filter(|s| !s.is_empty())
}

impl OutGuard {
    pub(crate) fn new(
        mode: OutMode,
        client: ClientDialect,
        guard: Arc<RequestGuard>,
        rt: Arc<Runtime>,
        log: Draft,
        attempts: u16,
    ) -> OutGuard {
        OutGuard {
            mode,
            client,
            guard,
            rt,
            log,
            attempts,
            streams: Vec::new(),
            queue: VecDeque::new(),
            released: false,
            buffered: 0,
            seq: None,
            chat_base: None,
            open_choices: Vec::new(),
            open_block: None,
            response: None,
        }
    }

    /// The mode in effect.
    pub(crate) fn mode(&self) -> &OutMode {
        &self.mode
    }

    /// `streaming.on_block: finish`.
    pub(crate) fn finishes_on_block(&self) -> bool {
        self.guard.streaming.finish_on_block
    }

    /// Whether events pass through unchanged as they come.
    pub(crate) fn passes(&self) -> bool {
        matches!(self.mode, OutMode::Passthrough | OutMode::Observe)
    }

    /// Text reached the client: the response is committed.
    pub(crate) fn committed(&self) -> bool {
        self.released
    }

    /// The heartbeat interval while buffer mode holds the whole stream.
    pub(crate) fn buffer_heartbeat(&self) -> Option<std::time::Duration> {
        self.guard
            .streaming
            .buffer_heartbeat
            .filter(|_| matches!(self.mode, OutMode::Buffer) && !self.released)
    }

    /// The sequence number the next Responses event gets.
    pub(crate) fn next_sequence(&self) -> Option<i64> {
        self.seq.filter(|_| !self.passes())
    }

    /// The error a stop answers with.
    pub(crate) fn error(&self, stop: &Stop) -> antirion_error::GatewayError {
        match stop {
            Stop::Guardrail(e) => self.guard.error(e, self.attempts),
            Stop::Overflow => crate::guard::buffer_overflow(&self.guard, self.attempts),
        }
    }

    fn stream_index(&mut self, key: Key) -> usize {
        if let Some(i) = self.streams.iter().position(|s| s.key == key) {
            return i;
        }
        self.streams.push(Stream {
            key,
            text: String::new(),
            released: 0,
            target: 0,
            redactions: Vec::new(),
            replaced: None,
            replaced_sent: false,
        });
        self.streams.len() - 1
    }

    fn text_item(&mut self, key: Key, text: &str, template: Template) {
        let i = self.stream_index(key);
        let end = match self.streams.get_mut(i) {
            Some(s) => {
                s.text.push_str(text);
                s.text.len()
            }
            None => return,
        };
        self.queue.push_back(Item::Text {
            stream: i,
            end,
            template,
        });
    }

    /// Splits one client-dialect frame into queued items.
    fn enqueue(&mut self, frame: Bytes) {
        if self.client == ClientDialect::Bedrock {
            self.converse(frame);
            return;
        }
        let Some((event, json)) = parse_frame(&frame) else {
            self.queue.push_back(Item::Raw(frame, None));
            return;
        };
        match self.client {
            ClientDialect::Chat => self.chat(frame, event, json),
            ClientDialect::Anthropic => self.anthropic(frame, event, json),
            ClientDialect::Responses => self.responses(frame, event, json),
            ClientDialect::Gemini => self.gemini(frame, json),
            ClientDialect::Bedrock => self.queue.push_back(Item::Raw(frame, Some((event, json)))),
        }
    }

    /// Takes the frames of one upstream event; the frames that may be sent now.
    pub(crate) fn push(&mut self, frames: Vec<Bytes>) -> Result<Vec<Bytes>, Stop> {
        if self.passes() {
            for f in &frames {
                self.observe(f);
            }
            if frames.iter().any(|f| !f.is_empty()) {
                self.released = true;
            }
            return Ok(frames);
        }
        for f in frames {
            self.buffered = self.buffered.saturating_add(f.len() as u64);
            self.enqueue(f);
        }
        if matches!(self.mode, OutMode::Buffer) {
            if self.buffered > self.guard.streaming.max_buffer {
                self.record_stop(&Stop::Overflow);
                return Err(Stop::Overflow);
            }
            return Ok(Vec::new());
        }
        if let Err(stop) = self.scan(false) {
            self.record_stop(&stop);
            return Err(stop);
        }
        Ok(self.drain())
    }

    /// Passthrough and observe: only the assembled text is kept.
    fn observe(&mut self, frame: &Bytes) {
        let before = self.queue.len();
        if self.client == ClientDialect::Bedrock {
            self.converse(frame.clone());
        } else if let Some((event, json)) = parse_frame(frame) {
            match self.client {
                ClientDialect::Chat => self.chat(Bytes::new(), event, json),
                ClientDialect::Anthropic => self.anthropic(Bytes::new(), event, json),
                ClientDialect::Responses => self.responses(Bytes::new(), event, json),
                ClientDialect::Gemini => self.gemini(Bytes::new(), json),
                ClientDialect::Bedrock => {}
            }
        }
        self.queue.truncate(before);
    }

    /// Window mode: scans every stream and sets its release point.
    fn scan(&mut self, ended: bool) -> Result<(), Stop> {
        let OutMode::Window(w) = &self.mode else {
            return Ok(());
        };
        for s in &mut self.streams {
            match w.step(&s.text, ended) {
                Step::Release(p, r) => {
                    s.target = p.max(s.released);
                    s.redactions = r;
                }
                Step::Block {
                    def,
                    category,
                    detail,
                } => {
                    return Err(Stop::Guardrail(Box::new(Evaluation {
                        def,
                        stage: GuardStage::Output,
                        verdict: Verdict::Block,
                        category: Some(category),
                        detail: Some(detail),
                        duration: std::time::Duration::ZERO,
                        spans: Vec::new(),
                        score: None,
                        confidence: 1.0,
                        digest: None,
                        exposed: Vec::new(),
                    })));
                }
            }
        }
        Ok(())
    }

    fn renumber(&mut self, json: &mut Value) {
        if self.client != ClientDialect::Responses {
            return;
        }
        if let (Some(n), Some(o)) = (self.seq, json.as_object_mut()) {
            o.insert("sequence_number".into(), Value::from(n));
            self.seq = Some(n + 1);
        }
    }

    fn frame_of(&self, event: Option<&str>, json: &Value) -> Bytes {
        match (self.client, event) {
            (ClientDialect::Gemini, _) => gemini_frame(json),
            (ClientDialect::Bedrock, Some(e)) => converse_frame(e, json),
            (ClientDialect::Chat, _) | (_, None) => chat_frame(json),
            _ => named(json),
        }
    }

    /// Replaces the full text of a changed stream wherever a later event repeats it.
    fn rewrite(&self, v: &mut Value) -> bool {
        let mut changed = false;
        match v {
            Value::String(s) => {
                for st in &self.streams {
                    let out = self.final_text(st);
                    if *s == st.text && out != st.text {
                        *s = out;
                        changed = true;
                    }
                }
            }
            Value::Array(a) => {
                for x in a {
                    changed |= self.rewrite(x);
                }
            }
            Value::Object(o) => {
                for x in o.values_mut() {
                    changed |= self.rewrite(x);
                }
            }
            _ => {}
        }
        changed
    }

    fn final_text(&self, s: &Stream) -> String {
        s.replaced
            .clone()
            .unwrap_or_else(|| render(&s.text, &s.redactions, 0, s.text.len()))
    }

    fn raw_out(&mut self, bytes: Bytes, parsed: Option<(Option<String>, Value)>) -> Bytes {
        let Some((event, mut json)) = parsed else {
            return bytes;
        };
        let rewritten = self.rewrite(&mut json);
        if self.client == ClientDialect::Responses && self.seq.is_some() {
            self.renumber(&mut json);
            return self.frame_of(event.as_deref(), &json);
        }
        if rewritten || bytes.is_empty() {
            return self.frame_of(event.as_deref(), &json);
        }
        bytes
    }

    /// Emits queued items in order, as far as every stream's release point allows.
    fn drain(&mut self) -> Vec<Bytes> {
        let mut out = Vec::new();
        while let Some(item) = self.queue.pop_front() {
            match item {
                Item::Raw(bytes, parsed) => {
                    let b = self.raw_out(bytes, parsed);
                    out.push(b);
                }
                Item::Text {
                    stream,
                    end,
                    template,
                } => {
                    let Some(s) = self.streams.get(stream) else {
                        continue;
                    };
                    if let Some(replaced) = &s.replaced {
                        if !s.replaced_sent {
                            let text = replaced.clone();
                            if let Some(st) = self.streams.get_mut(stream) {
                                st.replaced_sent = true;
                                st.released = st.text.len();
                            }
                            out.push(self.text_frame(template, &text));
                        }
                        continue;
                    }
                    if end <= s.released {
                        continue;
                    }
                    let mut cut = end.min(s.target);
                    if let Some(r) = s.redactions.iter().find(|r| r.start < cut && cut < r.end) {
                        cut = if r.end <= s.target { r.end } else { r.start };
                    }
                    if cut > s.released {
                        let text = render(&s.text, &s.redactions, s.released, cut);
                        if let Some(st) = self.streams.get_mut(stream) {
                            st.released = cut;
                        }
                        out.push(self.text_frame(template_clone(&template), &text));
                        self.released = true;
                    }
                    if cut < end {
                        self.queue.push_front(Item::Text {
                            stream,
                            end,
                            template,
                        });
                        break;
                    }
                }
            }
        }
        out
    }

    fn text_frame(&mut self, mut template: Template, text: &str) -> Bytes {
        if let Some(slot) = template.json.pointer_mut(template.pointer) {
            *slot = Value::from(text);
        }
        self.renumber(&mut template.json);
        self.frame_of(template.event.as_deref(), &template.json)
    }

    /// The assembled output as a client-dialect body, with the pointer of each stream's text.
    fn assembled(&self) -> (Value, Vec<String>) {
        if let Some(native) = self.assembled_native() {
            return native;
        }
        let mut pointers = Vec::new();
        let body = match self.client {
            ClientDialect::Anthropic | ClientDialect::Responses => {
                let mut items = Vec::new();
                for (i, s) in self.streams.iter().enumerate() {
                    let (item, ptr) = match (self.client, s.key.kind) {
                        (ClientDialect::Anthropic, SegKind::Reasoning) => (
                            json!({"type": "thinking", "thinking": s.text}),
                            format!("/content/{i}/thinking"),
                        ),
                        (ClientDialect::Anthropic, _) => (
                            json!({"type": "text", "text": s.text}),
                            format!("/content/{i}/text"),
                        ),
                        (_, SegKind::Reasoning) => (
                            json!({"type": "reasoning", "summary": [{"type": "summary_text", "text": s.text}]}),
                            format!("/output/{i}/summary/0/text"),
                        ),
                        (_, SegKind::ToolArguments) => (
                            json!({"type": "function_call", "arguments": s.text}),
                            format!("/output/{i}/arguments"),
                        ),
                        (_, SegKind::Refusal) => (
                            json!({"type": "message", "content": [{"type": "refusal", "refusal": s.text}]}),
                            format!("/output/{i}/content/0/refusal"),
                        ),
                        _ => (
                            json!({"type": "message", "content": [{"type": "output_text", "text": s.text}]}),
                            format!("/output/{i}/content/0/text"),
                        ),
                    };
                    items.push(item);
                    pointers.push(ptr);
                }
                if self.client == ClientDialect::Anthropic {
                    json!({"content": items})
                } else {
                    json!({"output": items})
                }
            }
            _ => {
                let mut choices = Vec::new();
                for (i, s) in self.streams.iter().enumerate() {
                    let message = match s.key.kind {
                        SegKind::Reasoning => json!({"reasoning_content": s.text}),
                        SegKind::Refusal => json!({"refusal": s.text}),
                        SegKind::ToolArguments => {
                            json!({"tool_calls": [{"function": {"arguments": s.text}}]})
                        }
                        _ => json!({"content": s.text}),
                    };
                    let field = match s.key.kind {
                        SegKind::Reasoning => "reasoning_content".to_owned(),
                        SegKind::Refusal => "refusal".to_owned(),
                        SegKind::ToolArguments => "tool_calls/0/function/arguments".to_owned(),
                        _ => "content".to_owned(),
                    };
                    choices.push(json!({"index": s.key.choice, "message": message}));
                    pointers.push(format!("/choices/{i}/message/{field}"));
                }
                json!({"choices": choices})
            }
        };
        (body, pointers)
    }

    /// The end of the upstream stream: `tail` are the final frames (terminator included). Runs the
    /// whole output stage once on the assembled output, then releases the rest. `trail` receives
    /// the verdicts when the response head is not sent yet (buffer mode).
    pub(crate) async fn finish(
        &mut self,
        tail: Vec<Bytes>,
        trail: Option<&Trail>,
    ) -> Result<Vec<Bytes>, Stop> {
        let observe = matches!(self.mode, OutMode::Observe);
        if self.passes() {
            for f in &tail {
                self.observe(f);
            }
            let (mut body, _) = self.assembled();
            let out = self.guard.run_output(&mut body).await;
            record(&self.rt, &self.log, trail, &out.evals, observe);
            return Ok(tail);
        }
        for f in tail {
            self.buffered = self.buffered.saturating_add(f.len() as u64);
            self.enqueue(f);
        }
        if matches!(self.mode, OutMode::Buffer) && self.buffered > self.guard.streaming.max_buffer {
            self.record_stop(&Stop::Overflow);
            return Err(Stop::Overflow);
        }
        if let Err(stop) = self.scan(true) {
            self.record_stop(&stop);
            return Err(stop);
        }
        let (mut body, pointers) = self.assembled();
        let out = self.guard.run_output(&mut body).await;
        let observing = self.guard.observing();
        record(&self.rt, &self.log, trail, &out.evals, observing);
        if let Some(e) = out.failed.and_then(|i| out.evals.get(i)).filter(|_| !observing) {
            if let (Some(t), antirion_guardrails::Check::JsonSchema(j)) = (trail, &e.def.check)
                && j.reask > 0
                && e.verdict == Verdict::Block
            {
                t.set_reask(j.reask);
            }
            return Err(Stop::Guardrail(Box::new(e.clone())));
        }
        if matches!(self.mode, OutMode::Buffer) {
            let tokens = self.guard.restoring();
            for (s, p) in self.streams.iter_mut().zip(&pointers) {
                let checked = body
                    .pointer(p)
                    .and_then(Value::as_str)
                    .filter(|_| out.changed)
                    .unwrap_or(&s.text);
                let restored = tokens.as_ref().and_then(|t| t.restore(checked));
                let text = restored.as_deref().unwrap_or(checked);
                if *text != s.text {
                    s.replaced = Some(text.to_owned());
                }
            }
        }
        for s in &mut self.streams {
            s.target = s.text.len();
        }
        Ok(self.drain())
    }

    /// Records a mid-stream stop: the blocking guardrail and every other output guardrail once.
    pub(crate) fn record_stop(&self, stop: &Stop) {
        let blocking = match stop {
            Stop::Guardrail(e) => Some((**e).clone()),
            Stop::Overflow => None,
        };
        let evals: Vec<Evaluation> = self
            .guard
            .attached
            .output
            .iter()
            .map(|d| match &blocking {
                Some(b) if b.def.name == d.name => b.clone(),
                _ => Evaluation {
                    def: Arc::clone(d),
                    stage: GuardStage::Output,
                    verdict: if blocking.is_none() && d.enforcing() {
                        Verdict::Error
                    } else {
                        Verdict::Skipped
                    },
                    category: blocking.is_none().then_some(Category::Error),
                    detail: None,
                    duration: std::time::Duration::ZERO,
                    spans: Vec::new(),
                    score: None,
                    confidence: 1.0,
                    digest: None,
                    exposed: Vec::new(),
                },
            })
            .collect();
        record(&self.rt, &self.log, None, &evals, false);
    }

    /// The content-filter ending of a stream blocked after commit (`on_block: finish`), before the
    /// usage chunk and the terminator the relay adds for Chat.
    pub(crate) fn finish_frames(&mut self) -> Vec<Bytes> {
        let mut out = Vec::new();
        match self.client {
            ClientDialect::Chat => {
                let base = self.chat_base.clone().unwrap_or_else(|| json!({}));
                for c in self.open_choices.clone() {
                    let mut chunk = base.clone();
                    if let Some(o) = chunk.as_object_mut() {
                        o.insert(
                            "choices".into(),
                            json!([{"index": c, "delta": {}, "finish_reason": "content_filter"}]),
                        );
                        o.remove("usage");
                    }
                    out.push(chat_frame(&chunk));
                }
            }
            ClientDialect::Anthropic => {
                if let Some(i) = self.open_block {
                    out.push(named(&json!({"type": "content_block_stop", "index": i})));
                }
                out.push(named(&json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": "refusal", "stop_sequence": null},
                    "usage": {"output_tokens": 0},
                })));
                out.push(named(&json!({"type": "message_stop"})));
            }
            ClientDialect::Responses => {
                let mut response = self
                    .response
                    .clone()
                    .unwrap_or_else(|| json!({"object": "response"}));
                if let Some(o) = response.as_object_mut() {
                    o.insert("status".into(), Value::from("incomplete"));
                    o.insert("incomplete_details".into(), json!({"reason": "content_filter"}));
                }
                let mut ev = json!({"type": "response.incomplete", "response": response});
                self.renumber(&mut ev);
                out.push(named(&ev));
            }
            _ => {}
        }
        out
    }
}

fn template_clone(t: &Template) -> Template {
    Template {
        event: t.event.clone(),
        json: t.json.clone(),
        pointer: t.pointer,
    }
}
