//! Stream encoder for Anthropic clients: one open block at a time (tool calls that cannot open yet
//! are held), a block opened only with content, a signature delta before a thinking block stops, and
//! one `message_delta` with the stop reason and final usage.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::{Ev, Frame, StreamCx};
use crate::xlate::ir::Family;
use crate::xlate::resp::{OutReasoning, Stop, Usage, prefixed};
use crate::xlate::warn::Warnings;

#[derive(Clone, Debug, PartialEq)]
enum Open {
    None,
    Thinking { index: u64 },
    Text { index: u64 },
    Tool { index: u64, key: u64 },
}

#[derive(Default)]
struct Held {
    id: String,
    name: String,
    args: String,
    done: bool,
}

pub(super) struct Enc {
    started: bool,
    id: String,
    next: u64,
    open: Open,
    held: BTreeMap<u64, Held>,
    order: Vec<u64>,
    stop: Option<(Stop, Option<String>)>,
    usage: Option<Usage>,
}

fn named(v: Value) -> Frame {
    Frame::Named(v)
}

impl Enc {
    pub(super) fn new(cx: &StreamCx) -> Enc {
        Enc {
            started: false,
            id: prefixed("msg_", Some(&cx.fallback_id)),
            next: 0,
            open: Open::None,
            held: BTreeMap::new(),
            order: Vec::new(),
            stop: None,
            usage: None,
        }
    }

    fn start(&mut self, cx: &StreamCx, out: &mut Vec<Frame>) {
        if self.started {
            return;
        }
        self.started = true;
        let u = self.usage.clone().unwrap_or_default();
        let mut usage = crate::xlate::resp::anthropic_usage_value(&u);
        if let Some(o) = usage.as_object_mut() {
            o.insert("output_tokens".into(), 0.into());
        }
        out.push(named(json!({"type": "message_start", "message": {
            "id": self.id, "type": "message", "role": "assistant", "model": cx.model, "content": [],
            "stop_reason": null, "stop_sequence": null, "usage": usage}})));
    }

    fn open_block(&mut self, cx: &StreamCx, block: Value, out: &mut Vec<Frame>) -> u64 {
        self.start(cx, out);
        let index = self.next;
        self.next += 1;
        out.push(named(
            json!({"type": "content_block_start", "index": index, "content_block": block}),
        ));
        index
    }

    fn delta(index: u64, delta: Value, out: &mut Vec<Frame>) {
        out.push(named(
            json!({"type": "content_block_delta", "index": index, "delta": delta}),
        ));
    }

    fn close(&mut self, out: &mut Vec<Frame>) {
        match std::mem::replace(&mut self.open, Open::None) {
            Open::None => {}
            Open::Thinking { index } => {
                Enc::delta(index, json!({"type": "signature_delta", "signature": ""}), out);
                out.push(named(json!({"type": "content_block_stop", "index": index})));
            }
            Open::Text { index } | Open::Tool { index, .. } => {
                out.push(named(json!({"type": "content_block_stop", "index": index})));
            }
        }
    }

    fn signature(cx: &StreamCx, sig: String, family: Family) -> String {
        let r = OutReasoning {
            text: String::new(),
            signature: Some(sig),
            redacted: None,
            family,
        };
        crate::xlate::resp::anthropic_signature(&r, &cx.provider)
    }

    /// Opens the held calls that are complete, in order of first appearance.
    fn release(&mut self, cx: &StreamCx, all: bool, out: &mut Vec<Frame>) {
        while let Some(&key) = self.order.first() {
            let ready = self.held.get(&key).is_some_and(|h| h.done || all);
            if !ready || self.open != Open::None {
                return;
            }
            self.order.remove(0);
            let Some(h) = self.held.remove(&key) else { continue };
            let index = self.open_block(
                cx,
                json!({"type": "tool_use", "id": h.id, "name": h.name, "input": {}}),
                out,
            );
            if !h.args.is_empty() {
                Enc::delta(
                    index,
                    json!({"type": "input_json_delta", "partial_json": h.args}),
                    out,
                );
            }
            out.push(named(json!({"type": "content_block_stop", "index": index})));
        }
    }

    pub(super) fn push(&mut self, ev: Ev, cx: &StreamCx, _w: &mut Warnings, out: &mut Vec<Frame>) {
        match ev {
            Ev::Meta { id } => {
                if !id.is_empty() && !self.started {
                    self.id = prefixed("msg_", Some(&id));
                }
            }
            Ev::Usage(u) => {
                self.usage = Some(u);
            }
            Ev::Text { choice: 0, text } | Ev::Refusal { text } => {
                if text.is_empty() {
                    return;
                }
                let index = match self.open {
                    Open::Text { index } => index,
                    _ => {
                        self.close(out);
                        let i = self.open_block(cx, json!({"type": "text", "text": ""}), out);
                        self.open = Open::Text { index: i };
                        i
                    }
                };
                Enc::delta(index, json!({"type": "text_delta", "text": text}), out);
            }
            Ev::Text { .. } => {}
            Ev::Think { text, .. } => {
                let index = match self.open {
                    Open::Thinking { index } => index,
                    _ => {
                        self.close(out);
                        let i = self.open_block(
                            cx,
                            json!({"type": "thinking", "thinking": "", "signature": ""}),
                            out,
                        );
                        self.open = Open::Thinking { index: i };
                        i
                    }
                };
                Enc::delta(index, json!({"type": "thinking_delta", "thinking": text}), out);
            }
            Ev::Sig { sig, family } => {
                let index = match self.open {
                    Open::Thinking { index } => index,
                    _ => {
                        self.close(out);
                        self.open_block(
                            cx,
                            json!({"type": "thinking", "thinking": "", "signature": ""}),
                            out,
                        )
                    }
                };
                let sig = Enc::signature(cx, sig, family);
                Enc::delta(index, json!({"type": "signature_delta", "signature": sig}), out);
                out.push(named(json!({"type": "content_block_stop", "index": index})));
                self.open = Open::None;
            }
            Ev::Redacted { data, family } => {
                self.close(out);
                if family == Family::Anthropic {
                    let i = self.open_block(cx, json!({"type": "redacted_thinking", "data": data}), out);
                    out.push(named(json!({"type": "content_block_stop", "index": i})));
                } else {
                    let i = self.open_block(
                        cx,
                        json!({"type": "thinking", "thinking": "", "signature": ""}),
                        out,
                    );
                    let sig = Enc::signature(cx, data, family);
                    Enc::delta(i, json!({"type": "signature_delta", "signature": sig}), out);
                    out.push(named(json!({"type": "content_block_stop", "index": i})));
                }
            }
            Ev::End => {
                if matches!(self.open, Open::Text { .. } | Open::Thinking { .. }) {
                    self.close(out);
                    self.release(cx, false, out);
                }
            }
            Ev::CallStart { key, id, name } => {
                if matches!(self.open, Open::Tool { .. }) || !self.order.is_empty() {
                    self.order.push(key);
                    self.held.insert(
                        key,
                        Held {
                            id,
                            name,
                            args: String::new(),
                            done: false,
                        },
                    );
                    return;
                }
                self.close(out);
                let i = self.open_block(
                    cx,
                    json!({"type": "tool_use", "id": id, "name": name, "input": {}}),
                    out,
                );
                self.open = Open::Tool { index: i, key };
            }
            Ev::CallArgs { key, frag } => match self.open {
                Open::Tool { index, key: k } if k == key => {
                    Enc::delta(
                        index,
                        json!({"type": "input_json_delta", "partial_json": frag}),
                        out,
                    );
                }
                _ => {
                    if let Some(h) = self.held.get_mut(&key) {
                        h.args.push_str(&frag);
                    }
                }
            },
            Ev::CallEnd { key } => {
                if let Open::Tool { key: k, .. } = self.open
                    && k == key
                {
                    self.close(out);
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
                        id,
                        name,
                        args,
                        done: true,
                    },
                );
                if !matches!(self.open, Open::Tool { .. }) {
                    self.close(out);
                }
                self.release(cx, false, out);
            }
            Ev::Stop { choice: 0, stop, seq } => {
                self.stop.get_or_insert((stop, seq));
            }
            Ev::Stop { .. } => {}
        }
    }

    pub(super) fn stop(&self) -> Stop {
        self.stop.as_ref().map_or(Stop::EndTurn, |s| s.0)
    }

    pub(super) fn finish(&mut self, cx: &StreamCx) -> Vec<Frame> {
        let mut out = Vec::new();
        self.start(cx, &mut out);
        self.close(&mut out);
        self.release(cx, true, &mut out);
        let (stop, seq) = self.stop.clone().unwrap_or((Stop::EndTurn, None));
        let seq = if stop == Stop::StopSequence { seq } else { None };
        let usage = crate::xlate::resp::anthropic_usage_value(&self.usage.clone().unwrap_or_default());
        out.push(named(
            json!({"type": "message_delta", "delta": {"stop_reason": stop.anthropic(), "stop_sequence": seq},
                              "usage": usage}),
        ));
        out.push(named(json!({"type": "message_stop"})));
        out
    }
}
