//! Streams translated incrementally: each upstream event is decoded into stream IR events and
//! encoded for the client before the next one is read. Only terminal events (finish reason, usage)
//! and tool calls that cannot be emitted yet are held.

mod anthropic;
mod chat;
mod collect;
mod converse;
mod dec_cohere;
mod dec_native;
mod dec_openai;
mod gemini;
mod responses;

use std::collections::HashSet;

use serde_json::Value;

use super::encode::{JSON_TOOL, Restore};
use super::ir::{Client, Family};
use super::replay::{Recorder, Saved};
use super::resp::{Stop, Usage};
use super::target::Upstream;
use super::warn::{Kind, Warnings};

/// One stream IR event.
#[derive(Clone, Debug, PartialEq)]
pub enum Ev {
    Meta {
        id: String,
    },
    Text {
        choice: u32,
        text: String,
    },
    Refusal {
        text: String,
    },
    /// Reasoning text of the current reasoning segment.
    Think {
        text: String,
        family: Family,
    },
    /// The signature (or encrypted payload) of the current reasoning segment, which ends it.
    Sig {
        sig: String,
        family: Family,
    },
    Redacted {
        data: String,
        family: Family,
    },
    /// The current text or reasoning segment ended.
    End,
    CallStart {
        key: u64,
        id: String,
        name: String,
    },
    CallArgs {
        key: u64,
        frag: String,
    },
    CallEnd {
        key: u64,
    },
    /// A call that arrives complete.
    Call {
        key: u64,
        id: String,
        name: String,
        args: String,
        sig: Option<String>,
    },
    Stop {
        choice: u32,
        stop: Stop,
        seq: Option<String>,
    },
    Usage(Usage),
}

/// An upstream event that cannot be mapped: the stream is malformed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Malformed;

/// One frame for the client.
#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    /// A `data:` event.
    Data(Value),
    /// An event named after its `type`.
    Named(Value),
    /// Chat's `[DONE]`.
    Done,
    /// A Gemini chunk, as a `data:` event with Google's `CR LF` separators.
    Gemini(Value),
    /// A ConverseStream event: its type and payload, one AWS event-stream message.
    Event(&'static str, Value),
}

/// What a stream translation needs, owned (it lives in the relay task).
#[derive(Clone, Debug)]
pub struct StreamCx {
    pub client: Client,
    pub upstream: Upstream,
    pub restore: Restore,
    pub provider: String,
    pub model: String,
    pub created: i64,
    pub fallback_id: String,
    /// The client's request body (Responses echo fields, `include`, `store`).
    pub request: Value,
    /// A Chat client asked for the usage chunk.
    pub include_usage: bool,
    /// A fixed id for the Responses object (gateway-stored responses).
    pub response_id: Option<String>,
}

pub use self::collect::Collector;

enum Decoder {
    Chat(dec_openai::Chat),
    Responses(dec_openai::Responses),
    Anthropic(dec_native::Anthropic),
    Gemini(dec_native::Gemini),
    Converse(dec_native::Converse),
    Cohere(dec_cohere::Cohere),
}

impl Decoder {
    fn new(cx: &StreamCx) -> Decoder {
        match cx.upstream {
            Upstream::Chat => Decoder::Chat(dec_openai::Chat::default()),
            Upstream::Responses => Decoder::Responses(dec_openai::Responses::default()),
            Upstream::Anthropic => Decoder::Anthropic(dec_native::Anthropic::default()),
            Upstream::Gemini => Decoder::Gemini(dec_native::Gemini::new(&cx.fallback_id)),
            Upstream::Converse => Decoder::Converse(dec_native::Converse::default()),
            Upstream::Cohere => Decoder::Cohere(dec_cohere::Cohere::default()),
        }
    }

    fn push(
        &mut self,
        event: Option<&str>,
        data: &Value,
        cx: &StreamCx,
        w: &mut Warnings,
    ) -> Result<Vec<Ev>, Malformed> {
        Ok(match self {
            Decoder::Chat(d) => d.push(data, cx),
            Decoder::Responses(d) => d.push(data, cx, w),
            Decoder::Anthropic(d) => d.push(data, cx, w)?,
            Decoder::Gemini(d) => d.push(data, cx, w),
            Decoder::Converse(d) => d.push(event.unwrap_or_default(), data, cx),
            Decoder::Cohere(d) => d.push(data, cx),
        })
    }

    fn finish(&mut self, cx: &StreamCx) -> Vec<Ev> {
        match self {
            Decoder::Chat(d) => d.finish(cx),
            Decoder::Responses(d) => d.finish(),
            Decoder::Anthropic(d) => d.finish(),
            Decoder::Gemini(d) => d.finish(),
            Decoder::Converse(d) => d.finish(),
            Decoder::Cohere(d) => d.finish(),
        }
    }
}

enum Encoder {
    Chat(chat::Enc),
    Anthropic(anthropic::Enc),
    Responses(responses::Enc),
    Gemini(gemini::Enc),
    Converse(converse::Enc),
}

/// A translating stream: upstream events in, client frames out.
pub struct Translator {
    dec: Decoder,
    enc: Encoder,
    cx: StreamCx,
    warnings: Warnings,
    /// Calls of the structured-output tool, streamed to the client as text.
    json_calls: HashSet<u64>,
    /// What the reasoning replay store keeps of this stream.
    recorder: Recorder,
}

impl std::fmt::Debug for Translator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Translator")
            .field("cx", &self.cx)
            .finish_non_exhaustive()
    }
}

impl Translator {
    #[must_use]
    pub fn new(cx: StreamCx) -> Translator {
        let dec = Decoder::new(&cx);
        let enc = match cx.client {
            Client::Chat => Encoder::Chat(chat::Enc::new(&cx)),
            Client::Anthropic => Encoder::Anthropic(anthropic::Enc::new(&cx)),
            Client::Responses => Encoder::Responses(responses::Enc::new(&cx)),
            Client::Gemini => Encoder::Gemini(gemini::Enc::new(&cx)),
            Client::Converse => Encoder::Converse(converse::Enc::new()),
        };
        Translator {
            dec,
            enc,
            cx,
            warnings: Warnings::default(),
            json_calls: HashSet::new(),
            recorder: Recorder::default(),
        }
    }

    /// Translates one upstream event (`event` is the SSE event name or the event-stream type).
    pub fn push(&mut self, event: Option<&str>, data: &Value) -> Result<Vec<Frame>, Malformed> {
        let evs = self.dec.push(event, data, &self.cx, &mut self.warnings)?;
        Ok(self.encode(evs))
    }

    /// Ends the stream: held terminal events, then the client dialect's closing events.
    pub fn finish(&mut self) -> Vec<Frame> {
        let evs = self.dec.finish(&self.cx);
        let mut out = self.encode(evs);
        let stop = match &mut self.enc {
            Encoder::Chat(e) => e.stop(),
            Encoder::Anthropic(e) => e.stop(),
            Encoder::Responses(e) => e.stop(),
            Encoder::Gemini(e) => e.stop(),
            Encoder::Converse(e) => e.stop(),
        };
        if stop.approximated(self.cx.client) {
            self.warnings.response(Kind::Approximated, "stop_reason");
        }
        out.extend(match &mut self.enc {
            Encoder::Chat(e) => e.finish(&self.cx),
            Encoder::Anthropic(e) => e.finish(&self.cx),
            Encoder::Responses(e) => e.finish(&self.cx),
            Encoder::Gemini(e) => e.finish(&self.cx, &mut self.warnings),
            Encoder::Converse(e) => e.finish(&mut self.warnings),
        });
        out
    }

    fn encode(&mut self, evs: Vec<Ev>) -> Vec<Frame> {
        let mut out = Vec::new();
        for ev in evs {
            let Some(ev) = self.json_text(ev) else { continue };
            self.recorder.push(&ev, &self.cx.provider);
            match &mut self.enc {
                Encoder::Chat(e) => e.push(ev, &self.cx, &mut out),
                Encoder::Anthropic(e) => e.push(ev, &self.cx, &mut self.warnings, &mut out),
                Encoder::Responses(e) => e.push(ev, &self.cx, &mut out),
                Encoder::Gemini(e) => e.push(ev, &self.cx, &mut self.warnings, &mut out),
                Encoder::Converse(e) => e.push(ev, &self.cx, &mut self.warnings, &mut out),
            }
        }
        out
    }

    /// Emulated structured output: the forced tool's arguments are streamed as text.
    fn json_text(&mut self, ev: Ev) -> Option<Ev> {
        if !self.cx.restore.json_tool {
            return Some(ev);
        }
        match ev {
            Ev::CallStart { key, name, .. } if name == JSON_TOOL => {
                self.json_calls.insert(key);
                None
            }
            Ev::Call { name, args, .. } if name == JSON_TOOL => Some(Ev::Text {
                choice: 0,
                text: args,
            }),
            Ev::CallArgs { key, frag } if self.json_calls.contains(&key) => Some(Ev::Text {
                choice: 0,
                text: frag,
            }),
            Ev::CallEnd { key } if self.json_calls.contains(&key) => None,
            Ev::Stop {
                choice,
                stop: Stop::ToolUse,
                seq,
            } if !self.json_calls.is_empty() => Some(Ev::Stop {
                choice,
                stop: Stop::EndTurn,
                seq,
            }),
            other => Some(other),
        }
    }

    /// The reasoning replay entries of the stream so far.
    #[must_use]
    pub fn replay(&self) -> &[(String, Saved)] {
        self.recorder.entries()
    }

    /// The response-side entries of the stream so far.
    #[must_use]
    pub fn warnings(&self) -> &Warnings {
        &self.warnings
    }

    /// The final Responses object a Responses client received.
    #[must_use]
    pub fn completed(&self) -> Option<&Value> {
        match &self.enc {
            Encoder::Responses(e) => e.completed(),
            _ => None,
        }
    }

    /// The `sequence_number` an inserted Responses event takes.
    #[must_use]
    pub fn next_sequence(&self) -> Option<i64> {
        match &self.enc {
            Encoder::Responses(e) => Some(e.next_sequence()),
            _ => None,
        }
    }
}

/// The restored id and name of an upstream call.
pub(super) fn restored(cx: &StreamCx, id: &str, name: &str) -> (String, String) {
    (cx.restore.ids.client(id), cx.restore.names.client(name))
}
