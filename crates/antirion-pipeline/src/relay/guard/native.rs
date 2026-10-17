//! Output guardrails on the native Gemini and Bedrock ConverseStream streams: their text parts and
//! deltas become segment streams like the other dialects', and the assembled output is checked in
//! the dialect's own response grammar.

use antirion_guardrails::SegKind;
use antirion_wire::eventstream;
use bytes::Bytes;
use serde_json::{Map, Value, json};

use super::{Item, Key, OutGuard, Template, non_empty};

/// Whether a Gemini candidate still says something once its text parts are taken out.
fn candidate_left(c: &Value) -> bool {
    let parts = c
        .pointer("/content/parts")
        .and_then(Value::as_array)
        .is_some_and(|p| !p.is_empty());
    parts
        || c.as_object()
            .is_some_and(|o| o.keys().any(|k| !matches!(k.as_str(), "index" | "content")))
}

impl OutGuard {
    /// A Gemini chunk: each text part of each candidate becomes a text item; what remains (other
    /// parts, finish reasons, usage) follows the texts as one chunk.
    pub(super) fn gemini(&mut self, frame: Bytes, json: Value) {
        let mut rest = json.clone();
        let mut texts: Vec<(Key, String, Value)> = Vec::new();
        for (ci, c) in rest
            .get_mut("candidates")
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let index = c.get("index").and_then(Value::as_u64).unwrap_or(ci as u64);
            let Some(parts) = c.pointer_mut("/content/parts").and_then(Value::as_array_mut) else {
                continue;
            };
            let mut kept = Vec::new();
            for p in parts.drain(..) {
                let Some(t) = non_empty(p.get("text")).map(str::to_owned) else {
                    kept.push(p);
                    continue;
                };
                let kind = if p.get("thought").and_then(Value::as_bool) == Some(true) {
                    SegKind::Reasoning
                } else {
                    SegKind::Content
                };
                let mut part = p;
                if let Some(o) = part.as_object_mut() {
                    o.insert("text".into(), Value::from(""));
                }
                let mut chunk = json.clone();
                if let Some(o) = chunk.as_object_mut() {
                    o.insert(
                        "candidates".into(),
                        json!([{"content": {"parts": [part], "role": "model"}, "index": index}]),
                    );
                    o.remove("usageMetadata");
                }
                texts.push((
                    Key {
                        choice: index,
                        kind,
                        tool: None,
                    },
                    t,
                    chunk,
                ));
            }
            *parts = kept;
        }
        if texts.is_empty() {
            self.queue.push_back(Item::Raw(frame, Some((None, json))));
            return;
        }
        if let Some(cands) = rest.get_mut("candidates").and_then(Value::as_array_mut) {
            cands.retain(candidate_left);
        }
        let left = rest
            .get("candidates")
            .and_then(Value::as_array)
            .is_some_and(|c| !c.is_empty())
            || rest.get("usageMetadata").is_some()
            || rest.get("promptFeedback").is_some();
        for (key, text, chunk) in texts {
            self.text_item(
                key,
                &text,
                Template {
                    event: None,
                    json: chunk,
                    pointer: "/candidates/0/content/parts/0/text",
                },
            );
        }
        if left {
            self.queue
                .push_back(Item::Raw(super::gemini_frame(&rest), Some((None, rest))));
        }
    }

    /// The AWS event-stream messages of a ConverseStream frame: text and reasoning deltas become
    /// text items; every other message passes in order, and exceptions untouched.
    pub(super) fn converse(&mut self, frame: Bytes) {
        let mut decoder = eventstream::Decoder::new(frame.len().max(1));
        let mut messages = Vec::new();
        if decoder.push(&frame, &mut messages).is_err() || decoder.truncated() {
            self.queue.push_back(Item::Raw(frame, None));
            return;
        }
        for m in messages {
            let event_type = m.header(":event-type").map(str::to_owned);
            let parsed = (m.header(":message-type") == Some("event"))
                .then(|| serde_json::from_slice::<Value>(&m.payload).ok())
                .flatten()
                .filter(Value::is_object);
            let (Some(event_type), Some(json)) = (event_type, parsed) else {
                self.queue.push_back(Item::Raw(m.raw, None));
                continue;
            };
            let found = if event_type == "contentBlockDelta" {
                [
                    (SegKind::Content, "/delta/text"),
                    (SegKind::Reasoning, "/delta/reasoningContent/text"),
                ]
                .into_iter()
                .find_map(|(kind, pointer)| {
                    non_empty(json.pointer(pointer)).map(|t| (kind, pointer, t.to_owned()))
                })
            } else {
                None
            };
            match found {
                Some((kind, pointer, text)) => {
                    let index = json.get("contentBlockIndex").and_then(Value::as_u64).unwrap_or(0);
                    self.text_item(
                        Key {
                            choice: index,
                            kind,
                            tool: None,
                        },
                        &text,
                        Template {
                            event: Some(event_type),
                            json,
                            pointer,
                        },
                    );
                }
                None => self
                    .queue
                    .push_back(Item::Raw(m.raw, Some((Some(event_type), json)))),
            }
        }
    }

    /// The assembled output of a Gemini or ConverseStream stream in that dialect's response
    /// grammar, with the pointer of each stream's text.
    pub(super) fn assembled_native(&self) -> Option<(Value, Vec<String>)> {
        let mut pointers = Vec::new();
        match self.client {
            super::ClientDialect::Gemini => {
                let mut candidates = Vec::new();
                for (i, s) in self.streams.iter().enumerate() {
                    let mut part = Map::new();
                    part.insert("text".into(), Value::from(s.text.clone()));
                    if s.key.kind == SegKind::Reasoning {
                        part.insert("thought".into(), Value::Bool(true));
                    }
                    candidates.push(json!({"index": s.key.choice, "content": {"parts": [part]}}));
                    pointers.push(format!("/candidates/{i}/content/parts/0/text"));
                }
                Some((json!({"candidates": candidates}), pointers))
            }
            super::ClientDialect::Bedrock => {
                let mut content = Vec::new();
                for (i, s) in self.streams.iter().enumerate() {
                    if s.key.kind == SegKind::Reasoning {
                        content.push(json!({"reasoningContent": {"reasoningText": {"text": s.text}}}));
                        pointers.push(format!(
                            "/output/message/content/{i}/reasoningContent/reasoningText/text"
                        ));
                    } else {
                        content.push(json!({"text": s.text}));
                        pointers.push(format!("/output/message/content/{i}/text"));
                    }
                }
                Some((json!({"output": {"message": {"content": content}}}), pointers))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_candidate_without_its_texts_is_kept_only_when_it_says_more() {
        assert!(!candidate_left(
            &json!({"index": 0, "content": {"parts": [], "role": "model"}})
        ));
        assert!(candidate_left(
            &json!({"index": 0, "content": {"parts": []}, "finishReason": "STOP"})
        ));
        assert!(candidate_left(
            &json!({"index": 0, "content": {"parts": [{"functionCall": {}}]}})
        ));
    }
}
