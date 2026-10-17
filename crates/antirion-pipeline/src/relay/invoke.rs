//! An InvokeModel stream served by an Anthropic Messages upstream: the client reads AWS event-stream
//! messages, as Bedrock frames that operation, so each Anthropic event the relay writes becomes a
//! `chunk` message carrying the event's JSON in base64, and the stream's error event becomes an
//! `exception` message.

use antirion_wire::eventstream;
use antirion_wire::sse::{SseEvent, SseParser};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bytes::{Bytes, BytesMut};
use serde_json::{Value, json};

/// Re-frames the relay's outgoing Anthropic SSE bytes.
#[derive(Debug)]
pub(crate) struct InvokeFrames {
    parser: SseParser,
}

impl Default for InvokeFrames {
    fn default() -> Self {
        InvokeFrames {
            parser: SseParser::new(usize::MAX),
        }
    }
}

impl InvokeFrames {
    /// The event-stream messages of the complete events in `sse`; comments (heartbeats) have none.
    pub(crate) fn reframe(&mut self, sse: &[u8]) -> Bytes {
        let mut events = Vec::new();
        if self.parser.push(sse, &mut events).is_err() {
            return Bytes::new();
        }
        let mut out = BytesMut::new();
        for ev in &events {
            out.extend_from_slice(&message(ev));
        }
        out.freeze()
    }
}

/// The exception type of an in-stream error, by its `antirion.code`, among the members the
/// InvokeModel stream union models.
fn exception_type(code: &str) -> &'static str {
    match code {
        "upstream_rate_limited" => "throttlingException",
        "upstream_timeout" => "modelTimeoutException",
        "upstream_rejected" | "guardrail_blocked" => "validationException",
        "draining" => "serviceUnavailableException",
        "internal" => "internalServerException",
        _ => "modelStreamErrorException",
    }
}

fn message(ev: &SseEvent) -> Bytes {
    let v: Value = serde_json::from_str(&ev.data).unwrap_or(Value::Null);
    let error =
        ev.event.as_deref() == Some("error") || v.get("type").and_then(Value::as_str) == Some("error");
    if error {
        let code = v
            .pointer("/antirion/code")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let payload = json!({
            "message": v.pointer("/error/message").cloned().unwrap_or(Value::Null),
            "antirion": v.get("antirion").cloned().unwrap_or(Value::Null),
        });
        return eventstream::encode(
            &[
                (":exception-type", exception_type(code)),
                (":content-type", "application/json"),
                (":message-type", "exception"),
            ],
            payload.to_string().as_bytes(),
        );
    }
    let payload = json!({"bytes": STANDARD.encode(ev.data.as_bytes())});
    eventstream::encode(
        &[
            (":event-type", "chunk"),
            (":content-type", "application/json"),
            (":message-type", "event"),
        ],
        payload.to_string().as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(bytes: &[u8]) -> Vec<eventstream::Message> {
        let mut d = eventstream::Decoder::new(1 << 20);
        let mut out = Vec::new();
        d.push(bytes, &mut out).unwrap();
        assert!(!d.truncated());
        out
    }

    #[test]
    fn events_become_chunks_and_errors_exceptions() {
        let mut f = InvokeFrames::default();
        let start = b"event: message_start\ndata: {\"type\":\"message_start\"}\n\n: ping\n\n";
        let first = f.reframe(&start[..20]);
        assert!(first.is_empty(), "nothing until an event is complete");
        let msgs = decode(&f.reframe(&start[20..]));
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].header(":event-type"), Some("chunk"));
        let payload: Value = serde_json::from_slice(&msgs[0].payload).unwrap();
        let inner = STANDARD.decode(payload["bytes"].as_str().unwrap()).unwrap();
        assert_eq!(inner, br#"{"type":"message_start"}"#);
        let err = b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\",\"message\":\"m\"},\"antirion\":{\"code\":\"upstream_rate_limited\"}}\n\n";
        let msgs = decode(&f.reframe(err));
        assert_eq!(msgs[0].header(":message-type"), Some("exception"));
        assert_eq!(msgs[0].header(":exception-type"), Some("throttlingException"));
        let payload: Value = serde_json::from_slice(&msgs[0].payload).unwrap();
        assert_eq!(payload["message"], "m");
        assert_eq!(payload["antirion"]["code"], "upstream_rate_limited");
    }
}
