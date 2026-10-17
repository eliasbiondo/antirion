//! Stream encoder for Gemini clients: one chunk per text or thought fragment, each function call as
//! one part once its arguments are complete, and a last chunk per candidate with its finish reason
//! and the usage. A signature that ends a thought is sent as an empty thought part after its text
//! (the decoder joins a thought's parts again); a Gemini signature before a call goes on the call.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::{Ev, Frame, StreamCx};
use crate::xlate::ir::Family;
use crate::xlate::resp::{
    OutCall, OutReasoning, Stop, Usage, gemini_call_part, gemini_finish, gemini_signature, gemini_thought,
    gemini_usage_value,
};
use crate::xlate::warn::{Kind, Warnings};

struct Held {
    id: String,
    name: String,
    args: String,
    sig: Option<String>,
}

pub(super) struct Enc {
    id: String,
    /// A Gemini signature that precedes a call, for that call.
    sig: Option<String>,
    /// Thought text was sent since the last signature: a signature now ends that thought.
    thinking: bool,
    held: BTreeMap<u64, Held>,
    stops: BTreeMap<u32, Stop>,
    usage: Option<Usage>,
}

impl Enc {
    pub(super) fn new(cx: &StreamCx) -> Enc {
        Enc {
            id: cx.fallback_id.clone(),
            sig: None,
            thinking: false,
            held: BTreeMap::new(),
            stops: BTreeMap::new(),
            usage: None,
        }
    }

    fn chunk(&self, cx: &StreamCx, choice: u32, part: Value) -> Frame {
        Frame::Gemini(json!({
            "candidates": [{"content": {"role": "model", "parts": [part]}, "index": choice}],
            "modelVersion": cx.model,
            "responseId": self.id,
        }))
    }

    fn flush_sig(&mut self, cx: &StreamCx, out: &mut Vec<Frame>) {
        if let Some(s) = self.sig.take() {
            out.push(self.chunk(cx, 0, gemini_thought("", Some(s))));
        }
    }

    fn signature(cx: &StreamCx, sig: String, family: Family) -> Option<String> {
        gemini_signature(
            &OutReasoning {
                text: String::new(),
                signature: Some(sig),
                redacted: None,
                family,
            },
            &cx.provider,
        )
    }

    fn call(&mut self, cx: &StreamCx, h: Held, w: &mut Warnings, out: &mut Vec<Frame>) {
        let c = OutCall {
            id: h.id,
            name: h.name,
            args: h.args,
            signature: None,
        };
        let part = gemini_call_part(&c, h.sig, w);
        out.push(self.chunk(cx, 0, part));
    }

    pub(super) fn push(&mut self, ev: Ev, cx: &StreamCx, w: &mut Warnings, out: &mut Vec<Frame>) {
        match ev {
            Ev::Meta { id } => {
                if !id.is_empty() {
                    self.id = id;
                }
            }
            Ev::Text { choice, text } => {
                self.flush_sig(cx, out);
                if !text.is_empty() {
                    out.push(self.chunk(cx, choice, json!({"text": text})));
                }
            }
            Ev::Refusal { text } => {
                self.flush_sig(cx, out);
                out.push(self.chunk(cx, 0, json!({"text": text})));
            }
            Ev::Think { text, .. } => {
                self.flush_sig(cx, out);
                self.thinking = true;
                out.push(self.chunk(cx, 0, gemini_thought(&text, None)));
            }
            Ev::Sig { sig, family } => {
                self.flush_sig(cx, out);
                let sig = Enc::signature(cx, sig, family);
                if family == Family::Gemini && !self.thinking {
                    self.sig = sig;
                } else if let Some(s) = sig {
                    out.push(self.chunk(cx, 0, gemini_thought("", Some(s))));
                }
                self.thinking = false;
            }
            Ev::Redacted { data, family } => {
                self.flush_sig(cx, out);
                if family == Family::Anthropic {
                    w.response(Kind::Dropped, "content[redacted_thinking]");
                } else if let Some(s) = Enc::signature(cx, data, family) {
                    out.push(self.chunk(cx, 0, gemini_thought("", Some(s))));
                }
            }
            Ev::End => {
                self.flush_sig(cx, out);
                self.thinking = false;
            }
            Ev::CallStart { key, id, name } => {
                let sig = self.sig.take();
                self.held.insert(
                    key,
                    Held {
                        id,
                        name,
                        args: String::new(),
                        sig,
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
                    self.call(cx, h, w, out);
                }
            }
            Ev::Call {
                id, name, args, sig, ..
            } => {
                let pending = self.sig.take();
                let sig = sig.or(pending);
                self.call(cx, Held { id, name, args, sig }, w, out);
            }
            Ev::Stop { choice, stop, .. } => {
                self.stops.entry(choice).or_insert(stop);
            }
            Ev::Usage(u) => self.usage = Some(u),
        }
    }

    pub(super) fn stop(&self) -> Stop {
        self.stops.get(&0).copied().unwrap_or(Stop::EndTurn)
    }

    pub(super) fn finish(&mut self, cx: &StreamCx, w: &mut Warnings) -> Vec<Frame> {
        let mut out = Vec::new();
        for (_, h) in std::mem::take(&mut self.held) {
            self.call(cx, h, w, &mut out);
        }
        self.flush_sig(cx, &mut out);
        if self.stops.is_empty() {
            self.stops.insert(0, Stop::EndTurn);
        }
        let candidates: Vec<Value> = self
            .stops
            .iter()
            .map(|(c, s)| {
                json!({"content": {"role": "model", "parts": [{"text": ""}]}, "finishReason": gemini_finish(*s),
                       "index": c})
            })
            .collect();
        let mut last = json!({"candidates": candidates});
        if let Some(o) = last.as_object_mut() {
            o.insert(
                "usageMetadata".into(),
                gemini_usage_value(&self.usage.clone().unwrap_or_default()),
            );
            o.insert("modelVersion".into(), cx.model.as_str().into());
            o.insert("responseId".into(), self.id.as_str().into());
        }
        out.push(Frame::Gemini(last));
        out
    }
}
