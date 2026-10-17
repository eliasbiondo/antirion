//! YAML 1.2 (core schema) into a JSON value, strictly: a duplicate key at any depth is an error naming
//! both lines, custom tags are rejected, and `yes`/`no`/`on`/`off` stay strings. Anchors, aliases and
//! merge keys (`<<`) are supported, with a bound on alias expansion.

use std::borrow::Cow;
use std::collections::HashMap;

use saphyr_parser::{Event, Parser, ScalarStyle, ScanError, Span, Tag};
use serde_json::{Map, Number, Value};

use crate::DocPath;

/// Upper bound on the number of values a document may expand to through aliases.
const MAX_NODES: usize = 1_000_000;

#[derive(Debug)]
pub(crate) struct YamlError {
    pub(crate) path: DocPath,
    pub(crate) message: String,
    pub(crate) duplicate: bool,
}

impl YamlError {
    fn at(path: &DocPath, message: impl Into<String>) -> YamlError {
        YamlError {
            path: path.clone(),
            message: message.into(),
            duplicate: false,
        }
    }
}

type Item<'a> = Result<(Event<'a>, Span), ScanError>;

struct Builder<'a> {
    events: Parser<'a, saphyr_parser::StrInput<'a>>,
    anchors: HashMap<usize, Value>,
    nodes: usize,
}

/// Parses one YAML document. An empty stream is `null`.
pub(crate) fn to_json(text: &str) -> Result<Value, YamlError> {
    let mut b = Builder {
        events: Parser::new_from_str(text),
        anchors: HashMap::new(),
        nodes: 0,
    };
    let root = DocPath::root();
    match b.next(&root)? {
        (Event::StreamStart, _) => {}
        _ => return Err(YamlError::at(&root, "malformed YAML stream")),
    }
    let value = match b.next(&root)? {
        (Event::StreamEnd, _) => return Ok(Value::Null),
        (Event::DocumentStart(_), _) => {
            let (ev, span) = b.next(&root)?;
            b.node(ev, span, &root)?
        }
        _ => return Err(YamlError::at(&root, "malformed YAML stream")),
    };
    match b.next(&root)? {
        (Event::DocumentEnd, _) => {}
        _ => return Err(YamlError::at(&root, "malformed YAML document")),
    }
    match b.next(&root)? {
        (Event::StreamEnd, _) => Ok(value),
        _ => Err(YamlError::at(&root, "the file holds more than one YAML document")),
    }
}

impl<'a> Builder<'a> {
    fn next(&mut self, path: &DocPath) -> Result<(Event<'a>, Span), YamlError> {
        let item: Option<Item<'a>> = self.events.next();
        match item {
            Some(Ok(pair)) => Ok(pair),
            Some(Err(e)) => Err(YamlError::at(
                path,
                format!(
                    "{} at line {} column {}",
                    e.info(),
                    e.marker().line(),
                    e.marker().col() + 1
                ),
            )),
            None => Err(YamlError::at(path, "unexpected end of YAML")),
        }
    }

    fn count(&mut self, n: usize, path: &DocPath) -> Result<(), YamlError> {
        self.nodes = self.nodes.saturating_add(n);
        if self.nodes > MAX_NODES {
            return Err(YamlError::at(
                path,
                "the document expands to too many values through aliases",
            ));
        }
        Ok(())
    }

    fn node(&mut self, ev: Event<'a>, span: Span, path: &DocPath) -> Result<Value, YamlError> {
        match ev {
            Event::Scalar(text, style, anchor, tag) => {
                check_tag(tag.as_deref(), path, span)?;
                let force_string = tag.as_deref().is_some_and(|t| t.suffix == "str");
                let v = if style == ScalarStyle::Plain && !force_string {
                    resolve_plain(&text).ok_or_else(|| {
                        YamlError::at(
                            path,
                            format!("unsupported number {text:?} at line {}", span.start.line()),
                        )
                    })?
                } else {
                    Value::String(text.into_owned())
                };
                self.count(1, path)?;
                self.remember(anchor, &v);
                Ok(v)
            }
            Event::SequenceStart(anchor, tag) => {
                check_tag(tag.as_deref(), path, span)?;
                let mut items = Vec::new();
                loop {
                    let (ev, span) = self.next(path)?;
                    if matches!(ev, Event::SequenceEnd) {
                        break;
                    }
                    let item_path = path.index(items.len());
                    items.push(self.node(ev, span, &item_path)?);
                }
                self.count(1, path)?;
                let v = Value::Array(items);
                self.remember(anchor, &v);
                Ok(v)
            }
            Event::MappingStart(anchor, tag) => {
                check_tag(tag.as_deref(), path, span)?;
                let v = Value::Object(self.mapping(path)?);
                self.count(1, path)?;
                self.remember(anchor, &v);
                Ok(v)
            }
            Event::Alias(id) => {
                let v = self.anchors.get(&id).cloned().ok_or_else(|| {
                    YamlError::at(path, format!("unknown alias at line {}", span.start.line()))
                })?;
                self.count(size_of_value(&v), path)?;
                Ok(v)
            }
            _ => Err(YamlError::at(
                path,
                format!("unexpected YAML structure at line {}", span.start.line()),
            )),
        }
    }

    fn mapping(&mut self, path: &DocPath) -> Result<Map<String, Value>, YamlError> {
        let mut out = Map::new();
        let mut lines: HashMap<String, usize> = HashMap::new();
        let mut merged: Vec<Map<String, Value>> = Vec::new();
        loop {
            let (ev, span) = self.next(path)?;
            let (key, merge) = match ev {
                Event::MappingEnd => break,
                Event::Scalar(text, style, _, tag) => {
                    check_tag(tag.as_deref(), path, span)?;
                    let merge = style == ScalarStyle::Plain && text == "<<";
                    (text.into_owned(), merge)
                }
                _ => {
                    return Err(YamlError::at(
                        path,
                        format!("mapping keys must be scalars (line {})", span.start.line()),
                    ));
                }
            };
            let line = span.start.line();
            let (vev, vspan) = self.next(path)?;
            if merge {
                let source = self.node(vev, vspan, path)?;
                match source {
                    Value::Object(m) => merged.push(m),
                    Value::Array(items) => {
                        for item in items {
                            match item {
                                Value::Object(m) => merged.push(m),
                                _ => {
                                    return Err(YamlError::at(
                                        path,
                                        format!("merge key at line {line} needs mappings"),
                                    ));
                                }
                            }
                        }
                    }
                    _ => {
                        return Err(YamlError::at(
                            path,
                            format!("merge key at line {line} needs a mapping"),
                        ));
                    }
                }
                continue;
            }
            let key_path = path.key(&key);
            if let Some(first) = lines.get(&key) {
                return Err(YamlError {
                    path: key_path,
                    message: format!("duplicate key {key:?} at lines {first} and {line}"),
                    duplicate: true,
                });
            }
            lines.insert(key.clone(), line);
            let value = self.node(vev, vspan, &key_path)?;
            out.insert(key, value);
        }
        for source in merged {
            for (k, v) in source {
                out.entry(k).or_insert(v);
            }
        }
        Ok(out)
    }

    fn remember(&mut self, anchor: usize, v: &Value) {
        if anchor != 0 {
            self.anchors.insert(anchor, v.clone());
        }
    }
}

fn check_tag(tag: Option<&Tag>, path: &DocPath, span: Span) -> Result<(), YamlError> {
    match tag {
        None => Ok(()),
        Some(t) if t.is_yaml_core_schema() => Ok(()),
        Some(t) => Err(YamlError::at(
            path,
            format!(
                "custom tag {}{} at line {} is not allowed",
                t.handle,
                t.suffix,
                span.start.line()
            ),
        )),
    }
}

fn size_of_value(v: &Value) -> usize {
    match v {
        Value::Array(a) => 1 + a.iter().map(size_of_value).sum::<usize>(),
        Value::Object(o) => 1 + o.values().map(size_of_value).sum::<usize>(),
        _ => 1,
    }
}

/// Resolves a plain scalar with the YAML 1.2 core schema. `None` for non-finite floats, which JSON
/// cannot hold.
fn resolve_plain(s: &str) -> Option<Value> {
    match s {
        "" | "~" | "null" | "Null" | "NULL" => return Some(Value::Null),
        "true" | "True" | "TRUE" => return Some(Value::Bool(true)),
        "false" | "False" | "FALSE" => return Some(Value::Bool(false)),
        _ => {}
    }
    if let Some(n) = core_int(s) {
        return Some(n);
    }
    if is_core_float(s) {
        let lower = s.to_ascii_lowercase();
        if lower.contains("inf") || lower.contains("nan") {
            return None;
        }
        return s
            .parse::<f64>()
            .ok()
            .and_then(Number::from_f64)
            .map(Value::Number);
    }
    Some(Value::String(Cow::Borrowed(s).into_owned()))
}

fn core_int(s: &str) -> Option<Value> {
    let (digits, radix) = if let Some(h) = s.strip_prefix("0x") {
        (h, 16)
    } else if let Some(o) = s.strip_prefix("0o") {
        (o, 8)
    } else {
        let body = s.strip_prefix(['-', '+']).unwrap_or(s);
        if body.is_empty() || !body.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        return s
            .parse::<i64>()
            .map(Value::from)
            .ok()
            .or_else(|| s.trim_start_matches('+').parse::<u64>().ok().map(Value::from))
            .or_else(|| {
                s.parse::<f64>()
                    .ok()
                    .and_then(Number::from_f64)
                    .map(Value::Number)
            });
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    u64::from_str_radix(digits, radix).ok().map(Value::from)
}

fn is_core_float(s: &str) -> bool {
    let body = s.strip_prefix(['-', '+']).unwrap_or(s);
    if matches!(body, ".inf" | ".Inf" | ".INF") || matches!(s, ".nan" | ".NaN" | ".NAN") {
        return true;
    }
    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(i) => (&body[..i], Some(&body[i + 1..])),
        None => (body, None),
    };
    let mantissa_ok = match mantissa.split_once('.') {
        Some((int, frac)) => {
            (!int.is_empty() || !frac.is_empty())
                && int.bytes().all(|b| b.is_ascii_digit())
                && frac.bytes().all(|b| b.is_ascii_digit())
                && !(int.is_empty() && frac.is_empty())
        }
        None => !mantissa.is_empty() && mantissa.bytes().all(|b| b.is_ascii_digit()),
    };
    let exponent_ok = exponent.is_none_or(|e| {
        let e = e.strip_prefix(['-', '+']).unwrap_or(e);
        !e.is_empty() && e.bytes().all(|b| b.is_ascii_digit())
    });
    mantissa_ok && exponent_ok
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn core_schema_scalars() {
        let v = to_json("a: yes\nb: on\nc: true\nd: 12\ne: 1.5\nf: ~\ng: '12'\nh: 0x1F\ni: 1e3\n").unwrap();
        assert_eq!(
            v,
            json!({"a": "yes", "b": "on", "c": true, "d": 12, "e": 1.5, "f": null, "g": "12", "h": 31, "i": 1000.0})
        );
    }

    #[test]
    fn duplicate_keys_name_both_lines_and_the_path() {
        let err = to_json("routes:\n  a:\n    tree: x\n  a:\n    tree: y\n").unwrap_err();
        assert!(err.duplicate);
        assert_eq!(err.path.to_string(), "routes.a");
        assert!(
            err.message.contains('2') && err.message.contains('4'),
            "{}",
            err.message
        );
    }

    #[test]
    fn custom_tags_are_rejected() {
        assert!(to_json("m: !custom {a: 1}\n").is_err());
        assert_eq!(to_json("m: !!str 12\n").unwrap(), json!({"m": "12"}));
    }

    #[test]
    fn anchors_aliases_and_merge_keys() {
        let v = to_json("base: &b {x: 1, y: 2}\nother:\n  <<: *b\n  y: 3\nlist: [*b]\n").unwrap();
        assert_eq!(v["other"], json!({"x": 1, "y": 3}));
        assert_eq!(v["list"][0], json!({"x": 1, "y": 2}));
    }

    #[test]
    fn alias_expansion_is_bounded() {
        let mut doc = String::from("a0: &a0 [x, x, x, x, x, x, x, x, x, x]\n");
        for i in 1..9 {
            let prev = format!("*a{}", i - 1);
            doc.push_str(&format!("a{i}: &a{i} [{}]\n", vec![prev; 10].join(", ")));
        }
        assert!(to_json(&doc).is_err());
    }

    #[test]
    fn empty_is_null_and_two_documents_are_refused() {
        assert_eq!(to_json("").unwrap(), Value::Null);
        assert!(to_json("a: 1\n---\nb: 2\n").is_err());
    }
}
