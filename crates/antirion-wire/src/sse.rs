//! A server-sent events parser over bytes that keeps every event's original bytes, so a relay can
//! forward events unchanged.
//!
//! Lines end at CR LF, LF or a lone CR; a CR at the end of the input read so far waits for the next
//! byte. Two deliberate deviations from the browser algorithm: invalid UTF-8 is an error instead of
//! being replaced, and at a clean end the pending event is dispatched instead of discarded.

use bytes::{Bytes, BytesMut};

/// One dispatched event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SseEvent {
    /// The bytes to forward: every line since the previous dispatched event (comments and blank
    /// lines included) through this event's dispatching blank line. At a clean end, the missing line
    /// terminators are appended.
    pub raw: Bytes,
    /// The `event` field, when one was given.
    pub event: Option<String>,
    /// The `data` lines joined with LF.
    pub data: String,
}

/// Why the byte stream cannot be parsed further.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SseError {
    #[error("an event is larger than the limit")]
    TooLarge,
    #[error("an event line is not valid UTF-8")]
    Malformed,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Eol {
    Lf,
    CrLf,
    Cr,
}

impl Eol {
    fn bytes(self) -> &'static [u8] {
        match self {
            Eol::Lf => b"\n",
            Eol::CrLf => b"\r\n",
            Eol::Cr => b"\r",
        }
    }
}

/// The incremental parser. Feed it reads with [`SseParser::push`] and end it with
/// [`SseParser::finish`] at a clean end of the stream.
#[derive(Debug)]
pub struct SseParser {
    pending: BytesMut,
    /// Bytes of `pending` already split into lines.
    scanned: usize,
    max_event: usize,
    started: bool,
    event: Option<String>,
    data: Option<String>,
    lines: u64,
    bare_errors: bool,
}

impl SseParser {
    #[must_use]
    pub fn new(max_event: usize) -> SseParser {
        SseParser {
            pending: BytesMut::new(),
            scanned: 0,
            max_event,
            started: false,
            event: None,
            data: None,
            lines: 0,
            bare_errors: false,
        }
    }

    /// Also dispatches, as an event of its own, a line between events that is a bare JSON object
    /// with a top-level `error` member: a Gemini upstream can report an error this way inside a
    /// 2xx stream, where an SSE parser would ignore the line as an unknown field.
    #[must_use]
    pub fn with_bare_errors(mut self) -> SseParser {
        self.bare_errors = true;
        self
    }

    /// Complete lines seen so far, comments included: upstream activity.
    #[must_use]
    pub fn lines(&self) -> u64 {
        self.lines
    }

    /// Bytes buffered for the event in progress.
    #[must_use]
    pub fn buffered(&self) -> usize {
        self.pending.len()
    }

    /// Feeds one read; appends every event it completes to `out`.
    pub fn push(&mut self, chunk: &[u8], out: &mut Vec<SseEvent>) -> Result<(), SseError> {
        let mut chunk = chunk;
        if !self.started && !chunk.is_empty() {
            self.started = true;
            if let Some(rest) = chunk.strip_prefix(b"\xEF\xBB\xBF".as_slice()) {
                chunk = rest;
            }
        }
        self.pending.extend_from_slice(chunk);
        self.scan(false, out)?;
        if self.pending.len() > self.max_event {
            return Err(SseError::TooLarge);
        }
        Ok(())
    }

    /// Ends a cleanly finished stream: a partial final line is complete, and a pending event is
    /// dispatched with its missing terminators appended to its bytes.
    pub fn finish(&mut self) -> Result<Option<SseEvent>, SseError> {
        let mut out = Vec::new();
        self.scan(true, &mut out)?;
        if let Some(ev) = out.pop() {
            return Ok(Some(ev));
        }
        let partial = self.scanned < self.pending.len();
        if partial {
            let start = self.scanned;
            let end = self.pending.len();
            self.line(start, end)?;
            self.lines += 1;
        }
        if self.data.is_none() {
            return Ok(None);
        }
        let eol = self.style();
        if partial {
            self.pending.extend_from_slice(eol.bytes());
        }
        self.pending.extend_from_slice(eol.bytes());
        let raw = self.pending.split().freeze();
        self.scanned = 0;
        Ok(self.dispatch(raw))
    }

    /// The terminator style of the pending event: its first terminator, LF when it has none.
    fn style(&self) -> Eol {
        let bytes: &[u8] = self.pending.as_ref();
        match bytes.iter().position(|b| *b == b'\r' || *b == b'\n') {
            Some(i) if bytes.get(i) == Some(&b'\r') => {
                if bytes.get(i + 1) == Some(&b'\n') {
                    Eol::CrLf
                } else {
                    Eol::Cr
                }
            }
            _ => Eol::Lf,
        }
    }

    fn scan(&mut self, at_end: bool, out: &mut Vec<SseEvent>) -> Result<(), SseError> {
        loop {
            let Some(rest) = self.pending.get(self.scanned..) else {
                return Ok(());
            };
            let Some(i) = rest.iter().position(|b| *b == b'\r' || *b == b'\n') else {
                return Ok(());
            };
            let start = self.scanned;
            let line_end = start + i;
            let eol = match rest.get(i) {
                Some(b'\n') => Eol::Lf,
                _ => match rest.get(i + 1) {
                    Some(b'\n') => Eol::CrLf,
                    Some(_) => Eol::Cr,
                    None if at_end => Eol::Cr,
                    None => return Ok(()),
                },
            };
            let next = line_end + eol.bytes().len();
            self.scanned = next;
            self.lines += 1;
            if line_end == start {
                if self.data.is_some() {
                    let raw = self.pending.split_to(next).freeze();
                    self.scanned = 0;
                    if let Some(ev) = self.dispatch(raw) {
                        out.push(ev);
                    }
                } else {
                    self.event = None;
                }
            } else if self.line(start, line_end)? {
                let raw = self.pending.split_to(next).freeze();
                self.scanned = 0;
                if let Some(ev) = self.dispatch(raw) {
                    out.push(ev);
                }
            }
        }
    }

    /// Applies one line; `true` when it is a bare error object, an event complete on its own.
    fn line(&mut self, start: usize, end: usize) -> Result<bool, SseError> {
        let bytes = self.pending.get(start..end).ok_or(SseError::Malformed)?;
        let text = std::str::from_utf8(bytes).map_err(|_| SseError::Malformed)?;
        if text.starts_with(':') {
            return Ok(false);
        }
        if self.bare_errors && self.data.is_none() && bare_error(text) {
            self.event = None;
            self.data = Some(text.trim().to_owned());
            return Ok(true);
        }
        let (field, value) = match text.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (text, ""),
        };
        match field {
            "event" => self.event = Some(value.to_owned()),
            "data" => match &mut self.data {
                Some(d) => {
                    d.push('\n');
                    d.push_str(value);
                }
                None => self.data = Some(value.to_owned()),
            },
            _ => {}
        }
        Ok(false)
    }

    fn dispatch(&mut self, raw: Bytes) -> Option<SseEvent> {
        let data = self.data.take()?;
        Some(SseEvent {
            raw,
            event: self.event.take(),
            data,
        })
    }
}

/// A JSON object with a top-level `error` member.
fn bare_error(line: &str) -> bool {
    line.trim_start().starts_with('{')
        && serde_json::from_str::<serde_json::Value>(line).is_ok_and(|v| v.get("error").is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_error_objects_are_events_when_asked() {
        let input: &[u8] = b"data: {\"a\":1}\r\n\r\n{\"error\":{\"code\":500}}\r\ndata: {\"b\":2}\r\n\r\n";
        let plain = parse_joined(&[input]);
        assert_eq!(datas(&plain), ["{\"a\":1}", "{\"b\":2}"]);
        let mut p = SseParser::new(1 << 20).with_bare_errors();
        let mut out = Vec::new();
        for b in input.chunks(1) {
            p.push(b, &mut out).unwrap();
        }
        assert_eq!(
            datas(&out),
            ["{\"a\":1}", "{\"error\":{\"code\":500}}", "{\"b\":2}"]
        );
        let joined: Vec<u8> = out.iter().flat_map(|e| e.raw.to_vec()).collect();
        assert_eq!(joined, input);
        let mut p = SseParser::new(1 << 20).with_bare_errors();
        p.push(b"{\"other\":1}\n{\"error\":{}}", &mut out).unwrap();
        assert_eq!(p.finish().unwrap().unwrap().data, "{\"error\":{}}");
    }

    fn parse_all(input: &[&[u8]]) -> (Vec<SseEvent>, Option<SseEvent>) {
        let mut p = SseParser::new(1 << 20);
        let mut out = Vec::new();
        for chunk in input {
            p.push(chunk, &mut out).unwrap();
        }
        let tail = p.finish().unwrap();
        (out, tail)
    }

    fn parse_joined(input: &[&[u8]]) -> Vec<SseEvent> {
        let (mut evs, tail) = parse_all(input);
        evs.extend(tail);
        evs
    }

    fn datas(evs: &[SseEvent]) -> Vec<&str> {
        evs.iter().map(|e| e.data.as_str()).collect()
    }

    #[test]
    fn line_endings_and_splits_give_the_same_events() {
        let lf: &[u8] = b"data: a\n\nevent: x\ndata: b\n\ndata: c\n\n";
        let crlf: &[u8] = b"data: a\r\n\r\nevent: x\r\ndata: b\r\n\r\ndata: c\r\n\r\n";
        let cr: &[u8] = b"data: a\r\revent: x\rdata: b\r\rdata: c\r\r";
        for input in [lf, crlf, cr] {
            let evs = parse_joined(&[input]);
            assert_eq!(datas(&evs), ["a", "b", "c"]);
            assert_eq!(evs[1].event.as_deref(), Some("x"));
            let joined: Vec<u8> = evs.iter().flat_map(|e| e.raw.to_vec()).collect();
            assert_eq!(joined, input);
            for split in 0..input.len() {
                let (a, b) = input.split_at(split);
                assert_eq!(parse_joined(&[a, b]), evs, "split at {split}");
            }
            let bytes: Vec<&[u8]> = input.chunks(1).collect();
            assert_eq!(parse_joined(&bytes), evs);
        }
    }

    #[test]
    fn comments_ids_bom_and_multiline_data() {
        let input = b"\xEF\xBB\xBF: hi\nid: 1\nretry: 5000\ndata: one\ndata:two\nfoo: bar\n\n";
        let (evs, _) = parse_all(&[input]);
        assert_eq!(datas(&evs), ["one\ntwo"]);
        assert_eq!(&evs[0].raw[..], &input[3..]);
    }

    #[test]
    fn blank_lines_without_data_dispatch_nothing() {
        let (evs, tail) = parse_all(&[b"event: x\n\n: c\n\ndata: y\n\n"]);
        assert_eq!(datas(&evs), ["y"]);
        assert_eq!(evs[0].event, None);
        assert_eq!(&evs[0].raw[..], b"event: x\n\n: c\n\ndata: y\n\n");
        assert!(tail.is_none());
        let (evs, _) = parse_all(&[b"data:\n\n"]);
        assert_eq!(datas(&evs), [""]);
    }

    #[test]
    fn clean_end_completes_the_pending_event() {
        let (evs, tail) = parse_all(&[b"data: a\n\ndata: [DONE]"]);
        assert_eq!(datas(&evs), ["a"]);
        let tail = tail.unwrap();
        assert_eq!(tail.data, "[DONE]");
        assert_eq!(&tail.raw[..], b"data: [DONE]\n\n");
        let (_, tail) = parse_all(&[b"data: b\n"]);
        assert_eq!(&tail.unwrap().raw[..], b"data: b\n\n");
        let (_, tail) = parse_all(&[b"data: b\r\n"]);
        assert_eq!(&tail.unwrap().raw[..], b"data: b\r\n\r\n");
        let (_, tail) = parse_all(&[b"data: c\r"]);
        assert_eq!(&tail.unwrap().raw[..], b"data: c\r\r");
        let (_, tail) = parse_all(&[b": only a comment"]);
        assert!(tail.is_none());
    }

    #[test]
    fn utf8_split_across_reads_is_kept() {
        let input = "data: Hello 雪 café\n\n".as_bytes();
        let bytes: Vec<&[u8]> = input.chunks(1).collect();
        let (evs, _) = parse_all(&bytes);
        assert_eq!(datas(&evs), ["Hello 雪 café"]);
    }

    #[test]
    fn invalid_utf8_and_size_limit() {
        let mut p = SseParser::new(1024);
        let mut out = Vec::new();
        assert_eq!(p.push(b"data: \xff\n\n", &mut out), Err(SseError::Malformed));
        let mut p = SseParser::new(16);
        assert_eq!(p.push(&[b'x'; 17], &mut out), Err(SseError::TooLarge));
        let mut p = SseParser::new(16);
        assert!(p.push(b"data: 0123456\n\n", &mut out).is_ok());
    }
}
