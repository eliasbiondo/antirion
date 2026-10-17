//! W3C trace context: ids, `traceparent` parsing and formatting.

use std::fmt;

/// A 16-byte trace id, never all zero.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct TraceId(pub [u8; 16]);

/// An 8-byte span id, never all zero.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct SpanId(pub [u8; 8]);

/// Random ids from the thread's fast stream: trace context ids need to be unique, not secret.
fn random_nonzero<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    for chunk in b.chunks_mut(8) {
        let bytes = antirion_core::rng::fast_u64().to_le_bytes();
        chunk.copy_from_slice(bytes.get(..chunk.len()).unwrap_or_default());
    }
    if b.iter().all(|x| *x == 0)
        && let Some(last) = b.last_mut()
    {
        *last = 1;
    }
    b
}

fn hex(bytes: &[u8]) -> String {
    use fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let mut out = [0u8; N];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    out.iter().any(|b| *b != 0).then_some(out)
}

impl TraceId {
    #[must_use]
    pub fn random() -> TraceId {
        TraceId(random_nonzero())
    }

    #[must_use]
    pub fn hex(&self) -> String {
        hex(&self.0)
    }

    /// The id written as 32 lowercase hex digits; `None` for any other text or the all-zero id.
    #[must_use]
    pub fn from_hex(s: &str) -> Option<TraceId> {
        unhex(s).map(TraceId)
    }

    /// The low 8 bytes, as ratio samplers read them.
    #[must_use]
    pub fn low_u64(&self) -> u64 {
        let mut b = [0u8; 8];
        b.copy_from_slice(self.0.get(8..).unwrap_or(&[0; 8]));
        u64::from_be_bytes(b)
    }
}

impl SpanId {
    #[must_use]
    pub fn random() -> SpanId {
        SpanId(random_nonzero())
    }

    #[must_use]
    pub fn hex(&self) -> String {
        hex(&self.0)
    }

    /// The id written as 16 lowercase hex digits; `None` for any other text or the all-zero id.
    #[must_use]
    pub fn from_hex(s: &str) -> Option<SpanId> {
        unhex(s).map(SpanId)
    }
}

/// A valid incoming `traceparent`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Parent {
    pub trace_id: TraceId,
    pub span_id: SpanId,
    pub sampled: bool,
}

/// Parses a `traceparent` header value; anything invalid (wrong version, length, case or an all-zero
/// id) is `None`, and a new trace starts.
#[must_use]
pub fn parse_traceparent(value: &str) -> Option<Parent> {
    let parts: Vec<&str> = value.trim().split('-').collect();
    let [version, trace, span, flags] = parts.as_slice() else {
        return None;
    };
    if *version != "00" {
        return None;
    }
    let flags = unhex_byte(flags)?;
    Some(Parent {
        trace_id: TraceId(unhex::<16>(trace)?),
        span_id: SpanId(unhex::<8>(span)?),
        sampled: flags & 1 == 1,
    })
}

fn unhex_byte(s: &str) -> Option<u8> {
    if s.len() != 2
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    u8::from_str_radix(s, 16).ok()
}

/// The `traceparent` value naming `span` of `trace`.
#[must_use]
pub fn format_traceparent(trace: &TraceId, span: &SpanId, sampled: bool) -> String {
    format!(
        "00-{}-{}-{}",
        trace.hex(),
        span.hex(),
        if sampled { "01" } else { "00" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_and_rejects_invalid() {
        let p = parse_traceparent("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01").unwrap();
        assert!(p.sampled);
        assert_eq!(p.trace_id.hex(), "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(p.span_id.hex(), "00f067aa0ba902b7");
        for bad in [
            "garbage",
            "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
            "00-abc-def-01",
            "ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01",
        ] {
            assert!(parse_traceparent(bad).is_none(), "{bad}");
        }
        let unsampled = parse_traceparent("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00").unwrap();
        assert!(!unsampled.sampled);
    }

    #[test]
    fn formats_round_trip() {
        let t = TraceId::random();
        let s = SpanId::random();
        let text = format_traceparent(&t, &s, true);
        let p = parse_traceparent(&text).unwrap();
        assert_eq!((p.trace_id, p.span_id, p.sampled), (t, s, true));
    }
}
