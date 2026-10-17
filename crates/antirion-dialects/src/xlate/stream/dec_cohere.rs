//! Cohere v2 Chat stream events: `message-start`, content and tool-call events with their
//! `-start`, `-delta` and `-end` forms, and `message-end` with the finish reason and usage.

use serde_json::Value;

use super::{Ev, StreamCx, restored};
use crate::xlate::ir::Family;
use crate::xlate::resp::{Stop, cohere_stop, cohere_usage};
use crate::xlate::util::str_of;

#[derive(Default)]
pub(super) struct Cohere {
    calls: bool,
    stop: Option<Stop>,
}

impl Cohere {
    pub(super) fn push(&mut self, data: &Value, cx: &StreamCx) -> Vec<Ev> {
        let mut out = Vec::new();
        let index = data.get("index").and_then(Value::as_u64).unwrap_or(0);
        let message = data.pointer("/delta/message").cloned().unwrap_or(Value::Null);
        match str_of(data, "type").unwrap_or_default() {
            "message-start" => {
                if let Some(id) = str_of(data, "id") {
                    out.push(Ev::Meta { id: id.to_owned() });
                }
            }
            "content-delta" => {
                let content = message.get("content").cloned().unwrap_or(Value::Null);
                if let Some(t) = str_of(&content, "text").filter(|t| !t.is_empty()) {
                    out.push(Ev::Text {
                        choice: 0,
                        text: t.to_owned(),
                    });
                }
                if let Some(t) = str_of(&content, "thinking").filter(|t| !t.is_empty()) {
                    out.push(Ev::Think {
                        text: t.to_owned(),
                        family: Family::None,
                    });
                }
            }
            "content-end" => out.push(Ev::End),
            "tool-call-start" => {
                let call = message.get("tool_calls").cloned().unwrap_or(Value::Null);
                let f = call.get("function").cloned().unwrap_or(Value::Null);
                let (id, name) = restored(
                    cx,
                    str_of(&call, "id").unwrap_or_default(),
                    str_of(&f, "name").unwrap_or_default(),
                );
                self.calls = true;
                out.push(Ev::CallStart { key: index, id, name });
                if let Some(a) = str_of(&f, "arguments").filter(|a| !a.is_empty()) {
                    out.push(Ev::CallArgs {
                        key: index,
                        frag: a.to_owned(),
                    });
                }
            }
            "tool-call-delta" => {
                if let Some(a) = message
                    .pointer("/tool_calls/function/arguments")
                    .and_then(Value::as_str)
                    .filter(|a| !a.is_empty())
                {
                    out.push(Ev::CallArgs {
                        key: index,
                        frag: a.to_owned(),
                    });
                }
            }
            "tool-call-end" => out.push(Ev::CallEnd { key: index }),
            "message-end" => {
                let delta = data.get("delta").cloned().unwrap_or(Value::Null);
                if let Some(u) = delta.get("usage").filter(|u| u.is_object()) {
                    out.push(Ev::Usage(cohere_usage(u)));
                }
                let reason = str_of(&delta, "finish_reason").unwrap_or("COMPLETE");
                self.stop = Some(cohere_stop(reason, self.calls));
            }
            _ => {}
        }
        out
    }

    pub(super) fn finish(&mut self) -> Vec<Ev> {
        vec![Ev::Stop {
            choice: 0,
            stop: self.stop.unwrap_or(Stop::EndTurn),
            seq: None,
        }]
    }
}
