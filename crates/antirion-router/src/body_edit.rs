//! A leaf's `upstream_body`: JSON Pointer edits of the final upstream body, after translation.
//! `set` writes each value at its pointer, creating missing objects on the way (`-`, or an index
//! one past the end, appends to an array); `remove` then deletes each pointer that exists.

use serde_json::{Map, Value};

/// A parsed JSON Pointer (RFC 6901): its reference tokens, unescaped.
pub type Pointer = Vec<String>;

/// Parses a JSON Pointer; `None` when it is not one.
#[must_use]
pub fn parse_pointer(p: &str) -> Option<Pointer> {
    if p.is_empty() {
        return Some(Vec::new());
    }
    let rest = p.strip_prefix('/')?;
    rest.split('/')
        .map(|t| {
            let mut out = String::with_capacity(t.len());
            let mut chars = t.chars();
            while let Some(c) = chars.next() {
                if c == '~' {
                    match chars.next() {
                        Some('0') => out.push('~'),
                        Some('1') => out.push('/'),
                        _ => return None,
                    }
                } else {
                    out.push(c);
                }
            }
            Some(out)
        })
        .collect()
}

/// The edits of one leaf.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BodyEdit {
    pub set: Vec<(Pointer, Value)>,
    pub remove: Vec<Pointer>,
}

/// Why an `upstream_body` member is not valid: the pointer that is not one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BadPointer {
    Set(String),
    Remove(usize),
}

impl BodyEdit {
    /// Reads an `upstream_body` member; `Ok(None)` when it edits nothing.
    pub fn from_value(v: Option<&Value>) -> Result<Option<BodyEdit>, BadPointer> {
        let Some(v) = v else { return Ok(None) };
        let mut edit = BodyEdit::default();
        for (k, value) in v.get("set").and_then(Value::as_object).into_iter().flatten() {
            let p = parse_pointer(k).ok_or_else(|| BadPointer::Set(k.clone()))?;
            edit.set.push((p, value.clone()));
        }
        for (i, r) in v
            .get("remove")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let p = r.as_str().and_then(parse_pointer).ok_or(BadPointer::Remove(i))?;
            edit.remove.push(p);
        }
        Ok((!edit.set.is_empty() || !edit.remove.is_empty()).then_some(edit))
    }

    /// Applies the edits to a body.
    pub fn apply(&self, body: &mut Value) {
        for (p, v) in &self.set {
            set(body, p, v.clone());
        }
        for p in &self.remove {
            remove(body, p);
        }
    }
}

fn index(token: &str, len: usize) -> Option<usize> {
    if token == "-" {
        return Some(len);
    }
    if token.len() > 1 && token.starts_with('0') {
        return None;
    }
    token.parse().ok()
}

fn set(target: &mut Value, pointer: &[String], value: Value) {
    let Some((first, rest)) = pointer.split_first() else {
        *target = value;
        return;
    };
    if target.is_null() {
        *target = Value::Object(Map::new());
    }
    match target {
        Value::Object(o) => {
            let slot = o.entry(first.clone()).or_insert(Value::Null);
            set(slot, rest, value);
        }
        Value::Array(a) => {
            let Some(i) = index(first, a.len()) else { return };
            if i == a.len() {
                a.push(Value::Null);
            }
            if let Some(slot) = a.get_mut(i) {
                set(slot, rest, value);
            }
        }
        _ => {}
    }
}

fn remove(target: &mut Value, pointer: &[String]) {
    let Some((last, parents)) = pointer.split_last() else {
        return;
    };
    let mut cur = target;
    for t in parents {
        let next = match cur {
            Value::Object(o) => o.get_mut(t.as_str()),
            Value::Array(a) => index(t, a.len()).and_then(|i| a.get_mut(i)),
            _ => None,
        };
        let Some(next) = next else { return };
        cur = next;
    }
    match cur {
        Value::Object(o) => {
            o.shift_remove(last.as_str());
        }
        Value::Array(a) => {
            if let Some(i) = index(last, a.len()).filter(|i| *i < a.len()) {
                a.remove(i);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn pointers_follow_rfc_6901() {
        assert_eq!(parse_pointer(""), Some(vec![]));
        assert_eq!(
            parse_pointer("/a~1b/c~0d"),
            Some(vec!["a/b".into(), "c~d".into()])
        );
        assert_eq!(parse_pointer("a"), None);
        assert_eq!(parse_pointer("/a~2"), None);
    }

    #[test]
    fn set_creates_parents_and_remove_ignores_missing_paths() {
        let edit = BodyEdit::from_value(Some(&json!({
            "set": {"/metadata/tier": "gold", "/stop/-": "x", "/messages/0/role": "user"},
            "remove": ["/top_k", "/absent/deep", "/stop/0"]
        })))
        .unwrap()
        .unwrap();
        let mut body = json!({"top_k": 5, "stop": ["a"], "messages": [{"role": "system"}]});
        edit.apply(&mut body);
        assert_eq!(
            body,
            json!({"stop": ["x"], "messages": [{"role": "user"}], "metadata": {"tier": "gold"}})
        );
        assert_eq!(
            BodyEdit::from_value(Some(&json!({"remove": ["nope"]}))),
            Err(BadPointer::Remove(0))
        );
        assert_eq!(BodyEdit::from_value(Some(&json!({}))), Ok(None));
    }
}
