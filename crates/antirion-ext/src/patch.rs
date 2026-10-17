//! JSON Patch (RFC 6902) applied atomically: every operation succeeds on a copy, or nothing changes.

use serde_json::Value;

fn unescape(token: &str) -> String {
    token.replace("~1", "/").replace("~0", "~")
}

/// The parent pointer and last token of `path`.
fn split(path: &str) -> Result<(&str, String), String> {
    let (parent, last) = path
        .rsplit_once('/')
        .ok_or_else(|| format!("{path:?} is not a JSON pointer"))?;
    Ok((parent, unescape(last)))
}

fn array_index(token: &str, len: usize, insert: bool) -> Result<usize, String> {
    if insert && token == "-" {
        return Ok(len);
    }
    let i: usize = token
        .parse()
        .map_err(|_| format!("{token:?} is not an array index"))?;
    let max = if insert { len } else { len.saturating_sub(1) };
    if (token.len() > 1 && token.starts_with('0')) || i > max || (!insert && len == 0) {
        return Err(format!("array index {token} is out of range"));
    }
    Ok(i)
}

fn add(doc: &mut Value, path: &str, value: Value) -> Result<(), String> {
    if path.is_empty() {
        *doc = value;
        return Ok(());
    }
    let (parent, last) = split(path)?;
    match doc.pointer_mut(parent) {
        Some(Value::Object(o)) => {
            o.insert(last, value);
            Ok(())
        }
        Some(Value::Array(a)) => {
            let i = array_index(&last, a.len(), true)?;
            a.insert(i, value);
            Ok(())
        }
        _ => Err(format!("the parent of {path:?} does not exist")),
    }
}

fn remove(doc: &mut Value, path: &str) -> Result<Value, String> {
    let (parent, last) = split(path)?;
    match doc.pointer_mut(parent) {
        Some(Value::Object(o)) => o.remove(&last).ok_or_else(|| format!("{path:?} does not exist")),
        Some(Value::Array(a)) => {
            let i = array_index(&last, a.len(), false)?;
            Ok(a.remove(i))
        }
        _ => Err(format!("{path:?} does not exist")),
    }
}

fn op_path<'a>(op: &'a Value, key: &str) -> Result<&'a str, String> {
    let p = op
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("the operation has no `{key}`"))?;
    if !p.is_empty() && !p.starts_with('/') {
        return Err(format!("{p:?} is not a JSON pointer"));
    }
    Ok(p)
}

fn apply_one(doc: &mut Value, op: &Value) -> Result<(), String> {
    let path = op_path(op, "path")?;
    let value = || op.get("value").cloned().ok_or("the operation has no `value`");
    match op.get("op").and_then(Value::as_str) {
        Some("add") => add(doc, path, value()?),
        Some("remove") => remove(doc, path).map(|_| ()),
        Some("replace") => {
            let v = value()?;
            let slot = doc
                .pointer_mut(path)
                .ok_or_else(|| format!("{path:?} does not exist"))?;
            *slot = v;
            Ok(())
        }
        Some("move") => {
            let from = op_path(op, "from")?;
            if path.starts_with(&format!("{from}/")) {
                return Err("a value cannot move into itself".to_owned());
            }
            let v = remove(doc, from)?;
            add(doc, path, v)
        }
        Some("copy") => {
            let from = op_path(op, "from")?;
            let v = doc
                .pointer(from)
                .cloned()
                .ok_or_else(|| format!("{from:?} does not exist"))?;
            add(doc, path, v)
        }
        Some("test") => {
            if doc.pointer(path) == Some(&value()?) {
                Ok(())
            } else {
                Err(format!("test of {path:?} failed"))
            }
        }
        _ => Err("unknown patch operation".to_owned()),
    }
}

/// Every path an operation writes or reads from.
pub(crate) fn touched(op: &Value) -> impl Iterator<Item = &str> {
    ["path", "from"]
        .into_iter()
        .filter_map(|k| op.get(k).and_then(Value::as_str))
}

/// Applies `ops` to a copy of `doc`; the patched document, or why the patch fails.
pub fn apply(doc: &Value, ops: &[Value]) -> Result<Value, String> {
    let mut out = doc.clone();
    for op in ops {
        apply_one(&mut out, op)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn operations_apply_atomically() {
        let doc = json!({"messages": [{"role": "user", "content": "hi"}]});
        let out = apply(
            &doc,
            &[
                json!({"op": "add", "path": "/messages/0", "value": {"role": "system", "content": "s"}}),
                json!({"op": "add", "path": "/messages/-", "value": {"role": "user", "content": "m"}}),
                json!({"op": "replace", "path": "/messages/1/content", "value": "x"}),
            ],
        )
        .unwrap();
        assert_eq!(out["messages"][0]["role"], "system");
        assert_eq!(out["messages"][1]["content"], "x");
        assert_eq!(out["messages"][2]["content"], "m");
        assert!(apply(&doc, &[json!({"op": "remove", "path": "/does/not/exist"})]).is_err());
        assert!(apply(&doc, &[json!({"op": "explode", "path": "/a"})]).is_err());
    }
}
