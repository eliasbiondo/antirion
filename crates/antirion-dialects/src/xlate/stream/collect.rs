//! An upstream stream gathered into one complete response: the stream decoders' events assembled
//! into the IR response, which the client codecs then encode as a non-streaming answer.

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

use super::{Decoder, Ev, Malformed, StreamCx};
use crate::xlate::resp::{Candidate, Out, OutCall, OutReasoning, Response, Stop, Usage};
use crate::xlate::warn::Warnings;

#[derive(Default)]
struct Building {
    blocks: Vec<Out>,
    /// The last text or reasoning block is still receiving deltas.
    open: bool,
    stop: Option<(Stop, Option<String>)>,
}

/// Collects a stream into a [`Response`].
pub struct Collector {
    dec: Decoder,
    cx: StreamCx,
    warnings: Warnings,
    id: Option<String>,
    candidates: BTreeMap<u32, Building>,
    /// Calls being streamed: their candidate and block.
    calls: HashMap<u64, (u32, usize)>,
    usage: Option<Usage>,
}

impl std::fmt::Debug for Collector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Collector")
            .field("cx", &self.cx)
            .finish_non_exhaustive()
    }
}

impl Collector {
    #[must_use]
    pub fn new(cx: StreamCx) -> Collector {
        Collector {
            dec: Decoder::new(&cx),
            cx,
            warnings: Warnings::default(),
            id: None,
            candidates: BTreeMap::new(),
            calls: HashMap::new(),
            usage: None,
        }
    }

    /// Adds one upstream event (`event` is the SSE event name or the event-stream type).
    pub fn push(&mut self, event: Option<&str>, data: &Value) -> Result<(), Malformed> {
        let evs = self.dec.push(event, data, &self.cx, &mut self.warnings)?;
        for ev in evs {
            self.add(ev);
        }
        Ok(())
    }

    fn add(&mut self, ev: Ev) {
        match ev {
            Ev::Meta { id } => {
                if !id.is_empty() {
                    self.id = Some(id);
                }
            }
            Ev::Text { choice, text } => {
                let c = self.candidates.entry(choice).or_default();
                match c.blocks.last_mut() {
                    Some(Out::Text(t)) if c.open => t.push_str(&text),
                    _ => c.blocks.push(Out::Text(text)),
                }
                c.open = true;
            }
            Ev::Refusal { text } => {
                let c = self.candidates.entry(0).or_default();
                match c.blocks.last_mut() {
                    Some(Out::Refusal(t)) => t.push_str(&text),
                    _ => c.blocks.push(Out::Refusal(text)),
                }
                c.open = false;
            }
            Ev::Think { text, family } => {
                let c = self.candidates.entry(0).or_default();
                match c.blocks.last_mut() {
                    Some(Out::Reasoning(r)) if c.open && r.signature.is_none() => r.text.push_str(&text),
                    _ => c.blocks.push(Out::Reasoning(OutReasoning {
                        text,
                        signature: None,
                        redacted: None,
                        family,
                    })),
                }
                c.open = true;
            }
            Ev::Sig { sig, family } => {
                let c = self.candidates.entry(0).or_default();
                match c.blocks.last_mut() {
                    Some(Out::Reasoning(r)) if c.open && r.signature.is_none() => r.signature = Some(sig),
                    _ => c.blocks.push(Out::Reasoning(OutReasoning {
                        text: String::new(),
                        signature: Some(sig),
                        redacted: None,
                        family,
                    })),
                }
                c.open = false;
            }
            Ev::Redacted { data, family } => {
                let c = self.candidates.entry(0).or_default();
                c.blocks.push(Out::Reasoning(OutReasoning {
                    text: String::new(),
                    signature: None,
                    redacted: Some(data),
                    family,
                }));
                c.open = false;
            }
            Ev::End => {
                for c in self.candidates.values_mut() {
                    c.open = false;
                }
            }
            Ev::CallStart { key, id, name } => {
                let c = self.candidates.entry(0).or_default();
                c.blocks.push(Out::Call(OutCall {
                    id,
                    name,
                    args: String::new(),
                    signature: None,
                }));
                c.open = false;
                self.calls.insert(key, (0, c.blocks.len() - 1));
            }
            Ev::CallArgs { key, frag } => {
                if let Some((choice, at)) = self.calls.get(&key)
                    && let Some(Out::Call(call)) = self
                        .candidates
                        .get_mut(choice)
                        .and_then(|c| c.blocks.get_mut(*at))
                {
                    call.args.push_str(&frag);
                }
            }
            Ev::CallEnd { key } => {
                if let Some((choice, at)) = self.calls.remove(&key)
                    && let Some(Out::Call(call)) = self
                        .candidates
                        .get_mut(&choice)
                        .and_then(|c| c.blocks.get_mut(at))
                    && call.args.is_empty()
                {
                    call.args = "{}".to_owned();
                }
            }
            Ev::Call {
                id, name, args, sig, ..
            } => {
                let c = self.candidates.entry(0).or_default();
                c.blocks.push(Out::Call(OutCall {
                    id,
                    name,
                    args,
                    signature: sig,
                }));
                c.open = false;
            }
            Ev::Stop { choice, stop, seq } => {
                self.candidates.entry(choice).or_default().stop = Some((stop, seq));
            }
            Ev::Usage(u) => self.usage = Some(u),
        }
    }

    /// The complete response, once the upstream stream has ended.
    #[must_use]
    pub fn finish(mut self) -> (Response, Warnings) {
        let tail = self.dec.finish(&self.cx);
        for ev in tail {
            self.add(ev);
        }
        if self.candidates.is_empty() {
            self.candidates.insert(0, Building::default());
        }
        let candidates = self
            .candidates
            .into_values()
            .map(|b| {
                let (stop, stop_sequence) = b.stop.unwrap_or((Stop::EndTurn, None));
                let mut c = Candidate {
                    blocks: b.blocks,
                    stop,
                    stop_sequence,
                    cites: Vec::new(),
                    searches: Vec::new(),
                };
                for out in &mut c.blocks {
                    if let Out::Call(call) = out
                        && call.args.is_empty()
                    {
                        call.args = "{}".to_owned();
                    }
                }
                if self.cx.restore.json_tool {
                    crate::xlate::resp::unwrap_json_tool(&mut c);
                }
                c
            })
            .collect();
        (
            Response {
                id: self.id,
                created: None,
                candidates,
                usage: self.usage,
            },
            self.warnings,
        )
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::xlate::encode::Restore;
    use crate::xlate::ir::Client;
    use crate::xlate::target::Upstream;

    fn cx(upstream: Upstream) -> StreamCx {
        StreamCx {
            client: Client::Chat,
            upstream,
            restore: Restore::default(),
            provider: "p".into(),
            model: "m".into(),
            created: 1,
            fallback_id: "f".into(),
            request: json!({}),
            include_usage: false,
            response_id: None,
        }
    }

    #[test]
    fn a_chat_stream_becomes_one_response() {
        let mut c = Collector::new(cx(Upstream::Chat));
        for chunk in [
            json!({"id": "c1", "choices": [{"index": 0, "delta": {"role": "assistant", "content": "Hel"}}]}),
            json!({"id": "c1", "choices": [{"index": 0, "delta": {"content": "lo"}}]}),
            json!({"id": "c1", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "t1",
                   "type": "function", "function": {"name": "f", "arguments": "{\"a\""}}]}}]}),
            json!({"id": "c1", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0,
                   "function": {"arguments": ":1}"}}]}}]}),
            json!({"id": "c1", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]}),
            json!({"id": "c1", "choices": [], "usage": {"prompt_tokens": 2, "completion_tokens": 3}}),
        ] {
            c.push(None, &chunk).unwrap();
        }
        let (resp, _) = c.finish();
        let cand = &resp.candidates[0];
        assert_eq!(cand.blocks[0], Out::Text("Hello".into()));
        match &cand.blocks[1] {
            Out::Call(call) => assert_eq!((call.name.as_str(), call.args.as_str()), ("f", "{\"a\":1}")),
            other => panic!("{other:?}"),
        }
        assert_eq!(cand.stop, Stop::ToolUse);
        assert_eq!(resp.usage.map(|u| (u.input, u.output)), Some((2, 3)));
    }

    #[test]
    fn a_cohere_stream_becomes_one_response() {
        let mut c = Collector::new(cx(Upstream::Cohere));
        for ev in [
            json!({"type": "message-start", "id": "co1"}),
            json!({"type": "content-delta", "index": 0, "delta": {"message": {"content": {"text": "Hi"}}}}),
            json!({"type": "content-end", "index": 0}),
            json!({"type": "message-end", "delta": {"finish_reason": "MAX_TOKENS",
                   "usage": {"tokens": {"input_tokens": 4, "output_tokens": 1}}}}),
        ] {
            c.push(None, &ev).unwrap();
        }
        let (resp, _) = c.finish();
        assert_eq!(resp.id.as_deref(), Some("co1"));
        assert_eq!(resp.candidates[0].stop, Stop::MaxTokens);
        assert_eq!(resp.usage.map(|u| u.output), Some(1));
    }
}
