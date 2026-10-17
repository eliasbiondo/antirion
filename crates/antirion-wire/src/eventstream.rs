//! The AWS event-stream binary framing: an incremental decoder that verifies both CRCs and keeps
//! each message's original bytes, and an encoder for the messages the gateway writes itself.

use bytes::{Buf, Bytes, BytesMut};

/// One decoded message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// The whole message as received, prelude to trailing CRC.
    pub raw: Bytes,
    /// String-valued headers; headers of other types are checked and skipped.
    pub headers: Vec<(String, String)>,
    pub payload: Bytes,
}

impl Message {
    /// The value of a string header.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Why the byte stream cannot be decoded further.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EventStreamError {
    #[error("an event-stream message is larger than the limit")]
    TooLarge,
    #[error("an event-stream message is malformed")]
    Malformed,
}

const PRELUDE: usize = 12;
const MAX_HEADERS: usize = 131_072;

/// The incremental decoder.
#[derive(Debug)]
pub struct Decoder {
    pending: BytesMut,
    max_message: usize,
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 4)?;
    Some(u32::from_be_bytes([
        *s.first()?,
        *s.get(1)?,
        *s.get(2)?,
        *s.get(3)?,
    ]))
}

fn parse_headers(mut h: &[u8]) -> Result<Vec<(String, String)>, EventStreamError> {
    let bad = EventStreamError::Malformed;
    let mut out = Vec::new();
    while h.has_remaining() {
        let name_len = usize::from(*h.first().ok_or(bad)?);
        if name_len == 0 {
            return Err(bad);
        }
        let name = h.get(1..=name_len).ok_or(bad)?;
        let name = std::str::from_utf8(name).map_err(|_| bad)?.to_owned();
        h = h.get(name_len + 1..).ok_or(bad)?;
        let kind = *h.first().ok_or(bad)?;
        h = h.get(1..).ok_or(bad)?;
        let fixed = match kind {
            0 | 1 => Some(0),
            2 => Some(1),
            3 => Some(2),
            4 => Some(4),
            5 | 8 => Some(8),
            9 => Some(16),
            6 | 7 => None,
            _ => return Err(bad),
        };
        match fixed {
            Some(n) => h = h.get(n..).ok_or(bad)?,
            None => {
                let len_bytes = h.get(..2).ok_or(bad)?;
                let len = usize::from(u16::from_be_bytes([
                    *len_bytes.first().ok_or(bad)?,
                    *len_bytes.get(1).ok_or(bad)?,
                ]));
                let value = h.get(2..2 + len).ok_or(bad)?;
                if kind == 7 {
                    let v = std::str::from_utf8(value).map_err(|_| bad)?;
                    out.push((name, v.to_owned()));
                }
                h = h.get(2 + len..).ok_or(bad)?;
            }
        }
    }
    Ok(out)
}

impl Decoder {
    #[must_use]
    pub fn new(max_message: usize) -> Decoder {
        Decoder {
            pending: BytesMut::new(),
            max_message,
        }
    }

    /// Bytes received that do not yet form a whole message.
    #[must_use]
    pub fn buffered(&self) -> usize {
        self.pending.len()
    }

    /// Feeds one read; every message it completes is appended to `out`.
    pub fn push(&mut self, chunk: &[u8], out: &mut Vec<Message>) -> Result<(), EventStreamError> {
        self.pending.extend_from_slice(chunk);
        loop {
            if self.pending.len() < PRELUDE {
                return Ok(());
            }
            let total = be32(&self.pending, 0).ok_or(EventStreamError::Malformed)? as usize;
            let headers_len = be32(&self.pending, 4).ok_or(EventStreamError::Malformed)? as usize;
            let prelude_crc = be32(&self.pending, 8).ok_or(EventStreamError::Malformed)?;
            if crc32fast::hash(self.pending.get(..8).unwrap_or_default()) != prelude_crc {
                return Err(EventStreamError::Malformed);
            }
            if total > self.max_message {
                return Err(EventStreamError::TooLarge);
            }
            if total < PRELUDE + 4 || headers_len > MAX_HEADERS || headers_len > total - PRELUDE - 4 {
                return Err(EventStreamError::Malformed);
            }
            if self.pending.len() < total {
                return Ok(());
            }
            let raw = self.pending.split_to(total).freeze();
            let body_end = total - 4;
            let crc = be32(&raw, body_end).ok_or(EventStreamError::Malformed)?;
            if crc32fast::hash(raw.get(..body_end).unwrap_or_default()) != crc {
                return Err(EventStreamError::Malformed);
            }
            let headers = parse_headers(raw.get(PRELUDE..PRELUDE + headers_len).unwrap_or_default())?;
            let payload = raw.slice(PRELUDE + headers_len..body_end);
            out.push(Message {
                raw,
                headers,
                payload,
            });
        }
    }

    /// At a clean end: leftover bytes mean the stream was cut inside a message.
    #[must_use]
    pub fn truncated(&self) -> bool {
        !self.pending.is_empty()
    }
}

/// Encodes one message with string headers.
#[must_use]
pub fn encode(headers: &[(&str, &str)], payload: &[u8]) -> Bytes {
    let mut h: Vec<u8> = Vec::new();
    for (name, value) in headers {
        h.push(u8::try_from(name.len()).unwrap_or(u8::MAX));
        h.extend_from_slice(name.as_bytes());
        h.push(7);
        h.extend_from_slice(&u16::try_from(value.len()).unwrap_or(u16::MAX).to_be_bytes());
        h.extend_from_slice(value.as_bytes());
    }
    let total = PRELUDE + h.len() + payload.len() + 4;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&u32::try_from(total).unwrap_or(u32::MAX).to_be_bytes());
    out.extend_from_slice(&u32::try_from(h.len()).unwrap_or(u32::MAX).to_be_bytes());
    let prelude_crc = crc32fast::hash(&out);
    out.extend_from_slice(&prelude_crc.to_be_bytes());
    out.extend_from_slice(&h);
    out.extend_from_slice(payload);
    let crc = crc32fast::hash(&out);
    out.extend_from_slice(&crc.to_be_bytes());
    Bytes::from(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: &str, payload: &str) -> Bytes {
        encode(
            &[
                (":event-type", kind),
                (":content-type", "application/json"),
                (":message-type", "event"),
            ],
            payload.as_bytes(),
        )
    }

    #[test]
    fn round_trip_at_every_split() {
        let mut all = event("messageStart", r#"{"role":"assistant"}"#).to_vec();
        all.extend_from_slice(&event("messageStop", r#"{"stopReason":"end_turn"}"#));
        for cut in 0..all.len() {
            let mut d = Decoder::new(1 << 20);
            let mut out = Vec::new();
            d.push(&all[..cut], &mut out).unwrap();
            d.push(&all[cut..], &mut out).unwrap();
            assert_eq!(out.len(), 2, "cut at {cut}");
            assert_eq!(out[0].header(":event-type"), Some("messageStart"));
            assert_eq!(&out[1].payload[..], br#"{"stopReason":"end_turn"}"#);
            assert!(!d.truncated());
        }
    }

    #[test]
    fn corruption_and_limits() {
        let mut m = event("x", "{}").to_vec();
        let last = m.len() - 1;
        m[last] ^= 1;
        assert_eq!(
            Decoder::new(1 << 20).push(&m, &mut Vec::new()),
            Err(EventStreamError::Malformed)
        );
        let m = event("x", "{}");
        assert_eq!(
            Decoder::new(8).push(&m, &mut Vec::new()),
            Err(EventStreamError::TooLarge)
        );
        let mut d = Decoder::new(1 << 20);
        d.push(&m[..20], &mut Vec::new()).unwrap();
        assert!(d.truncated());
        let mut bad_type = encode(&[], b"{}").to_vec();
        bad_type.clear();
        let headers = [1u8, b'a', 42];
        let total = (12 + headers.len() + 4) as u32;
        bad_type.extend_from_slice(&total.to_be_bytes());
        bad_type.extend_from_slice(&(headers.len() as u32).to_be_bytes());
        let pc = crc32fast::hash(&bad_type);
        bad_type.extend_from_slice(&pc.to_be_bytes());
        bad_type.extend_from_slice(&headers);
        let c = crc32fast::hash(&bad_type);
        bad_type.extend_from_slice(&c.to_be_bytes());
        assert_eq!(
            Decoder::new(1 << 20).push(&bad_type, &mut Vec::new()),
            Err(EventStreamError::Malformed)
        );
    }
}
