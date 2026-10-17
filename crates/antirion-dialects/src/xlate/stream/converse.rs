//! Stream encoder for ConverseStream clients: `messageStart`, content blocks one at a time (a tool
//! use is sent once its input is complete), then `messageStop` and `metadata` with the usage.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use super::{Ev, Frame, StreamCx};
use crate::xlate::envelope;
use crate::xlate::ir::Family;
use crate::xlate::resp::{
    OutReasoning, Stop, Usage, converse_reasoning, converse_stop, converse_usage_value,
};
use crate::xlate::util::parse_args;
use crate::xlate::warn::{Kind, Warnings};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Open {
    None,
    Text(u64),
    Reasoning(u64),
}

#[derive(Default)]
struct Held {
    id: String,
    name: String,
    args: String,
}

pub(super) struct Enc {
    started: bool,
    next: u64,
    open: Open,
    held: BTreeMap<u64, Held>,
    stop: Option<(Stop, Option<String>)>,
    usage: Option<Usage>,
}

fn event(kind: &'static str, payload: Value) -> Frame {
    Frame::Event(kind, payload)
}

fn delta(index: u64, d: Value) -> Frame {
    event(
        "contentBlockDelta",
        json!({"contentBlockIndex": index, "delta": d}),
    )
}

fn stop_block(index: u64) -> Frame {
    event("contentBlockStop", json!({"contentBlockIndex": index}))
}

impl Enc {
    pub(super) fn new() -> Enc {
        Enc {
            started: false,
            next: 0,
            open: Open::None,
            held: BTreeMap::new(),
            stop: None,
            usage: None,
        }
    }

    fn start(&mut self, out: &mut Vec<Frame>) {
        if !self.started {
            self.started = true;
            out.push(event("messageStart", json!({"role": "assistant"})));
        }
    }

    fn take_index(&mut self) -> u64 {
        let i = self.next;
        self.next += 1;
        i
    }

    fn close(&mut self, out: &mut Vec<Frame>) {
        match std::mem::replace(&mut self.open, Open::None) {
            Open::None => {}
            Open::Text(i) | Open::Reasoning(i) => out.push(stop_block(i)),
        }
    }

    fn reasoning_index(&mut self, out: &mut Vec<Frame>) -> u64 {
        self.start(out);
        if let Open::Reasoning(i) = self.open {
            return i;
        }
        self.close(out);
        let i = self.take_index();
        self.open = Open::Reasoning(i);
        i
    }

    fn tool(&mut self, h: Held, w: &mut Warnings, out: &mut Vec<Frame>) {
        self.start(out);
        self.close(out);
        let i = self.take_index();
        out.push(event(
            "contentBlockStart",
            json!({"contentBlockIndex": i, "start": {"toolUse": {"toolUseId": h.id, "name": h.name}}}),
        ));
        let input = if h.args.trim().is_empty() {
            "{}".to_owned()
        } else if parse_args(&h.args).is_some() {
            h.args
        } else {
            w.response(Kind::Approximated, "tool_calls[].arguments");
            "{}".to_owned()
        };
        out.push(delta(i, json!({"toolUse": {"input": input}})));
        out.push(stop_block(i));
    }

    pub(super) fn push(&mut self, ev: Ev, cx: &StreamCx, w: &mut Warnings, out: &mut Vec<Frame>) {
        match ev {
            Ev::Meta { .. } => {}
            Ev::Text { choice: 0, text } | Ev::Refusal { text } => {
                if text.is_empty() {
                    return;
                }
                self.start(out);
                let i = match self.open {
                    Open::Text(i) => i,
                    _ => {
                        self.close(out);
                        let i = self.take_index();
                        self.open = Open::Text(i);
                        i
                    }
                };
                out.push(delta(i, json!({"text": text})));
            }
            Ev::Text { .. } => {}
            Ev::Think { text, .. } => {
                let i = self.reasoning_index(out);
                out.push(delta(i, json!({"reasoningContent": {"text": text}})));
            }
            Ev::Sig { sig, family } => {
                let i = self.reasoning_index(out);
                let r = OutReasoning {
                    text: String::new(),
                    signature: Some(sig),
                    redacted: None,
                    family,
                };
                let content = converse_reasoning(&r, &cx.provider);
                if let Some(s) = content.pointer("/reasoningText/signature") {
                    out.push(delta(i, json!({"reasoningContent": {"signature": s}})));
                }
                self.close(out);
            }
            Ev::Redacted { data, family } => {
                let content = match family {
                    Family::Anthropic => json!({"redactedContent": data}),
                    Family::None => return,
                    f => {
                        let provider = (f == Family::OpenAi).then_some(cx.provider.as_str());
                        json!({"signature": envelope::wrap(f, provider, &data)})
                    }
                };
                self.start(out);
                self.close(out);
                let i = self.take_index();
                out.push(delta(i, json!({"reasoningContent": content})));
                out.push(stop_block(i));
            }
            Ev::End => self.close(out),
            Ev::CallStart { key, id, name } => {
                self.held.insert(
                    key,
                    Held {
                        id,
                        name,
                        args: String::new(),
                    },
                );
            }
            Ev::CallArgs { key, frag } => {
                if let Some(h) = self.held.get_mut(&key) {
                    h.args.push_str(&frag);
                }
            }
            Ev::CallEnd { key } => {
                if let Some(h) = self.held.remove(&key) {
                    self.tool(h, w, out);
                }
            }
            Ev::Call { id, name, args, .. } => self.tool(Held { id, name, args }, w, out),
            Ev::Stop { choice: 0, stop, seq } => {
                self.stop.get_or_insert((stop, seq));
            }
            Ev::Stop { .. } => {}
            Ev::Usage(u) => self.usage = Some(u),
        }
    }

    pub(super) fn stop(&self) -> Stop {
        self.stop.as_ref().map_or(Stop::EndTurn, |s| s.0)
    }

    pub(super) fn finish(&mut self, w: &mut Warnings) -> Vec<Frame> {
        let mut out = Vec::new();
        self.start(&mut out);
        self.close(&mut out);
        for (_, h) in std::mem::take(&mut self.held) {
            self.tool(h, w, &mut out);
        }
        let (stop, seq) = self.stop.clone().unwrap_or((Stop::EndTurn, None));
        let mut m = Map::new();
        m.insert("stopReason".into(), converse_stop(stop).into());
        if let Some(s) = seq.filter(|_| stop == Stop::StopSequence) {
            m.insert(
                "additionalModelResponseFields".into(),
                json!({"stop_sequence": s}),
            );
        }
        out.push(event("messageStop", Value::Object(m)));
        let usage = converse_usage_value(&self.usage.clone().unwrap_or_default());
        out.push(event(
            "metadata",
            json!({"usage": usage, "metrics": {"latencyMs": 0}}),
        ));
        out
    }
}
