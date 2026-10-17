//! Stream encoder for Chat clients: chunks with the role first, tool calls numbered in order of first
//! appearance, one finish chunk per choice, the usage chunk when asked, then `[DONE]`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::{Map, Value, json};

use super::{Ev, Frame, StreamCx};
use crate::xlate::ir::Family;
use crate::xlate::resp::{OutReasoning, Stop, Usage, prefixed};

pub(super) struct Enc {
    id: String,
    roles: BTreeSet<u32>,
    calls: HashMap<u64, u64>,
    thinking: String,
    family: Family,
    stops: BTreeMap<u32, Stop>,
    usage: Option<Usage>,
}

impl Enc {
    pub(super) fn new(cx: &StreamCx) -> Enc {
        Enc {
            id: prefixed("chatcmpl-", Some(&cx.fallback_id)),
            roles: BTreeSet::new(),
            calls: HashMap::new(),
            thinking: String::new(),
            family: Family::None,
            stops: BTreeMap::new(),
            usage: None,
        }
    }

    fn chunk(&self, cx: &StreamCx, choice: u32, delta: Value, finish: Option<&str>) -> Frame {
        Frame::Data(json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": cx.created,
            "model": cx.model,
            "choices": [{"index": choice, "delta": delta, "finish_reason": finish}],
        }))
    }

    fn delta(&mut self, cx: &StreamCx, choice: u32, mut delta: Map<String, Value>, out: &mut Vec<Frame>) {
        if self.roles.insert(choice) {
            delta.insert("role".into(), "assistant".into());
            if !delta.contains_key("content") {
                delta.insert("content".into(), "".into());
            }
        }
        out.push(self.chunk(cx, choice, Value::Object(delta), None));
    }

    fn one(&mut self, cx: &StreamCx, key: &str, value: Value, out: &mut Vec<Frame>) {
        let mut d = Map::new();
        d.insert(key.into(), value);
        self.delta(cx, 0, d, out);
    }

    /// Ends a reasoning segment that had text but no signature.
    fn flush_thinking(&mut self, cx: &StreamCx, out: &mut Vec<Frame>) {
        if self.thinking.is_empty() {
            return;
        }
        let r = OutReasoning {
            text: std::mem::take(&mut self.thinking),
            signature: None,
            redacted: None,
            family: self.family,
        };
        self.one(
            cx,
            "reasoning_details",
            json!([crate::xlate::resp::chat_detail(&r)]),
            out,
        );
    }

    fn index(&mut self, key: u64) -> u64 {
        let next = self.calls.len() as u64;
        *self.calls.entry(key).or_insert(next)
    }

    pub(super) fn push(&mut self, ev: Ev, cx: &StreamCx, out: &mut Vec<Frame>) {
        match ev {
            Ev::Meta { id } => {
                if !id.is_empty() {
                    self.id = prefixed("chatcmpl-", Some(&id));
                }
            }
            Ev::Text { choice, text } => {
                let mut d = Map::new();
                d.insert("content".into(), text.into());
                self.delta(cx, choice, d, out);
            }
            Ev::Refusal { text } => self.one(cx, "refusal", text.into(), out),
            Ev::Think { text, family } => {
                self.family = family;
                self.thinking.push_str(&text);
                self.one(cx, "reasoning_content", text.into(), out);
            }
            Ev::Sig { sig, family } => {
                let r = OutReasoning {
                    text: std::mem::take(&mut self.thinking),
                    signature: Some(sig),
                    redacted: None,
                    family,
                };
                self.one(
                    cx,
                    "reasoning_details",
                    json!([crate::xlate::resp::chat_detail(&r)]),
                    out,
                );
            }
            Ev::Redacted { data, family } => {
                self.flush_thinking(cx, out);
                self.one(
                    cx,
                    "reasoning_details",
                    json!([{"type": "reasoning.encrypted", "data": data, "format": family.as_str()}]),
                    out,
                );
            }
            Ev::End => self.flush_thinking(cx, out),
            Ev::CallStart { key, id, name } => {
                self.flush_thinking(cx, out);
                let k = self.index(key);
                self.one(
                    cx,
                    "tool_calls",
                    json!([{"index": k, "id": id, "type": "function", "function": {"name": name, "arguments": ""}}]),
                    out,
                );
            }
            Ev::CallArgs { key, frag } => {
                let k = self.index(key);
                self.one(
                    cx,
                    "tool_calls",
                    json!([{"index": k, "function": {"arguments": frag}}]),
                    out,
                );
            }
            Ev::Call {
                key,
                id,
                name,
                args,
                sig,
            } => {
                self.flush_thinking(cx, out);
                let k = self.index(key);
                let mut tc = crate::xlate::resp::chat_tool_call(&id, &name, &args, sig.as_deref());
                if let Some(o) = tc.as_object_mut() {
                    o.shift_insert(0, "index".into(), k.into());
                }
                self.one(cx, "tool_calls", Value::Array(vec![tc]), out);
            }
            Ev::CallEnd { .. } => {}
            Ev::Stop { choice, stop, .. } => {
                self.stops.entry(choice).or_insert(stop);
            }
            Ev::Usage(u) => self.usage = Some(u),
        }
    }

    pub(super) fn stop(&self) -> Stop {
        self.stops.get(&0).copied().unwrap_or(Stop::EndTurn)
    }

    pub(super) fn finish(&mut self, cx: &StreamCx) -> Vec<Frame> {
        let mut out = Vec::new();
        self.flush_thinking(cx, &mut out);
        let mut choices: BTreeSet<u32> = self.roles.clone();
        choices.extend(self.stops.keys());
        choices.insert(0);
        for c in choices {
            if !self.roles.contains(&c) {
                self.delta(cx, c, Map::new(), &mut out);
            }
            let stop = self.stops.get(&c).copied().unwrap_or(Stop::EndTurn);
            out.push(self.chunk(cx, c, json!({}), Some(stop.chat())));
        }
        if cx.include_usage {
            let usage = self.usage.clone().unwrap_or_default();
            out.push(Frame::Data(json!({
                "id": self.id,
                "object": "chat.completion.chunk",
                "created": cx.created,
                "model": cx.model,
                "choices": [],
                "usage": crate::xlate::resp::chat_usage_value(&usage),
            })));
        }
        out.push(Frame::Done);
        out
    }
}
