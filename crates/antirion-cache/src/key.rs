//! The exact-cache key: SHA-256 over the canonical text of the key document, whose body member is the
//! request body without the members that do not change the generated content.

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::jcs::canonical;

/// Lowercase hex SHA-256 of `bytes`.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest.iter() {
        out.push(char::from_digit(u32::from(b >> 4), 16).unwrap_or('0'));
        out.push(char::from_digit(u32::from(b & 0xf), 16).unwrap_or('0'));
    }
    out
}

/// Lowercase hex SHA-256 of the canonical text of `v`.
#[must_use]
pub fn fingerprint(v: &Value) -> String {
    sha256_hex(canonical(v).as_bytes())
}

/// Whether `s` is a key: 64 lowercase hex digits.
#[must_use]
pub fn well_formed(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Everything the key is computed from.
#[derive(Clone, Debug)]
pub struct KeyDoc {
    /// The tenant scope array.
    pub scope: Vec<String>,
    /// Invalidation epochs: gateway, organization, team, project, route.
    pub epochs: [u64; 5],
    pub namespace: String,
    /// The canonical operation name (`openai.chat_completions`).
    pub endpoint: String,
    pub route: String,
    pub route_fp: String,
    pub guard_fp: String,
    /// `[name, value]` pairs of the included request headers, sorted by name.
    pub headers: Vec<(String, String)>,
    /// `[name, value]` pairs of the query string, sorted by name then value.
    pub query: Vec<(String, String)>,
    /// The normalised body.
    pub body: Value,
}

fn pairs(p: &[(String, String)]) -> Value {
    Value::Array(
        p.iter()
            .map(|(n, v)| Value::Array(vec![Value::from(n.as_str()), Value::from(v.as_str())]))
            .collect(),
    )
}

impl KeyDoc {
    /// The key: 64 lowercase hex digits.
    #[must_use]
    pub fn key(&self) -> String {
        self.digest(Value::from(1))
    }

    /// The semantic partition key: the same document as version `"s1"`, built by the caller with the
    /// last user text emptied.
    #[must_use]
    pub fn partition_key(&self) -> String {
        self.digest(Value::from("s1"))
    }

    fn digest(&self, version: Value) -> String {
        let mut d = Map::new();
        d.insert("v".into(), version);
        d.insert(
            "scope".into(),
            Value::Array(self.scope.iter().map(|s| Value::from(s.as_str())).collect()),
        );
        d.insert(
            "epochs".into(),
            Value::Array(self.epochs.iter().map(|e| Value::from(*e)).collect()),
        );
        d.insert("ns".into(), Value::from(self.namespace.as_str()));
        d.insert("endpoint".into(), Value::from(self.endpoint.as_str()));
        d.insert("route".into(), Value::from(self.route.as_str()));
        d.insert("route_fp".into(), Value::from(self.route_fp.as_str()));
        d.insert("guard_fp".into(), Value::from(self.guard_fp.as_str()));
        d.insert("headers".into(), pairs(&self.headers));
        d.insert("query".into(), pairs(&self.query));
        d.insert("body".into(), self.body.clone());
        fingerprint(&Value::Object(d))
    }
}

/// The included request headers: lowercase names, trimmed values, repeats joined with `", "` in
/// arrival order, sorted by name.
#[must_use]
pub fn included_headers(include: &[String], headers: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = include
        .iter()
        .filter_map(|name| {
            let name = name.to_ascii_lowercase();
            let values: Vec<&str> = headers
                .iter()
                .filter(|(n, _)| n.eq_ignore_ascii_case(&name))
                .map(|(_, v)| v.trim())
                .collect();
            (!values.is_empty()).then(|| (name, values.join(", ")))
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        let hex = |at: usize| bytes.get(at).and_then(|c| char::from(*c).to_digit(16));
        match (b, hex(i + 1), hex(i + 2)) {
            (b'%', Some(h), Some(l)) => {
                out.push(u8::try_from(h * 16 + l).unwrap_or(b'%'));
                i += 3;
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The query string's pairs, percent-decoded and sorted, without the parameters in `owned`.
#[must_use]
pub fn query_pairs(query: Option<&str>, owned: &[&str]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = query
        .unwrap_or_default()
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (n, v) = p.split_once('=').unwrap_or((p, ""));
            (percent_decode(n), percent_decode(v))
        })
        .filter(|(n, _)| !owned.contains(&n.as_str()))
        .collect();
    out.sort();
    out
}

/// The top-level members each endpoint drops before hashing.
fn dropped(endpoint: &str) -> &'static [&'static str] {
    match endpoint {
        "openai.chat_completions" | "openai.completions" => &[
            "store",
            "service_tier",
            "prompt_cache_key",
            "prompt_cache_retention",
            "safety_identifier",
        ],
        "openai.responses" => &[
            "store",
            "service_tier",
            "prompt_cache_key",
            "prompt_cache_retention",
            "safety_identifier",
            "stream_options",
        ],
        "anthropic.messages" => &["service_tier"],
        _ => &[],
    }
}

/// Where caller `cache_control` markers sit; nowhere else is one removed.
const MARKERS: &[&str] = &[
    "/system/*",
    "/tools/*",
    "/messages/*",
    "/messages/*/content/*",
    "/input/*",
    "/input/*/content/*",
];

/// The body as it is hashed: members that do not change the generated content removed, caller
/// cache markers removed, then the operator's `ignore_fields`.
#[must_use]
pub fn normalise(endpoint: &str, body: &Value, ignore_fields: &[String]) -> Value {
    let mut body = body.clone();
    if let Some(o) = body.as_object_mut() {
        for name in ["model", "stream", "stream_options", "user", "metadata"]
            .iter()
            .chain(dropped(endpoint))
        {
            o.shift_remove(*name);
        }
    }
    for pattern in MARKERS {
        let mut tokens = tokens(pattern);
        tokens.push("cache_control".to_owned());
        remove(&mut body, &tokens);
    }
    for pattern in ignore_fields {
        remove(&mut body, &tokens(pattern));
    }
    body
}

/// The reference tokens of a JSON Pointer, unescaped.
fn tokens(pointer: &str) -> Vec<String> {
    pointer
        .split('/')
        .skip(1)
        .map(|t| t.replace("~1", "/").replace("~0", "~"))
        .collect()
}

/// Removes every member the tokens reach; `*` matches any array index.
fn remove(v: &mut Value, tokens: &[String]) {
    let Some((first, rest)) = tokens.split_first() else {
        return;
    };
    match v {
        Value::Object(o) => {
            if rest.is_empty() {
                o.shift_remove(first);
            } else if let Some(child) = o.get_mut(first) {
                remove(child, rest);
            }
        }
        Value::Array(items) => {
            if first == "*" {
                if rest.is_empty() {
                    items.clear();
                } else {
                    for item in items.iter_mut() {
                        remove(item, rest);
                    }
                }
            } else if let Some(i) = first.parse::<usize>().ok().filter(|i| *i < items.len()) {
                if rest.is_empty() {
                    items.remove(i);
                } else if let Some(item) = items.get_mut(i) {
                    remove(item, rest);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn doc(body: Value) -> KeyDoc {
        KeyDoc {
            scope: vec!["project".into(), "o".into(), "t".into(), "p".into()],
            epochs: [0; 5],
            namespace: String::new(),
            endpoint: "openai.chat_completions".into(),
            route: "r".into(),
            route_fp: "f".into(),
            guard_fp: fingerprint(&json!([])),
            headers: Vec::new(),
            query: Vec::new(),
            body: normalise("openai.chat_completions", &body, &[]),
        }
    }

    #[test]
    fn neutral_members_and_spelling_share_a_key() {
        let a: Value = serde_json::from_str(
            r#"{"model":"m","messages":[{"role":"user","content":"x"}],"temperature":0.5}"#,
        )
        .unwrap();
        let b: Value = serde_json::from_str(
            r#"{"temperature":5e-1,"stream":false,"user":"u","metadata":{"a":1},"messages":[{"content":"x","role":"user"}],"model":"other"}"#,
        )
        .unwrap();
        assert_eq!(doc(a.clone()).key(), doc(b).key());
        let c = json!({"model":"m","messages":[{"role":"user","content":"x"}],"temperature":1});
        assert_ne!(doc(a).key(), doc(c).key());
        assert!(well_formed(&doc(json!({})).key()));
    }

    #[test]
    fn markers_are_removed_only_where_callers_put_them() {
        let body = json!({"system": [{"type": "text", "text": "s", "cache_control": {"type": "ephemeral"}}],
            "messages": [{"role": "user", "content": [{"type": "text", "text": "q", "cache_control": {}}]}],
            "tools": [{"name": "f", "input_schema": {"properties": {"cache_control": {"type": "string"}}}}]});
        let n = normalise("anthropic.messages", &body, &[]);
        assert!(n.pointer("/system/0/cache_control").is_none());
        assert!(n.pointer("/messages/0/content/0/cache_control").is_none());
        assert!(
            n.pointer("/tools/0/input_schema/properties/cache_control")
                .is_some()
        );
    }

    #[test]
    fn ignore_fields_take_wildcards() {
        let body = json!({"temperature": 1, "messages": [{"role": "user", "name": "a"}, {"role": "user", "name": "b"}]});
        let n = normalise(
            "openai.chat_completions",
            &body,
            &["/temperature".into(), "/messages/*/name".into()],
        );
        assert_eq!(n, json!({"messages": [{"role": "user"}, {"role": "user"}]}));
    }

    #[test]
    fn headers_and_queries_are_canonical() {
        let h = [("OpenAI-Beta", " a "), ("x", "1"), ("openai-beta", "b")];
        assert_eq!(
            included_headers(&["openai-beta".into(), "anthropic-version".into()], &h),
            vec![("openai-beta".to_owned(), "a, b".to_owned())]
        );
        assert_eq!(
            query_pairs(Some("b=2&a=%41&key=s"), &["key"]),
            vec![("a".to_owned(), "A".to_owned()), ("b".to_owned(), "2".to_owned())]
        );
    }
}
