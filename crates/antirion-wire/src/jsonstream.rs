//! Streams of JSON values: newline-delimited JSON (one value per line) and a streamed JSON array
//! (`[` value `,` value … `]`), each value dispatched as soon as its last byte arrives, whatever the
//! read boundaries.

use bytes::{Bytes, BytesMut};

/// One value of the stream: its bytes and its text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonEvent {
    pub raw: Bytes,
    pub data: String,
}

/// Why a stream of JSON values cannot be read further.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum JsonStreamError {
    #[error("a value is larger than the limit")]
    TooLarge,
    #[error("the stream is not well framed or not valid UTF-8")]
    Malformed,
    /// A clean end before the array was closed.
    #[error("the stream ended before its last value")]
    Truncated,
}

fn event(bytes: &[u8]) -> Result<JsonEvent, JsonStreamError> {
    let data = std::str::from_utf8(bytes).map_err(|_| JsonStreamError::Malformed)?;
    Ok(JsonEvent {
        raw: Bytes::copy_from_slice(bytes),
        data: data.to_owned(),
    })
}

/// Newline-delimited JSON: lines end at LF, a trailing CR is removed, blank lines are skipped, and a
/// final line without LF is a value at a clean end.
#[derive(Debug)]
pub struct NdjsonParser {
    pending: BytesMut,
    max: usize,
    lines: u64,
}

impl NdjsonParser {
    #[must_use]
    pub fn new(max_line: usize) -> NdjsonParser {
        NdjsonParser {
            pending: BytesMut::new(),
            max: max_line,
            lines: 0,
        }
    }

    /// Lines completed so far, values or blank.
    #[must_use]
    pub fn lines(&self) -> u64 {
        self.lines
    }

    fn line(&mut self, line: &[u8], out: &mut Vec<JsonEvent>) -> Result<(), JsonStreamError> {
        self.lines += 1;
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        out.push(event(line)?);
        Ok(())
    }

    pub fn push(&mut self, chunk: &[u8], out: &mut Vec<JsonEvent>) -> Result<(), JsonStreamError> {
        self.pending.extend_from_slice(chunk);
        while let Some(at) = self.pending.iter().position(|b| *b == b'\n') {
            if at > self.max {
                return Err(JsonStreamError::TooLarge);
            }
            let line = self.pending.split_to(at + 1);
            self.line(line.get(..at).unwrap_or_default(), out)?;
        }
        if self.pending.len() > self.max {
            return Err(JsonStreamError::TooLarge);
        }
        Ok(())
    }

    /// The last line, at a clean end.
    pub fn finish(&mut self) -> Result<Vec<JsonEvent>, JsonStreamError> {
        let mut out = Vec::new();
        let rest = std::mem::take(&mut self.pending);
        if !rest.is_empty() {
            self.line(&rest, &mut out)?;
        }
        Ok(out)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum State {
    /// Before `[`.
    Open,
    /// After `[` or `,`: a value, or `]` right after `[`.
    Value,
    /// Inside a value.
    In,
    /// After a value: `,` or `]`.
    After,
    /// After `]`: only whitespace.
    Closed,
}

/// A streamed JSON array, read incrementally: depth, strings and escapes are tracked across reads,
/// and each top-level object or array is dispatched when its closing bracket arrives.
#[derive(Debug)]
pub struct JsonArrayParser {
    state: State,
    value: BytesMut,
    depth: usize,
    in_string: bool,
    escaped: bool,
    max: usize,
    values: u64,
    /// The first value has been read (a `]` before it closes an empty array).
    seen: bool,
}

impl JsonArrayParser {
    #[must_use]
    pub fn new(max_value: usize) -> JsonArrayParser {
        JsonArrayParser {
            state: State::Open,
            value: BytesMut::new(),
            depth: 0,
            in_string: false,
            escaped: false,
            max: max_value,
            values: 0,
            seen: false,
        }
    }

    /// Values completed so far.
    #[must_use]
    pub fn values(&self) -> u64 {
        self.values
    }

    /// Whether the closing `]` was read.
    #[must_use]
    pub fn closed(&self) -> bool {
        self.state == State::Closed
    }

    pub fn push(&mut self, chunk: &[u8], out: &mut Vec<JsonEvent>) -> Result<(), JsonStreamError> {
        for &b in chunk {
            match self.state {
                State::Open | State::Value | State::After | State::Closed if b.is_ascii_whitespace() => {}
                State::Open if b == b'[' => self.state = State::Value,
                State::Value if b == b']' && !self.seen => self.state = State::Closed,
                State::Value if matches!(b, b'{' | b'[') => {
                    self.state = State::In;
                    self.depth = 1;
                    self.value.clear();
                    self.value.extend_from_slice(&[b]);
                }
                State::After if b == b',' => self.state = State::Value,
                State::After if b == b']' => self.state = State::Closed,
                State::In => self.byte(b, out)?,
                _ => return Err(JsonStreamError::Malformed),
            }
        }
        Ok(())
    }

    fn byte(&mut self, b: u8, out: &mut Vec<JsonEvent>) -> Result<(), JsonStreamError> {
        self.value.extend_from_slice(&[b]);
        if self.value.len() > self.max {
            return Err(JsonStreamError::TooLarge);
        }
        if self.in_string {
            match (self.escaped, b) {
                (true, _) => self.escaped = false,
                (false, b'\\') => self.escaped = true,
                (false, b'"') => self.in_string = false,
                _ => {}
            }
            return Ok(());
        }
        match b {
            b'"' => self.in_string = true,
            b'{' | b'[' => self.depth += 1,
            b'}' | b']' => {
                self.depth = self.depth.saturating_sub(1);
                if self.depth == 0 {
                    out.push(event(&self.value)?);
                    self.value.clear();
                    self.values += 1;
                    self.seen = true;
                    self.state = State::After;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// At a clean end: an array that was never closed is truncated.
    pub fn finish(&mut self) -> Result<(), JsonStreamError> {
        match self.state {
            State::Closed => Ok(()),
            _ => Err(JsonStreamError::Truncated),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ndjson(chunks: &[&[u8]]) -> Result<Vec<String>, JsonStreamError> {
        let mut p = NdjsonParser::new(64);
        let mut out = Vec::new();
        for c in chunks {
            p.push(c, &mut out)?;
        }
        out.extend(p.finish()?);
        Ok(out.into_iter().map(|e| e.data).collect())
    }

    fn array(input: &[u8], split: usize) -> Result<Vec<String>, JsonStreamError> {
        let mut p = JsonArrayParser::new(1 << 10);
        let mut out = Vec::new();
        for c in input.chunks(split.max(1)) {
            p.push(c, &mut out)?;
        }
        p.finish()?;
        Ok(out.into_iter().map(|e| e.data).collect())
    }

    #[test]
    fn ndjson_lines_are_split_invariant() {
        let whole = ndjson(&[b"{\"a\":1}\r\n\n{\"b\":2}\n{\"c\":3}"]).unwrap();
        assert_eq!(whole, ["{\"a\":1}", "{\"b\":2}", "{\"c\":3}"]);
        let bytes = b"{\"a\":1}\r\n\n{\"b\":2}\n{\"c\":3}";
        let split: Vec<&[u8]> = bytes.chunks(1).collect();
        assert_eq!(ndjson(&split).unwrap(), whole);
        assert_eq!(ndjson(&[&[b'x'; 65]]).unwrap_err(), JsonStreamError::TooLarge);
        assert_eq!(ndjson(&[b"\xff\n"]).unwrap_err(), JsonStreamError::Malformed);
    }

    #[test]
    fn arrays_dispatch_each_value_whatever_the_splits() {
        let input = b" [{\"t\":\"a]\\\"}\"},\r\n{\"n\":[1,{\"x\":2}]}\r\n] \n";
        let want = ["{\"t\":\"a]\\\"}\"}", "{\"n\":[1,{\"x\":2}]}"];
        for split in [1, 2, 3, 7, input.len()] {
            assert_eq!(array(input, split).unwrap(), want, "split {split}");
        }
        assert_eq!(array(b"[]", 1).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn array_framing_errors() {
        assert_eq!(array(b"{\"a\":1}", 4).unwrap_err(), JsonStreamError::Malformed);
        assert_eq!(array(b"[{\"a\":1}", 4).unwrap_err(), JsonStreamError::Truncated);
        assert_eq!(
            array(b"[{\"a\":1}] x", 4).unwrap_err(),
            JsonStreamError::Malformed
        );
        assert_eq!(array(b"[{\"a\":1},]", 4).unwrap_err(), JsonStreamError::Malformed);
        assert_eq!(array(b"[1]", 4).unwrap_err(), JsonStreamError::Malformed);
        let mut p = JsonArrayParser::new(8);
        let mut out = Vec::new();
        assert_eq!(
            p.push(b"[{\"aaaaaaaa\":1}]", &mut out).unwrap_err(),
            JsonStreamError::TooLarge
        );
    }
}
