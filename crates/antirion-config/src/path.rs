//! Config-document paths: dotted keys, `[n]` for list indexes, `["…"]` for keys that contain `.` or `[`.

use std::fmt;

/// One step of a document path.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Seg {
    Key(String),
    Index(usize),
}

/// A path into the config document, rendered in the error style of the Admin API.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocPath(Vec<Seg>);

impl DocPath {
    #[must_use]
    pub fn root() -> DocPath {
        DocPath(Vec::new())
    }

    /// A path from dotted plain keys, for paths written in code (`"models.m.route"`).
    #[must_use]
    pub fn parse_dotted(s: &str) -> DocPath {
        DocPath(
            s.split('.')
                .filter(|p| !p.is_empty())
                .map(|p| Seg::Key(p.to_owned()))
                .collect(),
        )
    }

    #[must_use]
    pub fn key(&self, k: &str) -> DocPath {
        let mut p = self.clone();
        p.0.push(Seg::Key(k.to_owned()));
        p
    }

    #[must_use]
    pub fn index(&self, i: usize) -> DocPath {
        let mut p = self.clone();
        p.0.push(Seg::Index(i));
        p
    }

    pub fn push_key(&mut self, k: &str) {
        self.0.push(Seg::Key(k.to_owned()));
    }

    pub fn push_index(&mut self, i: usize) {
        self.0.push(Seg::Index(i));
    }

    pub fn pop(&mut self) {
        self.0.pop();
    }

    #[must_use]
    pub fn segments(&self) -> &[Seg] {
        &self.0
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    pub fn starts_with(&self, prefix: &DocPath) -> bool {
        self.0.starts_with(&prefix.0)
    }

    /// The value at this path in `doc`, if any.
    #[must_use]
    pub fn get<'a>(&self, doc: &'a serde_json::Value) -> Option<&'a serde_json::Value> {
        let mut cur = doc;
        for seg in &self.0 {
            cur = match seg {
                Seg::Key(k) => cur.as_object()?.get(k)?,
                Seg::Index(i) => cur.as_array()?.get(*i)?,
            };
        }
        Some(cur)
    }

    /// A mutable reference to the value at this path, if any.
    pub fn get_mut<'a>(&self, doc: &'a mut serde_json::Value) -> Option<&'a mut serde_json::Value> {
        let mut cur = doc;
        for seg in &self.0 {
            cur = match seg {
                Seg::Key(k) => cur.as_object_mut()?.get_mut(k)?,
                Seg::Index(i) => cur.as_array_mut()?.get_mut(*i)?,
            };
        }
        Some(cur)
    }
}

impl fmt::Display for DocPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, seg) in self.0.iter().enumerate() {
            match seg {
                Seg::Index(n) => write!(f, "[{n}]")?,
                Seg::Key(k) if k.contains(['.', '[']) || k.is_empty() => {
                    write!(f, "[\"{}\"]", k.replace('\\', "\\\\").replace('"', "\\\""))?;
                }
                Seg::Key(k) if i == 0 => f.write_str(k)?,
                Seg::Key(k) => write!(f, ".{k}")?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_in_the_admin_api_style() {
        let p = DocPath::root()
            .key("routes")
            .key("fast")
            .key("tree")
            .key("children")
            .index(2)
            .key("weight");
        assert_eq!(p.to_string(), "routes.fast.tree.children[2].weight");
        let q = DocPath::root().key("models").key("gpt-5.1").key("route");
        assert_eq!(q.to_string(), "models[\"gpt-5.1\"].route");
    }

    #[test]
    fn get_follows_the_path() {
        let doc = serde_json::json!({"a": {"b": [1, {"c": 2}]}});
        let p = DocPath::parse_dotted("a.b").index(1).key("c");
        assert_eq!(p.get(&doc), Some(&serde_json::json!(2)));
        assert_eq!(DocPath::parse_dotted("a.x").get(&doc), None);
    }
}
