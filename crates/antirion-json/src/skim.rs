//! One validating pass over a JSON text: strict RFC 8259 grammar, UTF-8, nesting depth, duplicate
//! member names in every object, and the position of the first error. It also records the byte spans
//! of the top-level members, which is all a byte-preserving rewrite needs.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt;
use std::ops::Range;

/// Limits applied while skimming.
#[derive(Copy, Clone, Debug)]
pub struct JsonLimits {
    /// Maximum nesting depth; the top-level value has depth 1.
    pub max_depth: usize,
}

impl Default for JsonLimits {
    fn default() -> Self {
        JsonLimits { max_depth: 128 }
    }
}

/// One step of a path into a JSON document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathSeg {
    Key(String),
    Index(usize),
}

/// What went wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JsonErrorKind {
    Empty,
    InvalidUtf8,
    Syntax(&'static str),
    TooDeep,
    DuplicateKey,
}

/// The first error found, with its 1-based line and column (in characters) and the path to it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub struct JsonError {
    pub kind: JsonErrorKind,
    pub line: usize,
    pub column: usize,
    pub path: Vec<PathSeg>,
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self.kind {
            JsonErrorKind::Empty => "empty document",
            JsonErrorKind::InvalidUtf8 => "invalid UTF-8",
            JsonErrorKind::Syntax(s) => s,
            JsonErrorKind::TooDeep => "nesting too deep",
            JsonErrorKind::DuplicateKey => "duplicate member name",
        };
        write!(f, "{what} at line {} column {}", self.line, self.column)
    }
}

/// A top-level member of an object document: its unescaped name, where its name starts, and the
/// byte span of its value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub name_at: usize,
    pub value: Range<usize>,
}

/// The result of a successful skim.
#[derive(Clone, Debug, Default)]
pub struct Skim {
    /// Top-level members, in document order; empty unless the document is an object.
    pub members: Vec<Member>,
    pub top_is_object: bool,
}

impl Skim {
    #[must_use]
    pub fn member(&self, name: &str) -> Option<&Member> {
        self.members.iter().find(|m| m.name == name)
    }
}

/// Validates `input` as one JSON text.
pub fn skim(input: &[u8], limits: &JsonLimits) -> Result<Skim, JsonError> {
    let text = match std::str::from_utf8(input) {
        Ok(t) => t,
        Err(e) => {
            let at = e.valid_up_to();
            return Err(error_at(input, at, JsonErrorKind::InvalidUtf8, Vec::new()));
        }
    };
    let mut p = Parser {
        text,
        s: text.as_bytes(),
        pos: 0,
        depth: 0,
        limits: *limits,
        path: Vec::new(),
        out: Skim::default(),
    };
    p.skip_ws();
    if p.pos >= p.s.len() {
        return Err(p.fail(JsonErrorKind::Empty));
    }
    p.out.top_is_object = p.peek() == Some(b'{');
    p.value()?;
    p.skip_ws();
    if p.pos < p.s.len() {
        return Err(p.fail(JsonErrorKind::Syntax("trailing characters after the document")));
    }
    Ok(p.out)
}

fn error_at(input: &[u8], at: usize, kind: JsonErrorKind, path: Vec<PathSeg>) -> JsonError {
    let consumed = input.get(..at).unwrap_or(input);
    let line = 1 + consumed.iter().filter(|b| **b == b'\n').count();
    let line_start = consumed.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    let column = 1 + String::from_utf8_lossy(consumed.get(line_start..).unwrap_or_default())
        .chars()
        .count();
    JsonError {
        kind,
        line,
        column,
        path,
    }
}

/// A step of the path being parsed; a member name is kept as the position of its opening quote and
/// unescaped only when an error reports it.
#[derive(Clone, Copy)]
enum Step {
    Key(usize),
    Index(usize),
}

/// The member names of one object seen so far: a short list scanned in order, or a set once the
/// object is large.
enum Seen<'a> {
    Few(Vec<Cow<'a, str>>),
    Many(HashSet<Cow<'a, str>>),
}

impl<'a> Seen<'a> {
    const FEW: usize = 16;

    /// Adds `name`; `false` when it was already there.
    fn insert(&mut self, name: Cow<'a, str>) -> bool {
        match self {
            Seen::Few(names) if names.contains(&name) => false,
            Seen::Few(names) if names.len() < Self::FEW => {
                names.push(name);
                true
            }
            Seen::Few(names) => {
                let mut set: HashSet<Cow<'a, str>> = names.drain(..).collect();
                set.insert(name);
                *self = Seen::Many(set);
                true
            }
            Seen::Many(set) => set.insert(name),
        }
    }
}

struct Parser<'a> {
    text: &'a str,
    s: &'a [u8],
    pos: usize,
    depth: usize,
    limits: JsonLimits,
    path: Vec<Step>,
    out: Skim,
}

impl<'a> Parser<'a> {
    fn fail(&self, kind: JsonErrorKind) -> JsonError {
        error_at(self.s, self.pos, kind, self.path())
    }

    /// The path to the current position, with member names unescaped.
    fn path(&self) -> Vec<PathSeg> {
        self.path
            .iter()
            .map(|step| match *step {
                Step::Index(i) => PathSeg::Index(i),
                Step::Key(at) => PathSeg::Key(self.name_at(at)),
            })
            .collect()
    }

    /// The unescaped member name whose opening quote is at `at`.
    fn name_at(&self, at: usize) -> String {
        let mut p = Parser {
            text: self.text,
            s: self.s,
            pos: at,
            depth: 0,
            limits: self.limits,
            path: Vec::new(),
            out: Skim::default(),
        };
        p.string(true)
            .ok()
            .flatten()
            .map(Cow::into_owned)
            .unwrap_or_default()
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, b: u8, what: &'static str) -> Result<(), JsonError> {
        if self.peek() == Some(b) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.fail(JsonErrorKind::Syntax(what)))
        }
    }

    fn value(&mut self) -> Result<(), JsonError> {
        match self.peek() {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => self.string(false).map(|_| ()),
            Some(b't') => self.literal(b"true"),
            Some(b'f') => self.literal(b"false"),
            Some(b'n') => self.literal(b"null"),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.fail(JsonErrorKind::Syntax("expected a value"))),
            None => Err(self.fail(JsonErrorKind::Syntax("unexpected end of input"))),
        }
    }

    fn enter(&mut self) -> Result<(), JsonError> {
        self.depth += 1;
        if self.depth > self.limits.max_depth {
            return Err(self.fail(JsonErrorKind::TooDeep));
        }
        Ok(())
    }

    fn object(&mut self) -> Result<(), JsonError> {
        self.enter()?;
        let top = self.depth == 1;
        self.pos += 1;
        let mut seen = Seen::Few(Vec::new());
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(());
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(self.fail(JsonErrorKind::Syntax("expected a member name")));
            }
            let name_at = self.pos;
            let name = self.string(true)?.unwrap_or_default();
            let kept = top.then(|| name.clone().into_owned());
            if !seen.insert(name) {
                self.path.push(Step::Key(name_at));
                let err = error_at(self.s, name_at, JsonErrorKind::DuplicateKey, self.path());
                return Err(err);
            }
            self.skip_ws();
            self.expect(b':', "expected ':' after a member name")?;
            self.skip_ws();
            let start = self.pos;
            self.path.push(Step::Key(name_at));
            self.value()?;
            self.path.pop();
            if let Some(name) = kept {
                self.out.members.push(Member {
                    name,
                    name_at,
                    value: start..self.pos,
                });
            }
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    self.depth -= 1;
                    return Ok(());
                }
                _ => return Err(self.fail(JsonErrorKind::Syntax("expected ',' or '}'"))),
            }
        }
    }

    fn array(&mut self) -> Result<(), JsonError> {
        self.enter()?;
        self.pos += 1;
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(());
        }
        let mut index = 0;
        loop {
            self.skip_ws();
            self.path.push(Step::Index(index));
            self.value()?;
            self.path.pop();
            index += 1;
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    self.depth -= 1;
                    return Ok(());
                }
                _ => return Err(self.fail(JsonErrorKind::Syntax("expected ',' or ']'"))),
            }
        }
    }

    fn literal(&mut self, word: &'static [u8]) -> Result<(), JsonError> {
        if self.s.get(self.pos..self.pos + word.len()) == Some(word) {
            self.pos += word.len();
            Ok(())
        } else {
            Err(self.fail(JsonErrorKind::Syntax("invalid literal")))
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.pos;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        self.pos - start
    }

    fn number(&mut self) -> Result<(), JsonError> {
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return Err(self.fail(JsonErrorKind::Syntax("invalid number"))),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if self.digits() == 0 {
                return Err(self.fail(JsonErrorKind::Syntax("invalid number")));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if self.digits() == 0 {
                return Err(self.fail(JsonErrorKind::Syntax("invalid number")));
            }
        }
        Ok(())
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let mut v = 0u32;
        for _ in 0..4 {
            let d = match self.peek() {
                Some(c @ b'0'..=b'9') => u32::from(c - b'0'),
                Some(c @ b'a'..=b'f') => u32::from(c - b'a' + 10),
                Some(c @ b'A'..=b'F') => u32::from(c - b'A' + 10),
                _ => return Err(self.fail(JsonErrorKind::Syntax("invalid \\u escape"))),
            };
            v = v * 16 + d;
            self.pos += 1;
        }
        Ok(v)
    }

    /// Validates a string; returns its unescaped value when `keep` is set, borrowed from the input
    /// when it has no escapes.
    fn string(&mut self, keep: bool) -> Result<Option<Cow<'a, str>>, JsonError> {
        self.pos += 1;
        let start = self.pos;
        let mut out: Option<String> = None;
        loop {
            let run_start = self.pos;
            while let Some(c) = self.peek() {
                if c == b'"' || c == b'\\' || c < 0x20 {
                    break;
                }
                self.pos += 1;
            }
            let text = self.text;
            if let (Some(o), Some(run)) = (out.as_mut(), text.get(run_start..self.pos)) {
                o.push_str(run);
            }
            match self.peek() {
                Some(b'"') => {
                    self.pos += 1;
                    if !keep {
                        return Ok(None);
                    }
                    return Ok(Some(match out {
                        Some(o) => Cow::Owned(o),
                        None => Cow::Borrowed(text.get(start..self.pos - 1).unwrap_or_default()),
                    }));
                }
                Some(b'\\') => {
                    if keep && out.is_none() {
                        out = Some(text.get(start..self.pos).unwrap_or_default().to_owned());
                    }
                    self.pos += 1;
                    let ch = match self.peek() {
                        Some(b'"') => '"',
                        Some(b'\\') => '\\',
                        Some(b'/') => '/',
                        Some(b'b') => '\u{8}',
                        Some(b'f') => '\u{c}',
                        Some(b'n') => '\n',
                        Some(b'r') => '\r',
                        Some(b't') => '\t',
                        Some(b'u') => {
                            self.pos += 1;
                            let ch = self.unicode_escape()?;
                            if let Some(o) = out.as_mut() {
                                o.push(ch);
                            }
                            continue;
                        }
                        _ => return Err(self.fail(JsonErrorKind::Syntax("invalid escape"))),
                    };
                    self.pos += 1;
                    if let Some(o) = out.as_mut() {
                        o.push(ch);
                    }
                }
                Some(_) => return Err(self.fail(JsonErrorKind::Syntax("control character in a string"))),
                None => return Err(self.fail(JsonErrorKind::Syntax("unterminated string"))),
            }
        }
    }

    fn unicode_escape(&mut self) -> Result<char, JsonError> {
        let hi = self.hex4()?;
        let code = if (0xD800..0xDC00).contains(&hi) {
            if self.s.get(self.pos..self.pos + 2) != Some(b"\\u") {
                return Err(self.fail(JsonErrorKind::Syntax("lone surrogate in \\u escape")));
            }
            self.pos += 2;
            let lo = self.hex4()?;
            if !(0xDC00..0xE000).contains(&lo) {
                return Err(self.fail(JsonErrorKind::Syntax("lone surrogate in \\u escape")));
            }
            0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
        } else if (0xDC00..0xE000).contains(&hi) {
            return Err(self.fail(JsonErrorKind::Syntax("lone surrogate in \\u escape")));
        } else {
            hi
        };
        char::from_u32(code).ok_or_else(|| self.fail(JsonErrorKind::Syntax("invalid \\u escape")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(s: &[u8]) -> JsonError {
        skim(s, &JsonLimits::default()).unwrap_err()
    }

    #[test]
    fn accepts_what_serde_json_accepts() {
        for doc in [
            &br#"{"a":[1,-2.5e3,true,false,null,{"b":"\u00e9\ud83d\ude00"}]}"#[..],
            b" [ ] ",
            b"0",
            b"\"x\"",
            br#"{"a":{"a":1}}"#,
        ] {
            assert!(
                skim(doc, &JsonLimits::default()).is_ok(),
                "{}",
                String::from_utf8_lossy(doc)
            );
            assert!(serde_json::from_slice::<serde_json::Value>(doc).is_ok());
        }
    }

    #[test]
    fn rejects_what_serde_json_rejects() {
        for doc in [
            &b"{\"a\":}"[..],
            b"[1,]",
            b"{'a':1}",
            b"01",
            b"1.",
            b"\"\\x\"",
            b"{\"a\" 1}",
            b"nul",
            b"\"\\ud800\"",
        ] {
            assert!(
                skim(doc, &JsonLimits::default()).is_err(),
                "{}",
                String::from_utf8_lossy(doc)
            );
        }
    }

    #[test]
    fn positions_are_one_based_lines_and_columns() {
        let e = err(b"{}x");
        assert_eq!((e.line, e.column), (1, 3));
        let e = err(b"{\n\"model\": \"m\",\n  x}");
        assert_eq!((e.line, e.column), (3, 3));
    }

    #[test]
    fn empty_and_whitespace_are_empty() {
        assert_eq!(err(b"").kind, JsonErrorKind::Empty);
        assert_eq!(err(b" \n").kind, JsonErrorKind::Empty);
    }

    #[test]
    fn invalid_utf8_is_reported() {
        assert_eq!(err(b"{\"a\":\"\xff\"}").kind, JsonErrorKind::InvalidUtf8);
    }

    #[test]
    fn duplicates_are_found_at_any_depth_with_their_path() {
        let e = err(br#"{"d":{"x":1},"e":[{"k":1,"k":2}]}"#);
        assert_eq!(e.kind, JsonErrorKind::DuplicateKey);
        assert_eq!(
            e.path,
            vec![
                PathSeg::Key("e".into()),
                PathSeg::Index(0),
                PathSeg::Key("k".into())
            ]
        );
        let escaped = err(br#"{"a":1,"\u0061":2}"#);
        assert_eq!(escaped.kind, JsonErrorKind::DuplicateKey);
    }

    #[test]
    fn duplicates_are_found_in_large_objects_and_escaped_names_are_unescaped() {
        let members: Vec<String> = (0..40).map(|i| format!("\"k{i}\":{i}")).collect();
        let doc = format!("{{{}}}", members.join(","));
        assert!(skim(doc.as_bytes(), &JsonLimits::default()).is_ok());
        let doc = format!("{{{},\"k\\u0033\":0}}", members.join(","));
        let e = err(doc.as_bytes());
        assert_eq!(e.kind, JsonErrorKind::DuplicateKey);
        assert_eq!(e.path, vec![PathSeg::Key("k3".into())]);
        let e = err(br#"{"a\nb":[1,}]}"#);
        assert_eq!(e.path, vec![PathSeg::Key("a\nb".into()), PathSeg::Index(1)]);
        let s = skim(br#"{"m\u006fdel":"x"}"#, &JsonLimits::default()).unwrap();
        assert!(s.member("model").is_some());
    }

    #[test]
    fn depth_is_limited() {
        let limits = JsonLimits { max_depth: 3 };
        assert!(skim(b"[[[1]]]", &limits).is_ok());
        assert_eq!(
            skim(b"[[[[1]]]]", &limits).unwrap_err().kind,
            JsonErrorKind::TooDeep
        );
    }

    #[test]
    fn top_level_member_spans() {
        let doc = br#"{"model": "m", "stream" : true}"#;
        let s = skim(doc, &JsonLimits::default()).unwrap();
        assert!(s.top_is_object);
        let m = s.member("model").unwrap();
        assert_eq!(&doc[m.value.clone()], b"\"m\"");
        assert_eq!(&doc[s.member("stream").unwrap().value.clone()], b"true");
    }
}
