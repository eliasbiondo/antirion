//! Pre-rendered response bodies and stream events, built once at startup.

use bytes::Bytes;
use serde_json::{Value, json};

use crate::profile::STREAM_EVENTS;

/// Every body the mock serves.
#[derive(Debug)]
pub struct Responses {
    pub chat_small: Bytes,
    pub chat_large: Bytes,
    /// The role chunk, 40 content chunks, the usage chunk and `[DONE]`.
    pub chat_stream: Vec<Bytes>,
    pub messages: Bytes,
    /// `message_start` with `content_block_start`, 40 deltas, then the closing events in one frame.
    pub messages_stream: Vec<Bytes>,
    pub embeddings: Bytes,
    pub mcp_tool_text: String,
}

const CREATED: u64 = 1_790_000_000;

/// Deterministic text of exactly `len` bytes.
fn text(len: usize, salt: usize) -> String {
    const WORDS: [&str; 8] = [
        "alpha", "bravo", "delta", "omega", "sigma", "theta", "kappa", "gamma",
    ];
    let mut out = String::with_capacity(len + 8);
    let mut i = salt;
    while out.len() < len {
        out.push_str(WORDS.get(i % WORDS.len()).copied().unwrap_or("x"));
        out.push(' ');
        i += 3;
    }
    out.truncate(len);
    out
}

fn usage() -> Value {
    json!({"prompt_tokens": 256, "completion_tokens": 256, "total_tokens": 512})
}

/// A body whose content string is padded so the whole JSON is exactly `size` bytes.
fn padded(size: usize, render: impl Fn(&str) -> String) -> Bytes {
    let empty = render("").len();
    Bytes::from(render(&text(size.saturating_sub(empty), 0)))
}

fn chat(size: usize) -> Bytes {
    padded(size, |content| {
        json!({
            "id": "chatcmpl-bench",
            "object": "chat.completion",
            "created": CREATED,
            "model": "bench-upstream",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": content},
                         "finish_reason": "stop"}],
            "usage": usage(),
        })
        .to_string()
    })
}

fn sse(data: &Value) -> Bytes {
    Bytes::from(format!("data: {data}\n\n"))
}

fn sse_event(event: &str, data: &Value) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

fn chunk(delta: &Value, finish: Option<&str>) -> Value {
    json!({
        "id": "chatcmpl-bench",
        "object": "chat.completion.chunk",
        "created": CREATED,
        "model": "bench-upstream",
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
    })
}

fn chat_stream() -> Vec<Bytes> {
    let mut events = vec![sse(&chunk(&json!({"role": "assistant", "content": ""}), None))];
    for i in 0..STREAM_EVENTS {
        let finish = (i + 1 == STREAM_EVENTS).then_some("stop");
        events.push(sse(&chunk(&json!({"content": text(24, i as usize)}), finish)));
    }
    let mut tail = chunk(&json!({}), None);
    tail["choices"] = json!([]);
    tail["usage"] = usage();
    let mut last = format!("data: {tail}\n\n");
    last.push_str("data: [DONE]\n\n");
    events.push(Bytes::from(last));
    events
}

fn messages(size: usize) -> Bytes {
    padded(size, |content| {
        json!({
            "id": "msg_bench",
            "type": "message",
            "role": "assistant",
            "model": "bench-upstream",
            "content": [{"type": "text", "text": content}],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {"input_tokens": 256, "output_tokens": 256},
        })
        .to_string()
    })
}

fn messages_stream() -> Vec<Bytes> {
    let start = json!({"type": "message_start", "message": {
        "id": "msg_bench", "type": "message", "role": "assistant", "model": "bench-upstream",
        "content": [], "stop_reason": null, "stop_sequence": null,
        "usage": {"input_tokens": 256, "output_tokens": 1}}});
    let block = json!({"type": "content_block_start", "index": 0,
                       "content_block": {"type": "text", "text": ""}});
    let mut events = vec![Bytes::from(
        sse_event("message_start", &start) + &sse_event("content_block_start", &block),
    )];
    for i in 0..STREAM_EVENTS {
        let delta = json!({"type": "content_block_delta", "index": 0,
                           "delta": {"type": "text_delta", "text": text(24, i as usize)}});
        events.push(Bytes::from(sse_event("content_block_delta", &delta)));
    }
    let stop = json!({"type": "content_block_stop", "index": 0});
    let delta = json!({"type": "message_delta", "delta": {"stop_reason": "end_turn", "stop_sequence": null},
                       "usage": {"output_tokens": 256}});
    let end = json!({"type": "message_stop"});
    events.push(Bytes::from(
        sse_event("content_block_stop", &stop)
            + &sse_event("message_delta", &delta)
            + &sse_event("message_stop", &end),
    ));
    events
}

fn embeddings() -> Bytes {
    let mut out = String::with_capacity(220 * 1024);
    out.push_str(r#"{"object":"list","data":["#);
    let mut state: u32 = 0x9E37_79B9;
    for i in 0..16 {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(r#"{{"object":"embedding","index":{i},"embedding":["#));
        for j in 0..1536 {
            if j > 0 {
                out.push(',');
            }
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let v = f64::from(state % 200_000) / 100_000.0 - 1.0;
            out.push_str(&format!("{v:.5}"));
        }
        out.push_str("]}");
    }
    out.push_str(r#"],"model":"bench-upstream","usage":{"prompt_tokens":1024,"total_tokens":1024}}"#);
    Bytes::from(out)
}

impl Responses {
    #[must_use]
    pub fn build() -> Self {
        Self {
            chat_small: chat(1024),
            chat_large: chat(4096),
            chat_stream: chat_stream(),
            messages: messages(1024),
            messages_stream: messages_stream(),
            embeddings: embeddings(),
            mcp_tool_text: text(1024, 5),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_match_the_matrix() {
        let r = Responses::build();
        assert_eq!(r.chat_small.len(), 1024);
        assert_eq!(r.chat_large.len(), 4096);
        assert_eq!(r.messages.len(), 1024);
        assert_eq!(r.chat_stream.len(), 42);
        assert_eq!(r.messages_stream.len(), 42);
        let kib = r.embeddings.len() / 1024;
        assert!((180..240).contains(&kib), "{kib} KiB");
        let v: Value = serde_json::from_slice(&r.embeddings).unwrap();
        assert_eq!(v["data"][15]["embedding"].as_array().unwrap().len(), 1536);
        let content = r.chat_stream.get(1).unwrap().len();
        assert!((150..260).contains(&content), "{content} B");
    }

    #[test]
    fn stream_events_are_json() {
        let r = Responses::build();
        for ev in &r.chat_stream {
            for line in std::str::from_utf8(ev).unwrap().lines() {
                if let Some(data) = line.strip_prefix("data: ")
                    && data != "[DONE]"
                {
                    serde_json::from_str::<Value>(data).unwrap();
                }
            }
        }
    }
}
