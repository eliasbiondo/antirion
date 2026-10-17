//! Byte-level edits at known spans, applied in one pass. The bytes outside the edited spans are
//! copied unchanged, so an untouched body is forwarded exactly as the client sent it.

use std::ops::Range;

use bytes::{Bytes, BytesMut};

/// A set of non-overlapping span replacements.
#[derive(Debug, Default)]
pub struct Splice {
    edits: Vec<(Range<usize>, Bytes)>,
}

impl Splice {
    pub fn replace_span(&mut self, at: Range<usize>, json: Bytes) {
        self.edits.push((at, json));
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    /// The edited body as slices of `raw` around the replacement bytes, in order: nothing is copied.
    /// Edits are taken as [`Splice::apply`] takes them.
    #[must_use]
    pub fn apply_parts(mut self, raw: &Bytes) -> Vec<Bytes> {
        if self.edits.is_empty() {
            return vec![raw.clone()];
        }
        self.edits.sort_by_key(|(r, _)| r.start);
        let mut out = Vec::with_capacity(self.edits.len() * 2 + 1);
        let mut cursor = 0;
        for (range, bytes) in self.edits {
            if range.start < cursor || range.end > raw.len() || range.start > range.end {
                continue;
            }
            out.push(raw.slice(cursor..range.start));
            out.push(bytes);
            cursor = range.end;
        }
        out.push(raw.slice(cursor..));
        out.retain(|b| !b.is_empty());
        out
    }

    /// Applies the edits to `raw`. With no edits this is `raw` itself, without a copy. Edits that
    /// overlap a previous one or fall outside `raw` are ignored.
    #[must_use]
    pub fn apply(mut self, raw: &Bytes) -> Bytes {
        if self.edits.is_empty() {
            return raw.clone();
        }
        self.edits.sort_by_key(|(r, _)| r.start);
        let mut out =
            BytesMut::with_capacity(raw.len() + self.edits.iter().map(|(_, b)| b.len()).sum::<usize>());
        let mut cursor = 0;
        for (range, bytes) in &self.edits {
            if range.start < cursor || range.end > raw.len() || range.start > range.end {
                continue;
            }
            out.extend_from_slice(raw.get(cursor..range.start).unwrap_or_default());
            out.extend_from_slice(bytes);
            cursor = range.end;
        }
        out.extend_from_slice(raw.get(cursor..).unwrap_or_default());
        out.freeze()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_share_the_body_and_match_the_copy() {
        let raw = Bytes::from_static(br#"{"model":"a","x":1}"#);
        let edits = || {
            let mut s = Splice::default();
            s.replace_span(9..12, Bytes::from_static(br#""bb""#));
            s.replace_span(18..18, Bytes::from_static(br#","y":2"#));
            s
        };
        let parts = edits().apply_parts(&raw);
        let joined: Vec<u8> = parts.iter().flat_map(|p| p.iter().copied()).collect();
        assert_eq!(joined, edits().apply(&raw).to_vec());
        assert_eq!(parts[0].as_ptr(), raw.as_ptr());
        assert_eq!(Splice::default().apply_parts(&raw), vec![raw.clone()]);
    }

    #[test]
    fn empty_splice_returns_the_same_bytes() {
        let raw = Bytes::from_static(b"{\"a\": 1}");
        let out = Splice::default().apply(&raw);
        assert_eq!(out.as_ptr(), raw.as_ptr());
    }

    #[test]
    fn replaces_spans_in_order() {
        let raw = Bytes::from_static(b"{\"model\": \"m\", \"x\": 1}");
        let mut s = Splice::default();
        s.replace_span(20..21, Bytes::from_static(b"2"));
        s.replace_span(10..13, Bytes::from_static(b"\"up-model\""));
        assert_eq!(&s.apply(&raw)[..], b"{\"model\": \"up-model\", \"x\": 2}");
    }
}
