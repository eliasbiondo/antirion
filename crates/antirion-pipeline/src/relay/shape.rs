//! What the client receives for each upstream event: the event's own bytes (passthrough), the
//! Anthropic event inside a Bedrock `chunk` as server-sent events, or the event translated into the
//! client's dialect.

use antirion_dialects::xlate::stream::Translator;
use bytes::Bytes;
use serde_json::Value;

use super::encode::frame_bytes;
use super::events::{Classified, StreamDialect, StreamFacts};
use super::pump::UpEvent;
use super::render::{self, named};
use super::{Setup, settled_usage};

/// The per-stream transformation state.
#[derive(Debug, Default)]
pub(crate) struct Shaper {
    /// Upstream events to the client's dialect, in a translated cell.
    pub(crate) translator: Option<Translator>,
}

/// An event that could not be mapped: the stream is malformed.
#[derive(Debug)]
pub(crate) struct Unmappable;

const DONE: &[u8] = b"data: [DONE]\n\n";

impl Shaper {
    /// Whether the client's bytes differ from the upstream's.
    pub(crate) fn transforms(&self, setup: &Setup) -> bool {
        self.translator.is_some() || setup.dialect == StreamDialect::InvokeAnthropic || setup.reframe
    }

    /// The bytes one forwarded event becomes; the second value says whether they were rewritten.
    pub(crate) fn event(
        &mut self,
        setup: &Setup,
        ev: &UpEvent,
        c: &Classified,
        _facts: &mut StreamFacts,
    ) -> Result<(Vec<Bytes>, bool), Unmappable> {
        if let Some(t) = self.translator.as_mut() {
            let Some(json) = c.json.as_ref() else {
                return Ok((Vec::new(), true));
            };
            let frames = t.push(ev.event.as_deref(), json).map_err(|_| Unmappable)?;
            return Ok((frames.iter().map(frame_bytes).collect(), true));
        }
        if !c.forward {
            return Ok((Vec::new(), false));
        }
        if setup.reframe {
            let mut j = c.json.clone().unwrap_or(Value::Null);
            if let Some(m) = &setup.rewrite_model {
                match j.get_mut("model") {
                    Some(v) if setup.dialect == StreamDialect::Chat => *v = Value::from(m.as_str()),
                    _ => {
                        render::nested_model(&mut j, m);
                    }
                }
            }
            let bytes = match setup.client {
                super::ClientDialect::Anthropic | super::ClientDialect::Responses => named(&j),
                super::ClientDialect::Gemini => render::gemini_frame(&j),
                super::ClientDialect::Chat | super::ClientDialect::Bedrock => render::chat_frame(&j),
            };
            return Ok((vec![bytes], true));
        }
        if setup.dialect == StreamDialect::InvokeAnthropic {
            let mut j = c.json.clone().unwrap_or(Value::Null);
            if let Some(m) = &setup.rewrite_model {
                render::nested_model(&mut j, m);
            }
            return Ok((vec![named(&j)], true));
        }
        if setup.dialect == StreamDialect::Gemini && setup.client == super::ClientDialect::Gemini {
            let framed = format!(
                "data: {}\r\n\r\n",
                ev.data.trim_matches(|c: char| c == '\r' || c == '\n')
            );
            let changed = framed.as_bytes() != ev.raw.as_ref();
            return Ok((vec![Bytes::from(framed)], changed));
        }
        let (bytes, rewritten) = render::forward(setup, ev, c.json.as_ref());
        Ok((vec![bytes], rewritten))
    }

    /// The bytes that end a complete stream: the encoder's closing events, or the client dialect's
    /// terminator (the upstream's own when `terminator` is given and the stream is passthrough).
    pub(crate) fn finish(
        &mut self,
        setup: &Setup,
        facts: &StreamFacts,
        terminator: Option<Bytes>,
    ) -> Vec<Bytes> {
        if let Some(t) = self.translator.as_mut() {
            return t.finish().iter().map(frame_bytes).collect();
        }
        let usage = settled_usage(facts, setup.input_estimate);
        let mut out = Vec::new();
        let chat_client = setup.dialect == StreamDialect::Chat;
        if chat_client {
            if setup.client_wants_usage && !facts.usage_chunk_seen {
                out.push(render::usage_chunk(facts, setup.rewrite_model.as_deref(), usage));
            }
            out.push(terminator.unwrap_or_else(|| Bytes::from_static(DONE)));
            return out;
        }
        match (setup.dialect, terminator) {
            (StreamDialect::Anthropic | StreamDialect::InvokeAnthropic, None) => {
                out.push(named(&serde_json::json!({"type": "message_stop"})));
            }
            (StreamDialect::InvokeAnthropic, Some(_)) => {
                out.push(named(&serde_json::json!({"type": "message_stop"})));
            }
            (_, Some(t)) => out.push(t),
            (_, None) => {}
        }
        out
    }

    /// The sequence number an inserted event gets, for Responses clients.
    pub(crate) fn next_sequence(&self, facts: &StreamFacts) -> Option<i64> {
        match self.translator.as_ref() {
            Some(t) => t.next_sequence(),
            None => facts.sequence.map(|s| s + 1),
        }
    }
}
