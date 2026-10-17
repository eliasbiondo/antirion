//! Stream decoders of the native dialects: Anthropic events, Gemini chunks and ConverseStream
//! events.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::Value;

use super::dec_openai::merge;
use super::{Ev, Malformed, StreamCx, restored};
use crate::xlate::ir::Family;
use crate::xlate::resp::{Stop, Usage, anthropic_usage, converse_usage, gemini_call_id, gemini_usage};
use crate::xlate::util::{compact, str_of};
use crate::xlate::warn::{Kind, Warnings};

#[derive(Copy, Clone, PartialEq, Eq)]
enum Open {
    Segment,
    Tool,
    Ignored,
}

/// Anthropic events.
#[derive(Default)]
pub(super) struct Anthropic {
    blocks: HashMap<u64, Open>,
    usage: Option<Usage>,
    stop: Option<(Stop, Option<String>)>,
}

impl Anthropic {
    pub(super) fn push(
        &mut self,
        data: &Value,
        cx: &StreamCx,
        w: &mut Warnings,
    ) -> Result<Vec<Ev>, Malformed> {
        let mut out = Vec::new();
        let index = data.get("index").and_then(Value::as_u64).unwrap_or(0);
        match str_of(data, "type").unwrap_or_default() {
            "content_block_delta" | "content_block_stop" if !self.blocks.contains_key(&index) => {
                return Err(Malformed);
            }
            "message_start" => {
                let m = data.get("message").cloned().unwrap_or(Value::Null);
                out.push(Ev::Meta {
                    id: str_of(&m, "id").unwrap_or_default().to_owned(),
                });
                if let Some(u) = m.get("usage").filter(|u| u.is_object()) {
                    out.push(Ev::Usage(merge(&mut self.usage, anthropic_usage(u))));
                }
            }
            "content_block_start" => {
                let b = data.get("content_block").cloned().unwrap_or(Value::Null);
                match str_of(&b, "type") {
                    Some("text") => {
                        self.blocks.insert(index, Open::Segment);
                        if let Some(t) = str_of(&b, "text").filter(|t| !t.is_empty()) {
                            out.push(Ev::Text {
                                choice: 0,
                                text: t.to_owned(),
                            });
                        }
                    }
                    Some("thinking") => {
                        self.blocks.insert(index, Open::Segment);
                        if let Some(t) = str_of(&b, "thinking").filter(|t| !t.is_empty()) {
                            out.push(Ev::Think {
                                text: t.to_owned(),
                                family: Family::Anthropic,
                            });
                        }
                    }
                    Some("redacted_thinking") => {
                        self.blocks.insert(index, Open::Ignored);
                        out.push(Ev::Redacted {
                            data: str_of(&b, "data").unwrap_or_default().to_owned(),
                            family: Family::Anthropic,
                        });
                    }
                    Some("tool_use") => {
                        self.blocks.insert(index, Open::Tool);
                        let (id, name) = restored(
                            cx,
                            str_of(&b, "id").unwrap_or_default(),
                            str_of(&b, "name").unwrap_or_default(),
                        );
                        out.push(Ev::CallStart { key: index, id, name });
                    }
                    other => {
                        self.blocks.insert(index, Open::Ignored);
                        w.response(Kind::Dropped, &format!("content[{}]", other.unwrap_or("unknown")));
                    }
                }
            }
            "content_block_delta" => {
                let d = data.get("delta").cloned().unwrap_or(Value::Null);
                if self.blocks.get(&index) == Some(&Open::Ignored) {
                    return Ok(out);
                }
                match str_of(&d, "type") {
                    Some("text_delta") => {
                        if let Some(t) = str_of(&d, "text").filter(|t| !t.is_empty()) {
                            out.push(Ev::Text {
                                choice: 0,
                                text: t.to_owned(),
                            });
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(t) = str_of(&d, "thinking").filter(|t| !t.is_empty()) {
                            out.push(Ev::Think {
                                text: t.to_owned(),
                                family: Family::Anthropic,
                            });
                        }
                    }
                    Some("signature_delta") => {
                        if let Some(s) = str_of(&d, "signature").filter(|s| !s.is_empty()) {
                            out.push(Ev::Sig {
                                sig: s.to_owned(),
                                family: Family::Anthropic,
                            });
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(p) = str_of(&d, "partial_json").filter(|p| !p.is_empty()) {
                            out.push(Ev::CallArgs {
                                key: index,
                                frag: p.to_owned(),
                            });
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => match self.blocks.remove(&index) {
                Some(Open::Tool) => out.push(Ev::CallEnd { key: index }),
                Some(Open::Segment) => out.push(Ev::End),
                _ => {}
            },
            "message_delta" => {
                if let Some(r) = data.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    let seq = data
                        .pointer("/delta/stop_sequence")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    self.stop = Some((Stop::from_anthropic(r), seq));
                }
                if let Some(u) = data.get("usage").filter(|u| u.is_object()) {
                    let mut partial = anthropic_usage(u);
                    if u.get("cache_creation_input_tokens").is_none() {
                        partial.cache_write = None;
                    }
                    out.push(Ev::Usage(merge(&mut self.usage, partial)));
                }
            }
            _ => {}
        }
        Ok(out)
    }

    pub(super) fn finish(&mut self) -> Vec<Ev> {
        let (stop, seq) = self.stop.clone().unwrap_or((Stop::EndTurn, None));
        vec![Ev::Stop { choice: 0, stop, seq }]
    }
}

/// Gemini chunks: each part is complete; calls without an id get one derived from the response id
/// and the part's position.
pub(super) struct Gemini {
    response_id: String,
    parts: usize,
    stops: BTreeMap<u32, Stop>,
    has_call: HashSet<u32>,
    finish: BTreeMap<u32, String>,
}

impl Gemini {
    pub(super) fn new(fallback_id: &str) -> Gemini {
        Gemini {
            response_id: fallback_id.to_owned(),
            parts: 0,
            stops: BTreeMap::new(),
            has_call: HashSet::new(),
            finish: BTreeMap::new(),
        }
    }

    pub(super) fn push(&mut self, data: &Value, cx: &StreamCx, w: &mut Warnings) -> Vec<Ev> {
        let mut out = Vec::new();
        if self.parts == 0
            && let Some(id) = str_of(data, "responseId")
        {
            id.clone_into(&mut self.response_id);
            out.push(Ev::Meta { id: id.to_owned() });
        }
        for c in data
            .get("candidates")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let choice = c
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|i| u32::try_from(i).ok())
                .unwrap_or(0);
            for p in c
                .pointer("/content/parts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let index = self.parts;
                self.parts += 1;
                let sig = str_of(p, "thoughtSignature").map(str::to_owned);
                if let Some(fc) = p.get("functionCall") {
                    self.has_call.insert(choice);
                    if let Some(s) = &sig {
                        out.push(Ev::Sig {
                            sig: s.clone(),
                            family: Family::Gemini,
                        });
                    }
                    let raw_id = str_of(fc, "id")
                        .filter(|i| !i.is_empty())
                        .map_or_else(|| gemini_call_id(&self.response_id, index), str::to_owned);
                    let (id, name) = restored(cx, &raw_id, str_of(fc, "name").unwrap_or_default());
                    out.push(Ev::Call {
                        key: index as u64,
                        id,
                        name,
                        args: fc.get("args").map_or_else(|| "{}".to_owned(), compact),
                        sig,
                    });
                } else if let Some(t) = str_of(p, "text") {
                    if p.get("thought") == Some(&Value::Bool(true)) {
                        if !t.is_empty() {
                            out.push(Ev::Think {
                                text: t.to_owned(),
                                family: Family::Gemini,
                            });
                        }
                        if let Some(s) = sig {
                            out.push(Ev::Sig {
                                sig: s,
                                family: Family::Gemini,
                            });
                        }
                    } else if !t.is_empty() {
                        out.push(Ev::Text {
                            choice,
                            text: t.to_owned(),
                        });
                    }
                } else if let Some(key) = p
                    .as_object()
                    .and_then(|o| o.keys().find(|k| k.as_str() != "thoughtSignature"))
                {
                    w.response(Kind::Dropped, &format!("candidates[].content.parts[{key}]"));
                }
            }
            if let Some(f) = str_of(c, "finishReason") {
                self.finish.insert(choice, f.to_owned());
            }
        }
        if data.pointer("/promptFeedback/blockReason").is_some() {
            self.stops.insert(0, Stop::ContentFilter);
        }
        if let Some(u) = data.get("usageMetadata").filter(|u| u.is_object()) {
            out.push(Ev::Usage(gemini_usage(u)));
        }
        out
    }

    pub(super) fn finish(&mut self) -> Vec<Ev> {
        for (choice, f) in &self.finish {
            self.stops
                .entry(*choice)
                .or_insert_with(|| Stop::from_gemini(f, self.has_call.contains(choice)));
        }
        if self.stops.is_empty() {
            self.stops.insert(0, Stop::EndTurn);
        }
        self.stops
            .iter()
            .map(|(c, s)| Ev::Stop {
                choice: *c,
                stop: *s,
                seq: None,
            })
            .collect()
    }
}

/// ConverseStream events: tool input fragments are accumulated until the block stops and the call
/// is emitted whole; usage arrives in `metadata`, after `messageStop`.
#[derive(Default)]
pub(super) struct Converse {
    tools: HashMap<u64, (String, String, String)>,
    stop: Option<(Stop, Option<String>)>,
}

impl Converse {
    pub(super) fn push(&mut self, event: &str, data: &Value, cx: &StreamCx) -> Vec<Ev> {
        let mut out = Vec::new();
        let index = data.get("contentBlockIndex").and_then(Value::as_u64).unwrap_or(0);
        match event {
            "contentBlockStart" => {
                if let Some(tu) = data.pointer("/start/toolUse") {
                    self.tools.insert(
                        index,
                        (
                            str_of(tu, "toolUseId").unwrap_or_default().to_owned(),
                            str_of(tu, "name").unwrap_or_default().to_owned(),
                            String::new(),
                        ),
                    );
                }
            }
            "contentBlockDelta" => {
                let d = data.get("delta").cloned().unwrap_or(Value::Null);
                if let Some(t) = str_of(&d, "text").filter(|t| !t.is_empty()) {
                    out.push(Ev::Text {
                        choice: 0,
                        text: t.to_owned(),
                    });
                } else if let Some(input) = d.pointer("/toolUse/input").and_then(Value::as_str) {
                    if let Some(slot) = self.tools.get_mut(&index) {
                        slot.2.push_str(input);
                    }
                } else if let Some(rc) = d.get("reasoningContent") {
                    if let Some(t) = str_of(rc, "text").filter(|t| !t.is_empty()) {
                        out.push(Ev::Think {
                            text: t.to_owned(),
                            family: Family::Anthropic,
                        });
                    }
                    if let Some(s) = str_of(rc, "signature").filter(|s| !s.is_empty()) {
                        out.push(Ev::Sig {
                            sig: s.to_owned(),
                            family: Family::Anthropic,
                        });
                    }
                    if let Some(r) = str_of(rc, "redactedContent") {
                        out.push(Ev::Redacted {
                            data: r.to_owned(),
                            family: Family::Anthropic,
                        });
                    }
                }
            }
            "contentBlockStop" => match self.tools.remove(&index) {
                Some((id, name, args)) => {
                    let (id, name) = restored(cx, &id, &name);
                    out.push(Ev::Call {
                        key: index,
                        id,
                        name,
                        args: if args.is_empty() { "{}".to_owned() } else { args },
                        sig: None,
                    });
                }
                None => out.push(Ev::End),
            },
            "messageStop" => {
                let reason = str_of(data, "stopReason").unwrap_or("end_turn");
                let seq = data
                    .pointer("/additionalModelResponseFields/stop_sequence")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                self.stop = Some((Stop::from_converse(reason), seq));
            }
            "metadata" => {
                if let Some(u) = data.get("usage").filter(|u| u.is_object()) {
                    out.push(Ev::Usage(converse_usage(u)));
                }
            }
            _ => {}
        }
        out
    }

    pub(super) fn finish(&mut self) -> Vec<Ev> {
        let (stop, seq) = self.stop.clone().unwrap_or((Stop::EndTurn, None));
        vec![Ev::Stop { choice: 0, stop, seq }]
    }
}
