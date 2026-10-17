//! What the reasoning replay store keeps: for each tool call of a translated response (by the id the
//! client sees), the signed or encrypted reasoning of its turn and a Gemini thought signature on the
//! call; and how a later request that lost them gets them back.

use std::sync::Arc;

use super::ir::{Block, Family, Reasoning, Request, Role};
use super::resp::{Out, Response};
use super::stream::Ev;

/// One reasoning block with its payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedReasoning {
    pub text: String,
    pub signature: Option<String>,
    pub redacted: Option<String>,
    pub family: Family,
    /// The provider an `openai`-family payload belongs to.
    pub provider: Option<String>,
}

/// What one tool call keeps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Saved {
    pub blocks: Vec<SavedReasoning>,
    /// A Gemini thought signature carried on the call itself.
    pub call_signature: Option<String>,
}

impl Saved {
    /// The size the entry counts against `max_entry_bytes`.
    #[must_use]
    pub fn bytes(&self) -> usize {
        let block = |b: &SavedReasoning| {
            b.text.len()
                + b.signature.as_ref().map_or(0, String::len)
                + b.redacted.as_ref().map_or(0, String::len)
        };
        self.blocks.iter().map(block).sum::<usize>() + self.call_signature.as_ref().map_or(0, String::len)
    }

    fn worth_keeping(&self) -> bool {
        !self.blocks.is_empty() || self.call_signature.is_some()
    }
}

fn provider_of(family: Family, provider: &str) -> Option<String> {
    (family == Family::OpenAi).then(|| provider.to_owned())
}

/// The entries a complete response saves (its first candidate is the turn the client keeps).
#[must_use]
pub fn entries(resp: &Response, provider: &str) -> Vec<(String, Saved)> {
    let mut blocks: Vec<SavedReasoning> = Vec::new();
    let mut out = Vec::new();
    for b in resp
        .candidates
        .first()
        .map(|c| c.blocks.as_slice())
        .unwrap_or_default()
    {
        match b {
            Out::Reasoning(r) if r.family == Family::Gemini && r.text.is_empty() => {}
            Out::Reasoning(r) if r.signature.is_some() || r.redacted.is_some() => {
                blocks.push(SavedReasoning {
                    text: r.text.clone(),
                    signature: r.signature.clone(),
                    redacted: r.redacted.clone(),
                    family: r.family,
                    provider: provider_of(r.family, provider),
                })
            }
            Out::Call(c) => {
                let saved = Saved {
                    blocks: blocks.clone(),
                    call_signature: c.signature.clone(),
                };
                if saved.worth_keeping() {
                    out.push((c.id.clone(), saved));
                }
            }
            _ => {}
        }
    }
    out
}

/// Gathers the same entries from a translated stream's events.
#[derive(Debug, Default)]
pub struct Recorder {
    blocks: Vec<SavedReasoning>,
    thinking: String,
    /// A Gemini signature waiting for the call that carries it.
    call_signature: Option<String>,
    entries: Vec<(String, Saved)>,
}

impl Recorder {
    pub fn push(&mut self, ev: &Ev, provider: &str) {
        match ev {
            Ev::Think { text, .. } => self.thinking.push_str(text),
            Ev::Sig { sig, family } if *family == Family::Gemini && self.thinking.is_empty() => {
                self.call_signature = Some(sig.clone());
            }
            Ev::Sig { sig, family } => self.blocks.push(SavedReasoning {
                text: std::mem::take(&mut self.thinking),
                signature: Some(sig.clone()),
                redacted: None,
                family: *family,
                provider: provider_of(*family, provider),
            }),
            Ev::Redacted { data, family } => self.blocks.push(SavedReasoning {
                text: String::new(),
                signature: None,
                redacted: Some(data.clone()),
                family: *family,
                provider: provider_of(*family, provider),
            }),
            Ev::End => self.thinking.clear(),
            Ev::CallStart { id, .. } => self.call(id, None),
            Ev::Call { id, sig, .. } => {
                let sig = sig.clone().or_else(|| self.call_signature.take());
                self.call(id, sig);
            }
            _ => {}
        }
    }

    fn call(&mut self, id: &str, call_signature: Option<String>) {
        let saved = Saved {
            blocks: self.blocks.clone(),
            call_signature,
        };
        if saved.worth_keeping() {
            self.entries.push((id.to_owned(), saved));
        }
    }

    /// The entries recorded so far.
    #[must_use]
    pub fn entries(&self) -> &[(String, Saved)] {
        &self.entries
    }
}

/// Re-inserts what the store kept for the tool calls of each assistant turn: the reasoning blocks at
/// the start of a turn that carries no reasoning payload of its own, and a Gemini signature on a call
/// that has none. Whether anything was inserted.
pub fn restore(req: &mut Request, lookup: impl Fn(&str) -> Option<Arc<Saved>>) -> bool {
    let mut any = false;
    for t in req.turns.iter_mut().filter(|t| t.role == Role::Assistant) {
        let carried = t
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Reasoning(r) if r.signature.is_some() || r.redacted.is_some()));
        let mut found: Vec<Arc<Saved>> = Vec::new();
        let mut src = None;
        for b in &mut t.blocks {
            let Block::ToolCall(c) = b else { continue };
            let Some(saved) = lookup(&c.id) else { continue };
            if c.signature.is_none() && saved.call_signature.is_some() {
                c.signature.clone_from(&saved.call_signature);
                any = true;
            }
            src.get_or_insert_with(|| c.src.clone());
            if !found.iter().any(|f| f.blocks == saved.blocks) {
                found.push(saved);
            }
        }
        let (false, Some(src)) = (carried, src) else {
            continue;
        };
        let inserted: Vec<Block> = found
            .iter()
            .flat_map(|s| s.blocks.iter())
            .map(|r| {
                Block::Reasoning(Reasoning {
                    text: r.text.clone(),
                    signature: r.signature.clone(),
                    redacted: r.redacted.clone(),
                    family: r.family,
                    provider: r.provider.clone(),
                    restored: true,
                    src: src.clone(),
                })
            })
            .collect();
        if !inserted.is_empty() {
            any = true;
            t.blocks.splice(0..0, inserted);
        }
    }
    any
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;
    use crate::xlate::decode::decode;
    use crate::xlate::encode::encode;
    use crate::xlate::ir::Client;
    use crate::xlate::resp::{Candidate, OutCall, OutReasoning, Stop};
    use crate::xlate::target::{Policy, Target, Upstream};

    #[test]
    fn a_stripped_tool_loop_gets_its_thinking_back_without_warnings() {
        let resp = Response {
            id: None,
            created: None,
            candidates: vec![Candidate {
                blocks: vec![
                    Out::Reasoning(OutReasoning {
                        text: "think".into(),
                        signature: Some("sig".into()),
                        redacted: None,
                        family: Family::Anthropic,
                    }),
                    Out::Call(OutCall {
                        id: "call_1".into(),
                        name: "f".into(),
                        args: "{}".into(),
                        signature: None,
                    }),
                ],
                stop: Stop::ToolUse,
                stop_sequence: None,
                cites: Vec::new(),
                searches: Vec::new(),
            }],
            usage: None,
        };
        let kept: HashMap<String, Arc<Saved>> = entries(&resp, "u")
            .into_iter()
            .map(|(k, v)| (k, Arc::new(v)))
            .collect();
        assert_eq!(kept.len(), 1);
        let body = json!({"model": "m", "reasoning_effort": "high", "messages": [
            {"role": "user", "content": "go"},
            {"role": "assistant", "content": null, "tool_calls": [
                {"id": "call_1", "type": "function", "function": {"name": "f", "arguments": "{}"}}]},
            {"role": "tool", "tool_call_id": "call_1", "content": "ok"}]});
        let mut ir = decode(Client::Chat, &body).unwrap();
        assert!(restore(&mut ir, |id| kept.get(id).cloned()));
        let mut t = Target::defaults(Upstream::Anthropic);
        t.control = crate::xlate::target::Control::Budget;
        let e = encode(&ir, &t, &Policy::default(), None).unwrap();
        assert_eq!(
            e.body["messages"][1]["content"][0],
            json!({"type": "thinking", "thinking": "think", "signature": "sig"})
        );
        assert!(e.body.get("thinking").is_some());
        assert!(e.warnings.entries().iter().all(|w| !w.starts_with("dropped=")));
        let gemini = encode(&ir, &Target::defaults(Upstream::Gemini), &Policy::default(), None).unwrap();
        assert!(
            gemini
                .warnings
                .entries()
                .iter()
                .all(|w| !w.starts_with("ignored=messages"))
        );
    }
}
