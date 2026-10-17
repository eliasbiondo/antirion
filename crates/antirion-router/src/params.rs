//! Node and alias parameters: `set` deep-merges objects and replaces everything else (explicit
//! `null` included); `remove` deletes dot paths. Fields not named are left as they are.

use serde_json::{Map, Value};

use crate::predicate::{PathSeg, parse_path};

/// A `params` member of a node or an alias.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Params {
    pub set: Map<String, Value>,
    pub remove: Vec<Vec<PathSeg>>,
}

impl Params {
    /// Reads a `params` value; `None` when it edits nothing.
    #[must_use]
    pub fn from_value(v: Option<&Value>) -> Option<Params> {
        let v = v?;
        let p = Params {
            set: v
                .get("set")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
            remove: v
                .get("remove")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter_map(parse_path)
                .collect(),
        };
        (!p.set.is_empty() || !p.remove.is_empty()).then_some(p)
    }

    /// Applies the edits to a request body.
    pub fn apply(&self, body: &mut Value) {
        if let Value::Object(obj) = body {
            merge(obj, &self.set);
        }
        for path in &self.remove {
            remove(body, path);
        }
    }
}

fn merge(into: &mut Map<String, Value>, from: &Map<String, Value>) {
    for (k, v) in from {
        match (into.get_mut(k), v) {
            (Some(Value::Object(a)), Value::Object(b)) => merge(a, b),
            _ => {
                into.insert(k.clone(), v.clone());
            }
        }
    }
}

fn remove(v: &mut Value, path: &[PathSeg]) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut cur = v;
    for seg in parents {
        let next = match seg {
            PathSeg::Key(k) => cur.get_mut(k.as_str()),
            PathSeg::Index(i) => cur.get_mut(*i),
        };
        let Some(next) = next else { return };
        cur = next;
    }
    match (cur, last) {
        (Value::Object(o), PathSeg::Key(k)) => {
            o.remove(k);
        }
        (Value::Array(a), PathSeg::Index(i)) if *i < a.len() => {
            a.remove(*i);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn set_merges_objects_and_replaces_the_rest() {
        let root = Params::from_value(Some(
            &json!({"set": {"temperature": 0.9, "metadata": {"src": "root"}, "stop": ["a"]}}),
        ))
        .unwrap();
        let leaf = Params::from_value(Some(&json!({"set": {"temperature": 0.2, "metadata": {"tier": "leaf"}, "stop": ["b"], "user": null}, "remove": ["top_p"]}))).unwrap();
        let mut body =
            json!({"temperature": 1, "top_p": 0.5, "metadata": {"client": "yes"}, "user": "bob", "seed": 7});
        root.apply(&mut body);
        leaf.apply(&mut body);
        assert_eq!(
            body,
            json!({"temperature": 0.2, "metadata": {"client": "yes", "src": "root", "tier": "leaf"},
                   "stop": ["b"], "user": null, "seed": 7})
        );
        assert!(Params::from_value(Some(&json!({}))).is_none());
    }
}
