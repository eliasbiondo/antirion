//! Output guardrails on the Chat Completions, Messages and Responses streams: each event's text
//! deltas become items of their segment stream, and everything else is queued as it came.

use antirion_guardrails::SegKind;
use bytes::Bytes;
use serde_json::{Value, json};

use super::{Item, Key, OutGuard, Template, non_empty};
use crate::relay::render::chat_frame;

impl OutGuard {
    pub(super) fn chat(&mut self, frame: Bytes, event: Option<String>, json: Value) {
        if self.chat_base.is_none() && json.get("choices").is_some() {
            self.chat_base = Some(json.clone());
        }
        let mut rest = json.clone();
        let mut texts: Vec<(Key, String, Value, &'static str)> = Vec::new();
        let mut trailing = rest.get("usage").is_some_and(|u| !u.is_null());
        if let Some(choices) = rest.get_mut("choices").and_then(Value::as_array_mut) {
            for c in choices.iter_mut() {
                let index = c.get("index").and_then(Value::as_u64).unwrap_or(0);
                if !self.open_choices.contains(&index) {
                    self.open_choices.push(index);
                }
                trailing |= c.get("finish_reason").is_some_and(|f| !f.is_null());
                let base = |delta: Value| json!({"index": index, "delta": delta, "finish_reason": null});
                if let Some(t) = non_empty(c.get("text")).map(str::to_owned) {
                    let key = Key {
                        choice: index,
                        kind: SegKind::Content,
                        tool: None,
                    };
                    texts.push((
                        key,
                        t,
                        json!({"index": index, "text": "", "finish_reason": null}),
                        "/choices/0/text",
                    ));
                    if let Some(o) = c.as_object_mut() {
                        o.insert("text".into(), Value::from(""));
                    }
                }
                let Some(delta) = c.get_mut("delta").and_then(Value::as_object_mut) else {
                    continue;
                };
                for (field, kind) in [
                    ("content", SegKind::Content),
                    ("reasoning_content", SegKind::Reasoning),
                    ("refusal", SegKind::Refusal),
                ] {
                    if let Some(t) = non_empty(delta.get(field)).map(str::to_owned) {
                        delta.remove(field);
                        let key = Key {
                            choice: index,
                            kind,
                            tool: None,
                        };
                        let pointer = match field {
                            "content" => "/choices/0/delta/content",
                            "reasoning_content" => "/choices/0/delta/reasoning_content",
                            _ => "/choices/0/delta/refusal",
                        };
                        texts.push((key, t, base(json!({ field: "" })), pointer));
                    }
                }
                for call in delta
                    .get_mut("tool_calls")
                    .and_then(Value::as_array_mut)
                    .into_iter()
                    .flatten()
                {
                    let k = call.get("index").and_then(Value::as_u64).unwrap_or(0);
                    let Some(args) = call.get_mut("function").and_then(|f| f.get_mut("arguments")) else {
                        continue;
                    };
                    if let Some(t) = non_empty(Some(args)).map(str::to_owned) {
                        *args = Value::from("");
                        let key = Key {
                            choice: index,
                            kind: SegKind::ToolArguments,
                            tool: Some(k),
                        };
                        texts.push((
                            key,
                            t,
                            base(json!({"tool_calls": [{"index": k, "function": {"arguments": ""}}]})),
                            "/choices/0/delta/tool_calls/0/function/arguments",
                        ));
                    }
                }
                if delta
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .is_some_and(|calls| {
                        calls.iter().all(|c| {
                            c.as_object().is_some_and(|o| {
                                o.keys().all(|k| k == "index" || k == "function")
                                    && o.get("function").and_then(Value::as_object).is_none_or(|f| {
                                        f.keys().all(|k| k == "arguments")
                                            && f.get("arguments").and_then(Value::as_str) == Some("")
                                    })
                            })
                        })
                    })
                {
                    delta.remove("tool_calls");
                }
            }
        }
        if texts.is_empty() {
            self.queue.push_back(Item::Raw(frame, Some((event, json))));
            return;
        }
        let empty_rest = rest.get("choices").and_then(Value::as_array).is_some_and(|cs| {
            cs.iter().all(|c| {
                c.get("delta")
                    .and_then(Value::as_object)
                    .is_none_or(serde_json::Map::is_empty)
                    && c.get("finish_reason").is_none_or(Value::is_null)
                    && c.get("text").is_none_or(|t| t.as_str() == Some(""))
            })
        }) && rest.get("usage").is_none_or(Value::is_null);
        let rest_frame = (!empty_rest).then(|| Item::Raw(chat_frame(&rest), Some((None, rest.clone()))));
        if !trailing && let Some(r) = rest_frame {
            self.queue.push_back(r);
            self.push_texts(&json, texts);
            return;
        }
        self.push_texts(&json, texts);
        if let Some(r) = rest_frame {
            self.queue.push_back(r);
        }
    }

    fn push_texts(&mut self, chunk: &Value, texts: Vec<(Key, String, Value, &'static str)>) {
        for (key, text, choice, pointer) in texts {
            let mut t = chunk.clone();
            if let Some(o) = t.as_object_mut() {
                o.insert("choices".into(), Value::Array(vec![choice]));
                o.remove("usage");
            }
            self.text_item(
                key,
                &text,
                Template {
                    event: None,
                    json: t,
                    pointer,
                },
            );
        }
    }

    pub(super) fn anthropic(&mut self, frame: Bytes, event: Option<String>, json: Value) {
        let index = json.get("index").and_then(Value::as_u64).unwrap_or(0);
        match json.get("type").and_then(Value::as_str) {
            Some("content_block_start") => self.open_block = Some(index),
            Some("content_block_stop") => self.open_block = None,
            Some("content_block_delta") => {
                let delta = json.get("delta");
                let found = match delta.and_then(|d| d.get("type")).and_then(Value::as_str) {
                    Some("text_delta") => Some((SegKind::Content, "text", "/delta/text")),
                    Some("thinking_delta") => Some((SegKind::Reasoning, "thinking", "/delta/thinking")),
                    Some("input_json_delta") => {
                        Some((SegKind::ToolArguments, "partial_json", "/delta/partial_json"))
                    }
                    _ => None,
                };
                if let Some((kind, field, pointer)) = found
                    && let Some(t) = non_empty(delta.and_then(|d| d.get(field))).map(str::to_owned)
                {
                    let key = Key {
                        choice: index,
                        kind,
                        tool: None,
                    };
                    self.text_item(key, &t, Template { event, json, pointer });
                    return;
                }
            }
            _ => {}
        }
        self.queue.push_back(Item::Raw(frame, Some((event, json))));
    }

    pub(super) fn responses(&mut self, frame: Bytes, event: Option<String>, json: Value) {
        if self.seq.is_none() {
            self.seq = json.get("sequence_number").and_then(Value::as_i64);
        }
        let kind = json.get("type").and_then(Value::as_str).unwrap_or_default();
        if matches!(kind, "response.created" | "response.in_progress")
            && let Some(r) = json.get("response")
        {
            self.response = Some(r.clone());
        }
        let found = match kind {
            "response.output_text.delta" => Some(SegKind::Content),
            "response.refusal.delta" => Some(SegKind::Refusal),
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                Some(SegKind::Reasoning)
            }
            "response.function_call_arguments.delta" => Some(SegKind::ToolArguments),
            _ => None,
        };
        if let Some(kind) = found
            && let Some(t) = non_empty(json.get("delta")).map(str::to_owned)
        {
            let n = |k: &str| json.get(k).and_then(Value::as_u64).unwrap_or(0);
            let key = Key {
                choice: n("output_index"),
                kind,
                tool: Some(n("content_index").max(n("summary_index"))),
            };
            self.text_item(
                key,
                &t,
                Template {
                    event,
                    json,
                    pointer: "/delta",
                },
            );
            return;
        }
        self.queue.push_back(Item::Raw(frame, Some((event, json))));
    }
}
