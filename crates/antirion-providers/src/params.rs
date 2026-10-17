//! `params` blocks: JSON Pointer removals, defaults and overrides applied to the upstream body.

use antirion_config::{ConfigError, DetailCode, DocPath};
use serde_json::{Map, Value};

/// One validated pointer: its source text and its unescaped reference tokens.
#[derive(Clone, Debug, PartialEq)]
pub struct Pointer {
    pub text: String,
    tokens: Vec<String>,
}

/// A compiled `params` block. Entries of each list are in ascending byte order of the pointer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Params {
    pub remove: Vec<Pointer>,
    pub defaults: Vec<(Pointer, Value)>,
    pub overrides: Vec<(Pointer, Value)>,
}

/// The pointers that changed a body, for the request log.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    pub overrides: Vec<String>,
    pub removed: Vec<String>,
}

impl Applied {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.overrides.is_empty() && self.removed.is_empty()
    }
}

fn parse_pointer(text: &str) -> Result<Pointer, &'static str> {
    let Some(rest) = text.strip_prefix('/') else {
        return Err("a JSON Pointer starts with \"/\"");
    };
    let tokens: Vec<String> = rest
        .split('/')
        .map(|t| t.replace("~1", "/").replace("~0", "~"))
        .collect();
    if tokens.iter().any(|t| t == "-") {
        return Err("the \"-\" array token is not allowed");
    }
    if matches!(tokens.as_slice(), [one] if one == "model" || one == "stream") {
        return Err("the upstream model and streaming are not parameters");
    }
    Ok(Pointer {
        text: text.to_owned(),
        tokens,
    })
}

impl Params {
    /// Compiles a `params` block at `at`, collecting every invalid entry.
    pub fn compile(block: Option<&Value>, at: &DocPath, errors: &mut Vec<ConfigError>) -> Params {
        let mut out = Params::default();
        let Some(block) = block else { return out };
        let mut pointer = |text: &str, list: &str| match parse_pointer(text) {
            Ok(p) => Some(p),
            Err(why) => {
                errors.push(ConfigError::new(
                    &at.key(list).key(text),
                    DetailCode::InvalidFormat,
                    why,
                ));
                None
            }
        };
        for v in block
            .get("remove")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(p) = v.as_str().and_then(|t| pointer(t, "remove")) {
                out.remove.push(p);
            }
        }
        for (list, slot) in [("defaults", &mut out.defaults), ("overrides", &mut out.overrides)] {
            for (k, v) in block.get(list).and_then(Value::as_object).into_iter().flatten() {
                if let Some(p) = pointer(k, list) {
                    slot.push((p, v.clone()));
                }
            }
        }
        out.remove.sort_by(|a, b| a.text.cmp(&b.text));
        out.defaults.sort_by(|a, b| a.0.text.cmp(&b.0.text));
        out.overrides.sort_by(|a, b| a.0.text.cmp(&b.0.text));
        out
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.remove.is_empty() && self.defaults.is_empty() && self.overrides.is_empty()
    }

    /// Applies the block: removals, then defaults, then overrides. An entry whose parent is not a
    /// container is skipped.
    pub fn apply(&self, body: &mut Value, applied: &mut Applied) {
        for p in &self.remove {
            if remove(body, &p.tokens) {
                applied.removed.push(p.text.clone());
            }
        }
        for (p, v) in &self.defaults {
            set(body, &p.tokens, v, false);
        }
        for (p, v) in &self.overrides {
            if set(body, &p.tokens, v, true) {
                applied.overrides.push(p.text.clone());
            }
        }
    }
}

fn parent_mut<'a>(body: &'a mut Value, tokens: &[String], create: bool) -> Option<&'a mut Value> {
    let (_, parents) = tokens.split_last()?;
    let mut cur = body;
    for t in parents {
        cur = match cur {
            Value::Object(m) => {
                if create && !m.contains_key(t) {
                    m.insert(t.clone(), Value::Object(Map::new()));
                }
                m.get_mut(t)?
            }
            Value::Array(a) => a.get_mut(t.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

fn remove(body: &mut Value, tokens: &[String]) -> bool {
    let Some(last) = tokens.last() else { return false };
    match parent_mut(body, tokens, false) {
        Some(Value::Object(m)) => m.remove(last).is_some(),
        Some(Value::Array(a)) => match last.parse::<usize>() {
            Ok(i) if i < a.len() => {
                a.remove(i);
                true
            }
            _ => false,
        },
        _ => false,
    }
}

fn set(body: &mut Value, tokens: &[String], v: &Value, replace: bool) -> bool {
    let Some(last) = tokens.last() else { return false };
    match parent_mut(body, tokens, true) {
        Some(Value::Object(m)) => {
            if !replace && m.contains_key(last) {
                return false;
            }
            m.insert(last.clone(), v.clone());
            true
        }
        Some(Value::Array(a)) => match last.parse::<usize>().ok().and_then(|i| a.get_mut(i)) {
            Some(slot) if replace => {
                *slot = v.clone();
                true
            }
            _ => false,
        },
        _ => {
            tracing::debug!(
                pointer = tokens.join("/"),
                "params entry skipped: parent is not a container"
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn params(v: Value) -> (Params, Vec<ConfigError>) {
        let mut errors = Vec::new();
        let p = Params::compile(Some(&v), &DocPath::root().key("params"), &mut errors);
        (p, errors)
    }

    #[test]
    fn order_and_semantics() {
        let (p, errors) = params(json!({
            "remove": ["/temperature", "/missing"],
            "defaults": {"/temperature": 0.2, "/top_p": 0.5, "/metadata/team": "s"},
            "overrides": {"/top_p": 0.7, "/messages/role": "x", "/n": null},
        }));
        assert!(errors.is_empty());
        let mut body = json!({"temperature": 0.9, "messages": [{"role": "user"}]});
        let mut applied = Applied::default();
        p.apply(&mut body, &mut applied);
        assert_eq!(
            body,
            json!({"temperature": 0.2, "top_p": 0.7, "metadata": {"team": "s"}, "n": null,
                   "messages": [{"role": "user"}]})
        );
        assert_eq!(applied.removed, ["/temperature"]);
        assert_eq!(applied.overrides, ["/n", "/top_p"]);
    }

    #[test]
    fn invalid_pointers() {
        for bad in [
            json!({"overrides": {"/stream": true}}),
            json!({"remove": ["/model"]}),
            json!({"overrides": {"/messages/-": 1}}),
            json!({"defaults": {"temperature": 1}}),
        ] {
            assert_eq!(params(bad).1.len(), 1);
        }
        let mut body = json!({"messages": [{"a": 1}]});
        let (p, _) = params(json!({"overrides": {"/messages/0/a": 2}}));
        p.apply(&mut body, &mut Applied::default());
        assert_eq!(body, json!({"messages": [{"a": 2}]}));
    }
}
