//! A Gemini stream for a client that asked for it without `alt=sse`: one JSON array, written as the
//! events arrive. `[` and the first event, each later event after `,\r\n`, and `]` once the stream
//! has ended (an error event is the array's last element).

use antirion_wire::sse::{SseEvent, SseParser};
use bytes::{BufMut, Bytes, BytesMut};

/// Re-frames the relay's outgoing Gemini server-sent events as array elements.
#[derive(Debug)]
pub(crate) struct ArrayFrames {
    parser: SseParser,
    started: bool,
}

impl Default for ArrayFrames {
    fn default() -> Self {
        ArrayFrames {
            parser: SseParser::new(usize::MAX),
            started: false,
        }
    }
}

impl ArrayFrames {
    /// The array bytes of the complete events in `sse`.
    pub(crate) fn reframe(&mut self, sse: &[u8]) -> Bytes {
        let mut events: Vec<SseEvent> = Vec::new();
        if self.parser.push(sse, &mut events).is_err() {
            return Bytes::new();
        }
        let mut out = BytesMut::new();
        for ev in &events {
            let data = ev.data.trim();
            if data.is_empty() {
                continue;
            }
            out.put_slice(if self.started { b",\r\n" } else { b"[" });
            self.started = true;
            out.put_slice(data.as_bytes());
        }
        out.freeze()
    }

    /// The bytes that close the array: `]`, after a `[` when no element was written.
    pub(crate) fn close(&mut self) -> Bytes {
        let closing: &'static [u8] = if self.started { b"]" } else { b"[]" };
        self.started = true;
        Bytes::from_static(closing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_become_elements() {
        let mut f = ArrayFrames::default();
        let first = f.reframe(b"data: {\"a\":1}\r\n\r\ndata: {\"b\"");
        assert_eq!(&first[..], b"[{\"a\":1}");
        let second = f.reframe(b":2}\r\n\r\n");
        assert_eq!(&second[..], b",\r\n{\"b\":2}");
        assert_eq!(&f.close()[..], b"]");
        let mut empty = ArrayFrames::default();
        assert_eq!(&empty.close()[..], b"[]");
    }
}
