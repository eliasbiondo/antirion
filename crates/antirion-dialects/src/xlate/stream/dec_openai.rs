//! Stream decoders of the OpenAI dialects: Chat chunks and Responses events.

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

use super::{Ev, StreamCx, restored};
use crate::xlate::ir::Family;
use crate::xlate::resp::{Stop, Usage};
use crate::xlate::util::str_of;
use crate::xlate::warn::{Kind, Warnings};

#[derive(Default)]
struct Slot {
    id: Option<String>,
    name: Option<String>,
    started: bool,
    pending: String,
    sig: Option<String>,
}

/// Chat chunks: tool-call fragments are joined by `index` (id and name may arrive once, and before
/// the name is known fragments are held); the first non-null finish reason wins.
#[derive(Default)]
pub(super) struct Chat {
    meta: bool,
    calls: BTreeMap<u64, Slot>,
    stops: BTreeMap<u32, Stop>,
}

fn reasoning_entries(list: &[Value], out: &mut Vec<Ev>) {
    for d in list {
        let family = str_of(d, "format").map_or(Family::None, Family::parse);
        match str_of(d, "type") {
            Some("reasoning.encrypted") => {
                if let Some(data) = str_of(d, "data") {
                    if family == Family::Anthropic {
                        out.push(Ev::Redacted {
                            data: data.to_owned(),
                            family,
                        });
                    } else {
                        out.push(Ev::Sig {
                            sig: data.to_owned(),
                            family,
                        });
                    }
                }
            }
            _ => {
                if let Some(t) = str_of(d, "text").filter(|t| !t.is_empty()) {
                    out.push(Ev::Think {
                        text: t.to_owned(),
                        family,
                    });
                }
                if let Some(s) = str_of(d, "signature").filter(|s| !s.is_empty()) {
                    out.push(Ev::Sig {
                        sig: s.to_owned(),
                        family,
                    });
                }
            }
        }
    }
}

impl Chat {
    pub(super) fn push(&mut self, data: &Value, cx: &StreamCx) -> Vec<Ev> {
        let mut out = Vec::new();
        if !self.meta
            && let Some(id) = str_of(data, "id")
        {
            self.meta = true;
            out.push(Ev::Meta { id: id.to_owned() });
        }
        for ch in data
            .get("choices")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let choice = ch
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|i| u32::try_from(i).ok())
                .unwrap_or(0);
            let d = ch.get("delta").cloned().unwrap_or(Value::Null);
            for key in ["reasoning_content", "reasoning"] {
                if let Some(t) = str_of(&d, key).filter(|t| !t.is_empty()) {
                    out.push(Ev::Think {
                        text: t.to_owned(),
                        family: Family::None,
                    });
                }
            }
            if let Some(Value::Array(list)) = d.get("reasoning_details") {
                reasoning_entries(list, &mut out);
            }
            if let Some(t) = str_of(&d, "content").filter(|t| !t.is_empty()) {
                out.push(Ev::Text {
                    choice,
                    text: t.to_owned(),
                });
            }
            if let Some(t) = str_of(&d, "refusal").filter(|t| !t.is_empty()) {
                out.push(Ev::Refusal { text: t.to_owned() });
            }
            for tc in d
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                self.call_delta(tc, cx, &mut out);
            }
            if let Some(f) = str_of(ch, "finish_reason") {
                self.stops.entry(choice).or_insert_with(|| Stop::from_chat(f));
            }
        }
        if let Some(u) = data.get("usage").filter(|u| u.is_object()) {
            out.push(Ev::Usage(crate::xlate::resp::chat_usage(u)));
        }
        out
    }

    fn call_delta(&mut self, tc: &Value, cx: &StreamCx, out: &mut Vec<Ev>) {
        let key = tc.get("index").and_then(Value::as_u64).unwrap_or(0);
        let slot = self.calls.entry(key).or_default();
        if let Some(id) = str_of(tc, "id").filter(|i| !i.is_empty()) {
            slot.id = Some(id.to_owned());
        }
        let f = tc.get("function");
        if let Some(n) = f.and_then(|f| str_of(f, "name")).filter(|n| !n.is_empty()) {
            slot.name = Some(n.to_owned());
        }
        if let Some(s) = tc
            .pointer("/extra_content/google/thought_signature")
            .and_then(Value::as_str)
        {
            slot.sig = Some(s.to_owned());
        }
        let frag = f.and_then(|f| str_of(f, "arguments")).unwrap_or_default();
        if slot.started {
            if !frag.is_empty() {
                out.push(Ev::CallArgs {
                    key,
                    frag: frag.to_owned(),
                });
            }
            return;
        }
        slot.pending.push_str(frag);
        if let Some(name) = slot.name.clone() {
            slot.started = true;
            let id = slot.id.clone().unwrap_or_else(|| format!("call_{key}"));
            if let Some(sig) = slot.sig.clone() {
                out.push(Ev::Sig {
                    sig,
                    family: Family::Gemini,
                });
            }
            let (id, name) = restored(cx, &id, &name);
            out.push(Ev::CallStart { key, id, name });
            if !slot.pending.is_empty() {
                out.push(Ev::CallArgs {
                    key,
                    frag: std::mem::take(&mut slot.pending),
                });
            }
        }
    }

    pub(super) fn finish(&mut self, cx: &StreamCx) -> Vec<Ev> {
        let mut out = Vec::new();
        for (key, slot) in &mut self.calls {
            if !slot.started {
                let (id, name) = restored(
                    cx,
                    slot.id.as_deref().unwrap_or_default(),
                    slot.name.as_deref().unwrap_or_default(),
                );
                out.push(Ev::Call {
                    key: *key,
                    id,
                    name,
                    args: std::mem::take(&mut slot.pending),
                    sig: slot.sig.clone(),
                });
            } else {
                out.push(Ev::CallEnd { key: *key });
            }
        }
        if self.stops.is_empty() {
            self.stops.insert(0, Stop::EndTurn);
        }
        for (choice, stop) in &self.stops {
            out.push(Ev::Stop {
                choice: *choice,
                stop: *stop,
                seq: None,
            });
        }
        out
    }
}

/// Responses events: items keyed by `output_index` (deltas that also carry `item_id` count once).
#[derive(Default)]
pub(super) struct Responses {
    meta: bool,
    keys: HashMap<String, u64>,
    calls: HashMap<u64, bool>,
    ignored: Vec<u64>,
    has_call: bool,
    stop: Option<Stop>,
}

impl Responses {
    fn key(&self, data: &Value) -> Option<u64> {
        data.get("output_index")
            .and_then(Value::as_u64)
            .or_else(|| str_of(data, "item_id").and_then(|i| self.keys.get(i).copied()))
    }

    pub(super) fn push(&mut self, data: &Value, cx: &StreamCx, w: &mut Warnings) -> Vec<Ev> {
        let mut out = Vec::new();
        let kind = str_of(data, "type").unwrap_or_default();
        let key = self.key(data);
        if key.is_some_and(|k| self.ignored.contains(&k)) {
            return out;
        }
        match kind {
            "response.created" => {
                if !self.meta {
                    self.meta = true;
                    if let Some(id) = data.pointer("/response/id").and_then(Value::as_str) {
                        out.push(Ev::Meta { id: id.to_owned() });
                    }
                }
            }
            "response.output_item.added" => {
                let item = data.get("item").cloned().unwrap_or(Value::Null);
                let k = key.unwrap_or(0);
                if let Some(id) = str_of(&item, "id") {
                    self.keys.insert(id.to_owned(), k);
                }
                match str_of(&item, "type") {
                    Some("function_call") => {
                        self.has_call = true;
                        self.calls.insert(k, false);
                        let (id, name) = restored(
                            cx,
                            str_of(&item, "call_id").unwrap_or_default(),
                            str_of(&item, "name").unwrap_or_default(),
                        );
                        out.push(Ev::CallStart { key: k, id, name });
                    }
                    Some("message" | "reasoning") | None => {}
                    Some(other) => {
                        self.ignored.push(k);
                        w.response(Kind::Dropped, &format!("output[{other}]"));
                    }
                }
            }
            "response.output_text.delta" => {
                if let Some(t) = str_of(data, "delta").filter(|t| !t.is_empty()) {
                    out.push(Ev::Text {
                        choice: 0,
                        text: t.to_owned(),
                    });
                }
            }
            "response.refusal.delta" => {
                if let Some(t) = str_of(data, "delta") {
                    out.push(Ev::Refusal { text: t.to_owned() });
                }
            }
            "response.output_text.done" => out.push(Ev::End),
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(t) = str_of(data, "delta").filter(|t| !t.is_empty()) {
                    out.push(Ev::Think {
                        text: t.to_owned(),
                        family: Family::OpenAi,
                    });
                }
            }
            "response.function_call_arguments.delta" => {
                if let (Some(k), Some(t)) = (key, str_of(data, "delta")) {
                    self.calls.insert(k, true);
                    out.push(Ev::CallArgs {
                        key: k,
                        frag: t.to_owned(),
                    });
                }
            }
            "response.output_item.done" => {
                let item = data.get("item").cloned().unwrap_or(Value::Null);
                let k = key.unwrap_or(0);
                match str_of(&item, "type") {
                    Some("reasoning") => {
                        if let Some(enc) = str_of(&item, "encrypted_content").filter(|e| !e.is_empty()) {
                            out.push(Ev::Sig {
                                sig: enc.to_owned(),
                                family: Family::OpenAi,
                            });
                        }
                        out.push(Ev::End);
                    }
                    Some("function_call") => {
                        if self.calls.get(&k) == Some(&false)
                            && let Some(a) = str_of(&item, "arguments").filter(|a| !a.is_empty())
                        {
                            out.push(Ev::CallArgs {
                                key: k,
                                frag: a.to_owned(),
                            });
                        }
                        out.push(Ev::CallEnd { key: k });
                    }
                    _ => out.push(Ev::End),
                }
            }
            "response.completed" | "response.incomplete" => {
                let r = data.get("response").cloned().unwrap_or(Value::Null);
                if let Some(u) = r.get("usage").filter(|u| u.is_object()) {
                    out.push(Ev::Usage(crate::xlate::resp::responses_usage(u)));
                }
                let reason = r.get("incomplete_details").and_then(|d| str_of(d, "reason"));
                let status = str_of(&r, "status").unwrap_or(if kind == "response.completed" {
                    "completed"
                } else {
                    "incomplete"
                });
                self.stop = Some(Stop::from_responses(status, reason, self.has_call));
            }
            _ => {}
        }
        out
    }

    pub(super) fn finish(&mut self) -> Vec<Ev> {
        let stop = self.stop.unwrap_or(if self.has_call {
            Stop::ToolUse
        } else {
            Stop::EndTurn
        });
        vec![Ev::Stop {
            choice: 0,
            stop,
            seq: None,
        }]
    }
}

/// Merges a usage report into what is known: later non-zero counts replace earlier ones.
pub(super) fn merge(into: &mut Option<Usage>, u: Usage) -> Usage {
    let merged = match into.take() {
        None => u,
        Some(old) => Usage {
            input: if u.input > 0 { u.input } else { old.input },
            output: if u.output > 0 { u.output } else { old.output },
            cache_read: if u.cache_read > 0 {
                u.cache_read
            } else {
                old.cache_read
            },
            cache_write: u
                .cache_write
                .filter(|w| *w > 0)
                .or(old.cache_write)
                .or(u.cache_write),
            reasoning: u.reasoning.or(old.reasoning),
        },
    };
    *into = Some(merged.clone());
    merged
}
