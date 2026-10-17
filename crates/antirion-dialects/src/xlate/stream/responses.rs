//! Stream encoder for Responses clients: `response.created`, output items one at a time with
//! contiguous `output_index` and `content_index`, contiguous `sequence_number`, and a terminal
//! `response.completed` or `response.incomplete` carrying the whole response.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use super::{Ev, Frame, StreamCx};
use crate::xlate::ir::Family;
use crate::xlate::resp::{OutReasoning, Stop, Usage, prefixed};

enum Open {
    None,
    Reasoning {
        oi: u64,
        id: String,
        text: String,
        family: Family,
    },
    Message {
        oi: u64,
        id: String,
        parts: Vec<Value>,
        part: Option<(u64, String, bool)>,
    },
    Call {
        oi: u64,
        id: String,
        key: u64,
        call_id: String,
        name: String,
        args: String,
        /// A custom tool's call: its input is sent whole when the call ends.
        custom: bool,
    },
}

#[derive(Default)]
struct Held {
    call_id: String,
    name: String,
    args: String,
    done: bool,
}

pub(super) struct Enc {
    seq: i64,
    started: bool,
    id: String,
    output: Vec<Value>,
    open: Open,
    held: BTreeMap<u64, Held>,
    order: Vec<u64>,
    stop: Option<Stop>,
    usage: Option<Usage>,
    completed: Option<Value>,
}

impl Enc {
    pub(super) fn new(cx: &StreamCx) -> Enc {
        Enc {
            seq: 0,
            started: false,
            id: cx
                .response_id
                .clone()
                .unwrap_or_else(|| prefixed("resp_", Some(&cx.fallback_id))),
            output: Vec::new(),
            open: Open::None,
            held: BTreeMap::new(),
            order: Vec::new(),
            stop: None,
            usage: None,
            completed: None,
        }
    }

    fn emit(&mut self, mut v: Value, out: &mut Vec<Frame>) {
        if let Some(o) = v.as_object_mut() {
            o.insert("sequence_number".into(), self.seq.into());
        }
        self.seq += 1;
        out.push(Frame::Named(v));
    }

    fn response(
        &self,
        cx: &StreamCx,
        status: &str,
        details: Value,
        output: Vec<Value>,
        usage: Value,
    ) -> Value {
        let mut r = Map::new();
        r.insert("id".into(), self.id.as_str().into());
        r.insert("object".into(), "response".into());
        r.insert("created_at".into(), cx.created.into());
        r.insert("status".into(), status.into());
        r.insert("incomplete_details".into(), details);
        r.insert("error".into(), Value::Null);
        r.insert("model".into(), cx.model.as_str().into());
        r.insert("output".into(), Value::Array(output));
        r.insert("usage".into(), usage);
        crate::xlate::resp::responses_echo(&mut r, &cx.request);
        Value::Object(r)
    }

    fn start(&mut self, cx: &StreamCx, out: &mut Vec<Frame>) {
        if self.started {
            return;
        }
        self.started = true;
        let r = self.response(cx, "in_progress", Value::Null, Vec::new(), Value::Null);
        self.emit(json!({"type": "response.created", "response": r}), out);
        self.emit(json!({"type": "response.in_progress", "response": r}), out);
    }

    fn next_index(&self) -> u64 {
        self.output.len() as u64
    }

    fn add_item(&mut self, cx: &StreamCx, item: Value, out: &mut Vec<Frame>) -> u64 {
        self.start(cx, out);
        let oi = self.next_index();
        self.output.push(Value::Null);
        self.emit(
            json!({"type": "response.output_item.added", "output_index": oi, "item": item}),
            out,
        );
        oi
    }

    fn done_item(&mut self, oi: u64, item: Value, out: &mut Vec<Frame>) {
        if let Some(slot) = usize::try_from(oi).ok().and_then(|i| self.output.get_mut(i)) {
            *slot = item.clone();
        }
        self.emit(
            json!({"type": "response.output_item.done", "output_index": oi, "item": item}),
            out,
        );
    }

    fn close_part(&mut self, out: &mut Vec<Frame>) {
        let mut done = Vec::new();
        if let Open::Message { oi, id, parts, part } = &mut self.open
            && let Some((ci, text, refusal)) = part.take()
        {
            let p = if refusal {
                json!({"type": "refusal", "refusal": text})
            } else {
                json!({"type": "output_text", "text": text, "annotations": []})
            };
            if !refusal {
                done.push(
                    json!({"type": "response.output_text.done", "item_id": id, "output_index": oi,
                                 "content_index": ci, "text": text}),
                );
            }
            done.push(
                json!({"type": "response.content_part.done", "item_id": id, "output_index": oi,
                             "content_index": ci, "part": p}),
            );
            parts.push(p);
        }
        for v in done {
            self.emit(v, out);
        }
    }

    fn close(&mut self, cx: &StreamCx, sig: Option<(String, Family)>, out: &mut Vec<Frame>) {
        self.close_part(out);
        match std::mem::replace(&mut self.open, Open::None) {
            Open::None => {}
            Open::Reasoning { oi, id, text, family } => {
                if !text.is_empty() {
                    let part = json!({"type": "summary_text", "text": text});
                    self.emit(json!({"type": "response.reasoning_summary_text.done", "item_id": id, "output_index": oi,
                                     "summary_index": 0, "text": text}), out);
                    self.emit(json!({"type": "response.reasoning_summary_part.done", "item_id": id, "output_index": oi,
                                     "summary_index": 0, "part": part}), out);
                }
                let (signature, family) = match sig {
                    Some((s, f)) => (Some(s), f),
                    None => (None, family),
                };
                let r = OutReasoning {
                    text,
                    signature,
                    redacted: None,
                    family,
                };
                let item = crate::xlate::resp::responses_reasoning_item(&id, &r, &resp_cx(cx));
                self.done_item(oi, item, out);
            }
            Open::Message { oi, id, parts, .. } => {
                let item = json!({"type": "message", "id": id, "status": "completed", "role": "assistant", "content": parts});
                self.done_item(oi, item, out);
            }
            Open::Call {
                oi,
                id,
                call_id,
                name,
                args,
                custom: true,
                ..
            } => {
                let input = crate::xlate::resp::responses_custom_input(&args);
                self.emit(
                    json!({"type": "response.custom_tool_call_input.delta", "item_id": id, "output_index": oi,
                           "delta": input}),
                    out,
                );
                self.emit(
                    json!({"type": "response.custom_tool_call_input.done", "item_id": id, "output_index": oi,
                           "input": input}),
                    out,
                );
                let item = crate::xlate::resp::responses_custom_call(&id, &call_id, &name, &input);
                self.done_item(oi, item, out);
            }
            Open::Call {
                oi,
                id,
                call_id,
                name,
                args,
                ..
            } => {
                self.emit(
                    json!({"type": "response.function_call_arguments.done", "item_id": id, "output_index": oi,
                                 "arguments": args}),
                    out,
                );
                let item = crate::xlate::resp::responses_function_call(&id, &call_id, &name, &args);
                self.done_item(oi, item, out);
            }
        }
    }

    fn open_call(&mut self, cx: &StreamCx, key: u64, call_id: String, name: String, out: &mut Vec<Frame>) {
        let custom = cx.restore.custom.contains(&name);
        let (id, item) = if custom {
            let id = format!("ctc_{}", self.next_index());
            let item = json!({"type": "custom_tool_call", "id": id, "call_id": call_id, "name": name, "input": "",
                              "status": "in_progress"});
            (id, item)
        } else {
            let id = format!("fc_{}", self.next_index());
            let item = json!({"type": "function_call", "id": id, "call_id": call_id, "name": name, "arguments": "",
                              "status": "in_progress"});
            (id, item)
        };
        let oi = self.add_item(cx, item, out);
        self.open = Open::Call {
            oi,
            id,
            key,
            call_id,
            name,
            args: String::new(),
            custom,
        };
    }

    fn args(&mut self, frag: &str, out: &mut Vec<Frame>) {
        let ev = if let Open::Call {
            oi, id, args, custom, ..
        } = &mut self.open
        {
            args.push_str(frag);
            (!*custom).then(||
                json!({"type": "response.function_call_arguments.delta", "item_id": id, "output_index": oi, "delta": frag}))
        } else {
            None
        };
        if let Some(ev) = ev {
            self.emit(ev, out);
        }
    }

    fn release(&mut self, cx: &StreamCx, all: bool, out: &mut Vec<Frame>) {
        while let Some(&key) = self.order.first() {
            let ready = self.held.get(&key).is_some_and(|h| h.done || all);
            if !ready || !matches!(self.open, Open::None) {
                return;
            }
            self.order.remove(0);
            let Some(h) = self.held.remove(&key) else { continue };
            self.open_call(cx, key, h.call_id, h.name, out);
            if !h.args.is_empty() {
                self.args(&h.args, out);
            }
            self.close(cx, None, out);
        }
    }

    pub(super) fn push(&mut self, ev: Ev, cx: &StreamCx, out: &mut Vec<Frame>) {
        match ev {
            Ev::Meta { .. } | Ev::Text { choice: 1.., .. } => {}
            Ev::Usage(u) => self.usage = Some(u),
            Ev::Stop { choice: 0, stop, .. } => {
                self.stop.get_or_insert(stop);
            }
            Ev::Stop { .. } => {}
            Ev::Think { text, family } => {
                if !matches!(self.open, Open::Reasoning { .. }) {
                    self.close(cx, None, out);
                    let id = format!("rs_{}", self.next_index());
                    let oi = self.add_item(cx, json!({"type": "reasoning", "id": id, "summary": []}), out);
                    self.emit(json!({"type": "response.reasoning_summary_part.added", "item_id": id, "output_index": oi,
                                     "summary_index": 0, "part": {"type": "summary_text", "text": ""}}), out);
                    self.open = Open::Reasoning {
                        oi,
                        id,
                        text: String::new(),
                        family,
                    };
                }
                let ev = if let Open::Reasoning {
                    oi, id, text: acc, ..
                } = &mut self.open
                {
                    acc.push_str(&text);
                    Some(
                        json!({"type": "response.reasoning_summary_text.delta", "item_id": id, "output_index": oi,
                                "summary_index": 0, "delta": text}),
                    )
                } else {
                    None
                };
                if let Some(ev) = ev {
                    self.emit(ev, out);
                }
            }
            Ev::Sig { sig, family } | Ev::Redacted { data: sig, family } => {
                if !matches!(self.open, Open::Reasoning { .. }) {
                    self.close(cx, None, out);
                    let id = format!("rs_{}", self.next_index());
                    let oi = self.add_item(cx, json!({"type": "reasoning", "id": id, "summary": []}), out);
                    self.open = Open::Reasoning {
                        oi,
                        id,
                        text: String::new(),
                        family,
                    };
                }
                self.close(cx, Some((sig, family)), out);
            }
            Ev::Text { text, .. } | Ev::Refusal { text } => {
                if text.is_empty() {
                    return;
                }
                let refusal = matches!(self.stop, Some(Stop::Refusal));
                if !matches!(self.open, Open::Message { .. }) {
                    self.close(cx, None, out);
                    let id = format!("msg_{}", self.next_index());
                    let oi = self.add_item(
                        cx,
                        json!({"type": "message", "id": id, "status": "in_progress", "role": "assistant", "content": []}),
                        out,
                    );
                    self.open = Open::Message {
                        oi,
                        id,
                        parts: Vec::new(),
                        part: None,
                    };
                }
                let mut evs = Vec::new();
                if let Open::Message { oi, id, parts, part } = &mut self.open {
                    if part.is_none() {
                        let ci = parts.len() as u64;
                        evs.push(json!({"type": "response.content_part.added", "item_id": id, "output_index": oi,
                                        "content_index": ci, "part": {"type": "output_text", "text": "", "annotations": []}}));
                        *part = Some((ci, String::new(), refusal));
                    }
                    if let Some((ci, acc, _)) = part {
                        acc.push_str(&text);
                        evs.push(
                            json!({"type": "response.output_text.delta", "item_id": id, "output_index": oi,
                                        "content_index": ci, "delta": text}),
                        );
                    }
                }
                for v in evs {
                    self.emit(v, out);
                }
            }
            Ev::End => match self.open {
                Open::Message { .. } => self.close_part(out),
                Open::Reasoning { .. } => {
                    self.close(cx, None, out);
                    self.release(cx, false, out);
                }
                _ => {}
            },
            Ev::CallStart { key, id, name } => {
                if matches!(self.open, Open::Call { .. }) || !self.order.is_empty() {
                    self.order.push(key);
                    self.held.insert(
                        key,
                        Held {
                            call_id: id,
                            name,
                            args: String::new(),
                            done: false,
                        },
                    );
                    return;
                }
                self.close(cx, None, out);
                self.open_call(cx, key, id, name, out);
            }
            Ev::CallArgs { key, frag } => match &self.open {
                Open::Call { key: k, .. } if *k == key => self.args(&frag, out),
                _ => {
                    if let Some(h) = self.held.get_mut(&key) {
                        h.args.push_str(&frag);
                    }
                }
            },
            Ev::CallEnd { key } => {
                if matches!(&self.open, Open::Call { key: k, .. } if *k == key) {
                    self.close(cx, None, out);
                } else if let Some(h) = self.held.get_mut(&key) {
                    h.done = true;
                }
                self.release(cx, false, out);
            }
            Ev::Call {
                key, id, name, args, ..
            } => {
                self.order.push(key);
                self.held.insert(
                    key,
                    Held {
                        call_id: id,
                        name,
                        args,
                        done: true,
                    },
                );
                if !matches!(self.open, Open::Call { .. }) {
                    self.close(cx, None, out);
                }
                self.release(cx, false, out);
            }
        }
    }

    pub(super) fn stop(&self) -> Stop {
        self.stop.unwrap_or(Stop::EndTurn)
    }

    pub(super) fn finish(&mut self, cx: &StreamCx) -> Vec<Frame> {
        let mut out = Vec::new();
        self.start(cx, &mut out);
        self.close(cx, None, &mut out);
        self.release(cx, true, &mut out);
        let (status, details) = crate::xlate::resp::responses_status(self.stop());
        let usage = self
            .usage
            .as_ref()
            .map_or(Value::Null, crate::xlate::resp::responses_usage_value);
        let output: Vec<Value> = self.output.iter().filter(|v| !v.is_null()).cloned().collect();
        let r = self.response(cx, status, details, output, usage);
        self.completed = Some(r.clone());
        let kind = if status == "incomplete" {
            "response.incomplete"
        } else {
            "response.completed"
        };
        self.emit(json!({"type": kind, "response": r}), &mut out);
        out
    }

    pub(super) fn completed(&self) -> Option<&Value> {
        self.completed.as_ref()
    }

    pub(super) fn next_sequence(&self) -> i64 {
        self.seq
    }
}

/// A response context over the stream's owned facts.
fn resp_cx(cx: &StreamCx) -> crate::xlate::resp::Cx<'_> {
    crate::xlate::resp::Cx {
        upstream: cx.upstream,
        client: cx.client,
        restore: &cx.restore,
        provider: &cx.provider,
        model: &cx.model,
        created: cx.created,
        fallback_id: &cx.fallback_id,
        request: &cx.request,
    }
}
