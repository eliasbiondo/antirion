//! Shadow comparison: a mirrored copy's answer against the one the client got. Each side reports
//! when it has finished (the copy when its body was read, the request when it settled); the side
//! that finishes last compares them and records the result for the mirroring node. Neither side
//! waits for the other, so the client's response is never delayed.

use std::sync::{Arc, Mutex};

use antirion_runtime::Runtime;
use serde_json::Value;

/// One side of a comparison.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Side {
    /// The normalised finish reason.
    pub(crate) finish: Option<String>,
    /// The output text with whitespace collapsed; `None` when it could not be read.
    pub(crate) text: Option<String>,
    pub(crate) latency_ms: f64,
    pub(crate) cost_usd: f64,
}

/// A copy and the request it shadows.
#[derive(Debug)]
pub(crate) struct Pair {
    rt: Arc<Runtime>,
    /// The mirroring node's path.
    node: String,
    sides: Mutex<(Option<Side>, Option<Side>)>,
}

impl Pair {
    pub(crate) fn new(rt: Arc<Runtime>, node: String) -> Arc<Pair> {
        Arc::new(Pair {
            rt,
            node,
            sides: Mutex::new((None, None)),
        })
    }

    pub(crate) fn served(&self, side: Side) {
        self.complete(Some(side), None);
    }

    pub(crate) fn shadow(&self, side: Side) {
        self.complete(None, Some(side));
    }

    fn complete(&self, served: Option<Side>, shadow: Option<Side>) {
        let both = {
            let Ok(mut s) = self.sides.lock() else { return };
            if served.is_some() {
                s.0 = served;
            }
            if shadow.is_some() {
                s.1 = shadow;
            }
            match (&s.0, &s.1) {
                (Some(a), Some(b)) => Some((a.clone(), b.clone())),
                _ => None,
            }
        };
        if let Some((served, shadow)) = both {
            self.rt.state.routing.shadows.record(
                &self.node,
                antirion_router::shadow::Compared {
                    at: self.rt.now(),
                    divergent: divergent(&served, &shadow),
                    latency_delta_ms: shadow.latency_ms - served.latency_ms,
                    cost_delta_usd: shadow.cost_usd - served.cost_usd,
                },
            );
        }
    }
}

/// Diverged: a different finish reason, or a different text when both texts are known.
fn divergent(a: &Side, b: &Side) -> bool {
    a.finish != b.finish || matches!((&a.text, &b.text), (Some(x), Some(y)) if x != y)
}

/// A finish reason in one vocabulary across dialects.
pub(crate) fn normalised_finish(reason: &str) -> String {
    match reason.to_ascii_lowercase().as_str() {
        "stop" | "end_turn" | "stop_sequence" | "completed" | "complete" | "end" => "stop".to_owned(),
        "length" | "max_tokens" | "max_output_tokens" | "incomplete" => "length".to_owned(),
        "tool_calls" | "tool_use" | "function_call" => "tool_calls".to_owned(),
        "content_filter" | "safety" | "refusal" | "recitation" | "blocklist" | "prohibited_content" => {
            "content_filter".to_owned()
        }
        other => other.to_owned(),
    }
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The text and finish reason one complete answer or stream event carries, in any dialect.
fn read_one(v: &Value, text: &mut String, finish: &mut Option<String>) {
    let mut put = |r: Option<&Value>| {
        if let Some(r) = r.and_then(Value::as_str).filter(|r| !r.is_empty()) {
            *finish = Some(normalised_finish(r));
        }
    };
    if let Some(choices) = v.get("choices").and_then(Value::as_array) {
        for c in choices.iter().take(1) {
            for t in [
                c.pointer("/message/content"),
                c.pointer("/delta/content"),
                c.get("text"),
            ]
            .into_iter()
            .flatten()
            {
                append(t, text);
            }
            put(c.get("finish_reason"));
        }
    }
    if let Some(parts) = v.pointer("/candidates/0/content/parts").and_then(Value::as_array) {
        for p in parts {
            append(p.get("text").unwrap_or(&Value::Null), text);
        }
        put(v.pointer("/candidates/0/finishReason"));
    }
    if v.get("type").and_then(Value::as_str) == Some("message") || v.get("stop_reason").is_some() {
        if let Some(blocks) = v.get("content").and_then(Value::as_array) {
            for b in blocks {
                append(b.get("text").unwrap_or(&Value::Null), text);
            }
        }
        put(v.get("stop_reason"));
    }
    match v.get("type").and_then(Value::as_str) {
        Some("content_block_delta") => append(v.pointer("/delta/text").unwrap_or(&Value::Null), text),
        Some("message_delta") => put(v.pointer("/delta/stop_reason")),
        Some("response.output_text.delta") => append(v.get("delta").unwrap_or(&Value::Null), text),
        Some("response.completed" | "response.incomplete") => put(v.pointer("/response/status")),
        _ => {}
    }
    if v.get("object").and_then(Value::as_str) == Some("response") {
        for item in v.get("output").and_then(Value::as_array).into_iter().flatten() {
            for c in item
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                append(c.get("text").unwrap_or(&Value::Null), text);
            }
        }
        put(v.get("status"));
    }
}

fn append(v: &Value, text: &mut String) {
    match v {
        Value::String(s) => text.push_str(s),
        Value::Array(parts) => {
            for p in parts {
                append(p.get("text").unwrap_or(p), text);
            }
        }
        _ => {}
    }
}

/// The finish reason and text of an answer: a JSON body (an object, or a JSON array of stream
/// chunks) or an event stream.
pub(crate) fn answer(body: &[u8]) -> (Option<String>, Option<String>) {
    let mut text = String::new();
    let mut finish = None;
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Array(chunks)) => {
            for c in &chunks {
                read_one(c, &mut text, &mut finish);
            }
        }
        Ok(v) => read_one(&v, &mut text, &mut finish),
        Err(_) => {
            let mut any = false;
            for line in body.split(|b| *b == b'\n') {
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                let Some(data) = line.strip_prefix(b"data:") else {
                    continue;
                };
                if let Ok(v) = serde_json::from_slice::<Value>(data.trim_ascii()) {
                    any = true;
                    read_one(&v, &mut text, &mut finish);
                }
            }
            if !any {
                return (None, None);
            }
        }
    }
    (finish, Some(collapse(&text)))
}

/// The finish reason and text of a parsed answer the client received.
pub(crate) fn answer_of(v: &Value) -> (Option<String>, Option<String>) {
    let mut text = String::new();
    let mut finish = None;
    read_one(v, &mut text, &mut finish);
    (finish, Some(collapse(&text)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_read_across_dialects() {
        let chat = br#"{"choices":[{"message":{"content":"Hello   world"},"finish_reason":"stop"}]}"#;
        assert_eq!(answer(chat), (Some("stop".into()), Some("Hello world".into())));
        let anthropic = br#"{"type":"message","content":[{"type":"text","text":"Hello\nworld"}],"stop_reason":"end_turn"}"#;
        assert_eq!(
            answer(anthropic),
            (Some("stop".into()), Some("Hello world".into()))
        );
        let sse = b"data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"length\"}]}\n\ndata: [DONE]\n\n";
        assert_eq!(answer(sse), (Some("length".into()), Some("Hello".into())));
        let gemini = br#"{"candidates":[{"content":{"parts":[{"text":"Hi"}]},"finishReason":"STOP"}]}"#;
        assert_eq!(answer(gemini), (Some("stop".into()), Some("Hi".into())));
        assert_eq!(answer(b"not json"), (None, None));
    }

    #[test]
    fn divergence_needs_a_difference() {
        let side = |f: &str, t: Option<&str>| Side {
            finish: Some(f.into()),
            text: t.map(str::to_owned),
            ..Side::default()
        };
        assert!(!divergent(&side("stop", Some("a b")), &side("stop", Some("a b"))));
        assert!(divergent(&side("stop", Some("a")), &side("stop", Some("b"))));
        assert!(divergent(&side("stop", Some("a")), &side("length", Some("a"))));
        assert!(!divergent(&side("stop", None), &side("stop", Some("b"))));
    }
}
