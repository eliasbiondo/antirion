//! `multipart/form-data` bodies: locating parts by name and replacing one part's value while every
//! other byte of the body stays as the client sent it.

use std::ops::Range;

use bytes::{Bytes, BytesMut};

/// Why a body is not a well-formed `multipart/form-data` body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MultipartError {
    #[error("the body does not start with the boundary delimiter")]
    MissingDelimiter,
    #[error("the body ends before the closing boundary delimiter")]
    Unterminated,
    #[error("a part has no header block terminator")]
    MalformedHeaders,
}

/// One part of a form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub name: Option<String>,
    pub filename: Option<String>,
    /// The part's body bytes within the original body.
    pub value: Range<usize>,
}

/// A parsed form: the parts, as byte ranges into the body they were parsed from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Form {
    parts: Vec<Part>,
}

/// The boundary parameter of a `multipart/form-data` content type; `None` when the type is not
/// `multipart/form-data` or has no valid boundary.
#[must_use]
pub fn form_data_boundary(content_type: &str) -> Option<String> {
    let mut pieces = content_type.split(';');
    let essence = pieces.next()?.trim();
    if !essence.eq_ignore_ascii_case("multipart/form-data") {
        return None;
    }
    for piece in pieces {
        let Some((key, value)) = piece.split_once('=') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("boundary") {
            continue;
        }
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(value);
        return valid_boundary(value).then(|| value.to_owned());
    }
    None
}

/// A boundary is 1 to 70 characters from the set of RFC 2046, not ending with a space.
fn valid_boundary(b: &str) -> bool {
    !b.is_empty()
        && b.len() <= 70
        && !b.ends_with(' ')
        && b.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"'()+_,-./:=? ".contains(&c))
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from > haystack.len() {
        return None;
    }
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// Parses a form body delimited by `boundary`.
pub fn parse(body: &[u8], boundary: &str) -> Result<Form, MultipartError> {
    let dash = format!("--{boundary}");
    let delimiter = format!("\r\n--{boundary}");
    // The first delimiter may open the body directly or follow a preamble ending in CRLF.
    let first = if body.starts_with(dash.as_bytes()) {
        0
    } else {
        find(body, delimiter.as_bytes(), 0).ok_or(MultipartError::MissingDelimiter)? + 2
    };
    let mut pos = first + dash.len();
    let mut parts = Vec::new();
    loop {
        let rest = body.get(pos..).ok_or(MultipartError::Unterminated)?;
        if rest.starts_with(b"--") {
            return Ok(Form { parts });
        }
        // Transport padding (spaces and tabs) may follow a delimiter before its CRLF.
        let pad = rest.iter().take_while(|c| **c == b' ' || **c == b'\t').count();
        if !rest.get(pad..).is_some_and(|r| r.starts_with(b"\r\n")) {
            return Err(MultipartError::Unterminated);
        }
        let head_start = pos + pad + 2;
        let (headers, value_start) = if body.get(head_start..).is_some_and(|r| r.starts_with(b"\r\n")) {
            (&[][..], head_start + 2)
        } else {
            let end = find(body, b"\r\n\r\n", head_start).ok_or(MultipartError::MalformedHeaders)?;
            (body.get(head_start..end).unwrap_or_default(), end + 4)
        };
        let value_end = find(body, delimiter.as_bytes(), value_start).ok_or(MultipartError::Unterminated)?;
        let (name, filename) = disposition(headers);
        parts.push(Part {
            name,
            filename,
            value: value_start..value_end,
        });
        pos = value_end + delimiter.len();
    }
}

/// The `name` and `filename` parameters of the part's `Content-Disposition: form-data` header.
fn disposition(headers: &[u8]) -> (Option<String>, Option<String>) {
    for line in headers.split(|c| *c == b'\n') {
        let line = String::from_utf8_lossy(line.strip_suffix(b"\r").unwrap_or(line)).into_owned();
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("content-disposition") {
            return (param(value, "name"), param(value, "filename"));
        }
    }
    (None, None)
}

/// One parameter of a header value, quoted (with backslash escapes) or as a token.
fn param(value: &str, wanted: &str) -> Option<String> {
    let mut rest = value.split_once(';')?.1;
    loop {
        rest = rest.trim_start_matches([' ', '\t', ';']);
        if rest.is_empty() {
            return None;
        }
        let (key, after) = rest.split_once('=')?;
        let after = after.trim_start();
        let (val, next) = if let Some(quoted) = after.strip_prefix('"') {
            let mut out = String::new();
            let mut chars = quoted.char_indices();
            let mut end = quoted.len();
            while let Some((i, c)) = chars.next() {
                match c {
                    '\\' => {
                        if let Some((_, e)) = chars.next() {
                            out.push(e);
                        }
                    }
                    '"' => {
                        end = i + 1;
                        break;
                    }
                    _ => out.push(c),
                }
            }
            (out, quoted.get(end..).unwrap_or_default())
        } else {
            let end = after.find(';').unwrap_or(after.len());
            let (v, n) = after.split_at(end);
            (v.trim().to_owned(), n)
        };
        if key.trim().eq_ignore_ascii_case(wanted) {
            return Some(val);
        }
        rest = next;
    }
}

impl Form {
    #[must_use]
    pub fn parts(&self) -> &[Part] {
        &self.parts
    }

    fn named(&self, name: &str) -> Option<&Part> {
        self.parts.iter().find(|p| p.name.as_deref() == Some(name))
    }

    /// The value of the first part named `name`.
    #[must_use]
    pub fn field<'a>(&self, body: &'a [u8], name: &str) -> Option<&'a [u8]> {
        body.get(self.named(name)?.value.clone())
    }

    /// The body with only the value of the first part named `name` replaced.
    #[must_use]
    pub fn replace_field(&self, body: &[u8], name: &str, value: &[u8]) -> Option<Bytes> {
        let range = self.named(name)?.value.clone();
        let before = body.get(..range.start)?;
        let after = body.get(range.end..)?;
        let mut out = BytesMut::with_capacity(before.len() + value.len() + after.len());
        out.extend_from_slice(before);
        out.extend_from_slice(value);
        out.extend_from_slice(after);
        Some(out.freeze())
    }
}

/// Where an incremental scan stands.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ScanState {
    /// Before the first delimiter.
    Preamble,
    /// Just after a delimiter: the next bytes say whether a part or the end follows.
    Delimited(usize),
    /// Reading a part's header block, which starts at the offset.
    Headers(usize),
    /// Reading a part's value.
    Value {
        name: Option<String>,
        filename: Option<String>,
        start: usize,
    },
    Closed,
}

/// An incremental parser over a body that grows at its end: each [`Scanner::feed`] continues where
/// the last one stopped, so reading a body chunk by chunk costs one pass over it.
#[derive(Debug, Clone)]
pub struct Scanner {
    dash: Vec<u8>,
    delimiter: Vec<u8>,
    state: ScanState,
    /// Where the next search for a delimiter or header terminator starts.
    searched: usize,
    parts: Vec<Part>,
}

/// The part whose value is still arriving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenPart {
    pub name: Option<String>,
    pub filename: Option<String>,
    /// Where its value starts in the body.
    pub start: usize,
}

impl Scanner {
    #[must_use]
    pub fn new(boundary: &str) -> Scanner {
        Scanner {
            dash: format!("--{boundary}").into_bytes(),
            delimiter: format!("\r\n--{boundary}").into_bytes(),
            state: ScanState::Preamble,
            searched: 0,
            parts: Vec::new(),
        }
    }

    /// Continues the scan over `body`, which holds every byte fed before followed by new ones.
    pub fn feed(&mut self, body: &[u8]) -> Result<(), MultipartError> {
        loop {
            match std::mem::replace(&mut self.state, ScanState::Closed) {
                ScanState::Preamble => {
                    if body.len() < self.dash.len() {
                        self.state = ScanState::Preamble;
                        return Ok(());
                    }
                    if body.starts_with(&self.dash) {
                        self.state = ScanState::Delimited(self.dash.len());
                        continue;
                    }
                    match find(body, &self.delimiter, self.searched) {
                        Some(at) => self.state = ScanState::Delimited(at + self.delimiter.len()),
                        None => {
                            self.searched = body.len().saturating_sub(self.delimiter.len());
                            self.state = ScanState::Preamble;
                            return Ok(());
                        }
                    }
                }
                ScanState::Delimited(pos) => {
                    let rest = body.get(pos..).unwrap_or_default();
                    if rest.starts_with(b"--") {
                        self.state = ScanState::Closed;
                        return Ok(());
                    }
                    let pad = rest.iter().take_while(|c| **c == b' ' || **c == b'\t').count();
                    let after = rest.get(pad..).unwrap_or_default();
                    if after.len() < 2 || rest.len() < 2 {
                        self.state = ScanState::Delimited(pos);
                        return Ok(());
                    }
                    if !after.starts_with(b"\r\n") {
                        return Err(MultipartError::Unterminated);
                    }
                    let head = pos + pad + 2;
                    self.searched = head;
                    self.state = ScanState::Headers(head);
                }
                ScanState::Headers(head) => {
                    let rest = body.get(head..).unwrap_or_default();
                    if rest.len() < 2 {
                        self.state = ScanState::Headers(head);
                        return Ok(());
                    }
                    let (headers, start) = if rest.starts_with(b"\r\n") {
                        (&[][..], head + 2)
                    } else {
                        match find(body, b"\r\n\r\n", self.searched.max(head)) {
                            Some(end) => (body.get(head..end).unwrap_or_default(), end + 4),
                            None => {
                                self.searched = body.len().saturating_sub(3).max(head);
                                self.state = ScanState::Headers(head);
                                return Ok(());
                            }
                        }
                    };
                    let (name, filename) = disposition(headers);
                    self.searched = start;
                    self.state = ScanState::Value {
                        name,
                        filename,
                        start,
                    };
                }
                ScanState::Value {
                    name,
                    filename,
                    start,
                } => match find(body, &self.delimiter, self.searched.max(start)) {
                    Some(end) => {
                        self.parts.push(Part {
                            name,
                            filename,
                            value: start..end,
                        });
                        self.state = ScanState::Delimited(end + self.delimiter.len());
                    }
                    None => {
                        self.searched = body.len().saturating_sub(self.delimiter.len() - 1).max(start);
                        self.state = ScanState::Value {
                            name,
                            filename,
                            start,
                        };
                        return Ok(());
                    }
                },
                ScanState::Closed => {
                    self.state = ScanState::Closed;
                    return Ok(());
                }
            }
        }
    }

    /// Whether the closing delimiter was seen.
    #[must_use]
    pub fn closed(&self) -> bool {
        self.state == ScanState::Closed
    }

    /// The parts read whole so far.
    #[must_use]
    pub fn form(&self) -> Form {
        Form {
            parts: self.parts.clone(),
        }
    }

    /// The part being read, once its headers are complete.
    #[must_use]
    pub fn open_part(&self) -> Option<OpenPart> {
        match &self.state {
            ScanState::Value {
                name,
                filename,
                start,
            } => Some(OpenPart {
                name: name.clone(),
                filename: filename.clone(),
                start: *start,
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(parts: &[(&str, &[u8], Option<&str>)], boundary: &str) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, value, filename) in parts {
            out.extend_from_slice(
                format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"").as_bytes(),
            );
            if let Some(f) = filename {
                out.extend_from_slice(format!("; filename=\"{f}\"\r\nContent-Type: audio/wav").as_bytes());
            }
            out.extend_from_slice(b"\r\n\r\n");
            out.extend_from_slice(value);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        out
    }

    const AUDIO: &[u8] = b"RIFF\x00\r\n--b\r\n\xff\xfe\r\n\r\nbinary";

    #[test]
    fn boundary_parameter() {
        assert_eq!(
            form_data_boundary("multipart/form-data; boundary=abc123").as_deref(),
            Some("abc123")
        );
        assert_eq!(
            form_data_boundary("Multipart/Form-Data; charset=utf-8; Boundary=\"a b:c\"").as_deref(),
            Some("a b:c")
        );
        assert_eq!(form_data_boundary("multipart/form-data"), None);
        assert_eq!(form_data_boundary("multipart/form-data; boundary="), None);
        assert_eq!(form_data_boundary("multipart/mixed; boundary=x"), None);
        assert_eq!(form_data_boundary("application/json"), None);
    }

    #[test]
    fn parses_and_rewrites_with_model_first_or_last() {
        let b = "test-boundary-7f3a";
        for order in [0, 1] {
            let mut parts: Vec<(&str, &[u8], Option<&str>)> = vec![
                ("model", b"alias", None),
                ("file", AUDIO, Some("a.wav")),
                ("response_format", b"srt", None),
            ];
            if order == 1 {
                parts.swap(0, 1);
            }
            let body = encode(&parts, b);
            let form = parse(&body, b).unwrap();
            assert_eq!(form.parts().len(), 3);
            assert_eq!(form.field(&body, "model"), Some(&b"alias"[..]));
            assert_eq!(form.field(&body, "file"), Some(AUDIO));
            let file = form
                .parts()
                .iter()
                .find(|p| p.name.as_deref() == Some("file"))
                .unwrap();
            assert_eq!(file.filename.as_deref(), Some("a.wav"));

            let rewritten = form.replace_field(&body, "model", b"upstream-model-id").unwrap();
            let mut expected_parts = parts.clone();
            for p in &mut expected_parts {
                if p.0 == "model" {
                    p.1 = b"upstream-model-id";
                }
            }
            assert_eq!(&rewritten[..], &encode(&expected_parts, b)[..]);
            let again = parse(&rewritten, b).unwrap();
            assert_eq!(again.field(&rewritten, "file"), Some(AUDIO));
        }
    }

    #[test]
    fn preamble_padding_and_empty_values() {
        let body = b"ignored preamble\r\n--x  \r\nContent-Disposition: form-data; name=\"a\"\r\n\r\n\r\n--x\r\n\r\nno headers\r\n--x--epilogue";
        let form = parse(body, "x").unwrap();
        assert_eq!(form.field(body, "a"), Some(&b""[..]));
        assert_eq!(form.parts().get(1).map(|p| p.name.clone()), Some(None));
        assert_eq!(body.get(form.parts()[1].value.clone()), Some(&b"no headers"[..]));
    }

    #[test]
    fn quoted_parameters() {
        let (n, f) = disposition(b"content-disposition: form-data; filename=\"a\\\"b;c.wav\"; name=file");
        assert_eq!((n.as_deref(), f.as_deref()), (Some("file"), Some("a\"b;c.wav")));
    }

    #[test]
    fn malformed_bodies() {
        assert_eq!(
            parse(b"{\"model\":\"m\"}", "x"),
            Err(MultipartError::MissingDelimiter)
        );
        assert_eq!(
            parse(
                b"--x\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\nvalue",
                "x"
            ),
            Err(MultipartError::Unterminated)
        );
        assert_eq!(
            parse(b"--x\r\nContent-Disposition: form-data; name=\"a\"", "x"),
            Err(MultipartError::MalformedHeaders)
        );
        let empty = parse(b"--x--\r\n", "x").unwrap();
        assert!(empty.parts().is_empty());
        assert!(empty.replace_field(b"--x--\r\n", "model", b"m").is_none());
    }

    #[test]
    fn the_scanner_reads_a_body_fed_in_pieces_like_the_whole() {
        let body = encode(
            &[
                ("model", b"m", None),
                ("file", AUDIO, Some("a.wav")),
                ("purpose", b"batch", None),
            ],
            "zz-boundary",
        );
        let whole = parse(&body, "zz-boundary").unwrap();
        for step in [1usize, 2, 3, 7, 64] {
            let mut sc = Scanner::new("zz-boundary");
            let mut end = 0;
            while end < body.len() {
                end = (end + step).min(body.len());
                sc.feed(&body[..end]).unwrap();
            }
            assert!(sc.closed(), "step {step}");
            assert_eq!(sc.form(), whole, "step {step}");
        }
        let mut sc = Scanner::new("zz-boundary");
        let cut = body.windows(6).position(|w| w == b"binary").unwrap();
        sc.feed(&body[..cut]).unwrap();
        assert!(!sc.closed());
        assert_eq!(sc.form().parts().len(), 1);
        let open = sc.open_part().unwrap();
        assert_eq!(open.name.as_deref(), Some("file"));
        assert_eq!(open.filename.as_deref(), Some("a.wav"));
        let mut bad = Scanner::new("b");
        assert_eq!(bad.feed(b"--bxx"), Err(MultipartError::Unterminated));
    }
}
