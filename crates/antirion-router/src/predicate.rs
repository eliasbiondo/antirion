//! Conditions: predicates compiled once per config and evaluated per request against the facts a
//! request exposes (headers, body, metadata, request facts and identity).

use std::cmp::Ordering;

use regex::Regex;
use serde_json::Value;

/// A step of a dot path with `[n]` indices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathSeg {
    Key(String),
    Index(usize),
}

/// Parses `a.b[2].c`.
#[must_use]
pub fn parse_path(s: &str) -> Option<Vec<PathSeg>> {
    let mut out = Vec::new();
    for part in s.split('.') {
        let (name, mut rest) = match part.find('[') {
            Some(i) => part.split_at(i),
            None => (part, ""),
        };
        if name.is_empty() && rest.is_empty() {
            return None;
        }
        if !name.is_empty() {
            out.push(PathSeg::Key(name.to_owned()));
        }
        while let Some(stripped) = rest.strip_prefix('[') {
            let end = stripped.find(']')?;
            out.push(PathSeg::Index(stripped.get(..end)?.parse().ok()?));
            rest = stripped.get(end + 1..)?;
        }
        if !rest.is_empty() {
            return None;
        }
    }
    Some(out)
}

/// The value at a path.
#[must_use]
pub fn lookup<'a>(v: &'a Value, path: &[PathSeg]) -> Option<&'a Value> {
    path.iter().try_fold(v, |cur, seg| match seg {
        PathSeg::Key(k) => cur.get(k.as_str()),
        PathSeg::Index(i) => cur.get(*i),
    })
}

/// What a selector reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selector {
    /// A request header, lowercased.
    Header(String),
    Body(Vec<PathSeg>),
    Metadata(Vec<PathSeg>),
    /// `request.<field>`.
    Request(String),
    /// `identity.<field>`.
    Identity(String),
    /// `guard.<name>.<field>`: a field of an input guardrail's result, run when first read.
    Guard {
        name: String,
        field: String,
    },
}

impl Selector {
    /// Parses a selector; `None` when it is malformed.
    #[must_use]
    pub fn parse(s: &str) -> Option<Selector> {
        let (head, rest) = s.split_once('.')?;
        Some(match head {
            "header" => {
                let name = rest.to_ascii_lowercase();
                let credential = matches!(
                    name.as_str(),
                    "authorization" | "x-api-key" | "cookie" | "proxy-authorization"
                );
                let control = name.starts_with("x-antirion-") && name != "x-antirion-metadata";
                if name.is_empty() || credential || control {
                    return None;
                }
                Selector::Header(name)
            }
            "body" => Selector::Body(parse_path(rest)?),
            "metadata" => Selector::Metadata(parse_path(rest)?),
            "request" => Selector::Request(rest.to_owned()),
            "identity" => Selector::Identity(rest.to_owned()),
            "guard" => {
                let (name, field) = rest.split_once('.')?;
                if name.is_empty() || field.is_empty() {
                    return None;
                }
                Selector::Guard {
                    name: name.to_owned(),
                    field: field.to_owned(),
                }
            }
            _ => return None,
        })
    }
}

/// The facts of one request that selectors read.
pub trait Facts {
    /// A header's value; repeated headers joined with `", "`.
    fn header(&self, name: &str) -> Option<String>;
    /// The client's JSON body in its own dialect.
    fn body(&self) -> Option<&Value>;
    /// The parsed `x-antirion-metadata` object.
    fn metadata(&self) -> Option<&Value>;
    /// `request.<field>`.
    fn request(&self, field: &str) -> Option<Value>;
    /// `identity.<field>`.
    fn identity(&self, field: &str) -> Option<Value>;
    /// The request's point in [0, 1) for `sample` predicates; `None` makes them false.
    fn sample_point(&self) -> Option<f64> {
        None
    }
    /// A field of the result of input guardrail `name`, running it if it has not run.
    fn guard(&self, _name: &str, _field: &str) -> Option<Value> {
        None
    }
}

/// The point in [0, 1) a request id has for `sample` predicates: the first eight bytes of
/// SHA-256 over the seed (eight bytes, big-endian) and the id, read as an unsigned integer over 2^64.
#[must_use]
pub fn sample_point(seed: u64, request_id: &str) -> f64 {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(seed.to_be_bytes());
    h.update(request_id.as_bytes());
    let digest = h.finalize();
    let mut first = [0u8; 8];
    for (dst, src) in first.iter_mut().zip(digest.iter()) {
        *dst = *src;
    }
    #[allow(clippy::cast_precision_loss)]
    let x = u64::from_be_bytes(first) as f64;
    x / 18_446_744_073_709_551_616.0
}

fn select(sel: &Selector, f: &dyn Facts) -> Option<Value> {
    match sel {
        Selector::Header(name) => f.header(name).map(Value::String),
        Selector::Body(p) => f.body().and_then(|b| lookup(b, p)).cloned(),
        Selector::Metadata(p) => f.metadata().and_then(|m| lookup(m, p)).cloned(),
        Selector::Request(field) => f.request(field),
        Selector::Identity(field) => f.identity(field),
        Selector::Guard { name, field } => f.guard(name, field),
    }
}

/// A comparison operator with its configured value.
#[derive(Clone, Debug)]
pub enum Op {
    Eq(Value),
    Ne(Value),
    In(Vec<Value>),
    Nin(Vec<Value>),
    Gt(Value),
    Gte(Value),
    Lt(Value),
    Lte(Value),
    Contains(Value),
    Regex(Regex),
    Exists(bool),
}

/// A compiled predicate: every present part must hold.
#[derive(Clone, Debug, Default)]
pub struct Predicate {
    all: Option<Vec<Predicate>>,
    any: Option<Vec<Predicate>>,
    not: Option<Box<Predicate>>,
    field: Option<(Selector, Op)>,
    /// `sample`: holds for this share of request ids.
    sample: Option<f64>,
}

/// Why a predicate does not compile; the string is the offending member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PredicateError {
    /// Path below the predicate, dot separated (`any[0].regex`).
    pub at: String,
    pub message: String,
}

fn err(at: &str, message: &str) -> PredicateError {
    PredicateError {
        at: at.to_owned(),
        message: message.to_owned(),
    }
}

const OPERATORS: [&str; 11] = [
    "eq", "ne", "in", "nin", "gt", "gte", "lt", "lte", "contains", "regex", "exists",
];

impl Predicate {
    /// A predicate that never holds.
    #[must_use]
    pub fn never() -> Predicate {
        Predicate {
            not: Some(Box::default()),
            ..Predicate::default()
        }
    }

    /// Compiles a predicate object.
    pub fn compile(v: &Value) -> Result<Predicate, PredicateError> {
        Predicate::compile_at(v, "")
    }

    fn compile_at(v: &Value, at: &str) -> Result<Predicate, PredicateError> {
        let join = |k: &str| {
            if at.is_empty() {
                k.to_owned()
            } else {
                format!("{at}.{k}")
            }
        };
        let obj = v.as_object().ok_or_else(|| err(at, "a predicate is an object"))?;
        let mut p = Predicate::default();
        let list = |key: &str| -> Result<Option<Vec<Predicate>>, PredicateError> {
            let Some(items) = obj.get(key) else {
                return Ok(None);
            };
            let items = items
                .as_array()
                .ok_or_else(|| err(&join(key), "expected a list of predicates"))?;
            items
                .iter()
                .enumerate()
                .map(|(i, x)| Predicate::compile_at(x, &format!("{}[{i}]", join(key))))
                .collect::<Result<Vec<_>, _>>()
                .map(Some)
        };
        p.all = list("all")?;
        p.any = list("any")?;
        if p.any.as_ref().is_some_and(Vec::is_empty) {
            return Err(err(&join("any"), "`any` needs at least one predicate"));
        }
        if let Some(n) = obj.get("not") {
            p.not = Some(Box::new(Predicate::compile_at(n, &join("not"))?));
        }
        if let Some(r) = obj.get("sample") {
            p.sample = Some(
                r.as_f64()
                    .filter(|r| (0.0..=1.0).contains(r))
                    .ok_or_else(|| err(&join("sample"), "sample is a ratio in [0, 1]"))?,
            );
        }
        let ops: Vec<&str> = OPERATORS
            .iter()
            .copied()
            .filter(|o| obj.contains_key(*o))
            .collect();
        for k in obj.keys() {
            if !matches!(k.as_str(), "all" | "any" | "not" | "field" | "sample")
                && !OPERATORS.contains(&k.as_str())
            {
                return Err(err(&join(k), "unknown operator"));
            }
        }
        match (obj.get("field"), ops.as_slice()) {
            (None, []) => {}
            (Some(f), [op]) => {
                let sel = f
                    .as_str()
                    .and_then(Selector::parse)
                    .ok_or_else(|| err(&join("field"), "not a valid selector"))?;
                let value = obj.get(*op).cloned().unwrap_or(Value::Null);
                let at_op = join(op);
                let op = match *op {
                    "eq" => Op::Eq(value),
                    "ne" => Op::Ne(value),
                    "in" | "nin" => {
                        let items = value
                            .as_array()
                            .cloned()
                            .ok_or_else(|| err(&at_op, "expected a list"))?;
                        if *op == "in" {
                            Op::In(items)
                        } else {
                            Op::Nin(items)
                        }
                    }
                    "gt" | "gte" | "lt" | "lte" => {
                        if !(value.is_number() || value.is_string()) {
                            return Err(err(&at_op, "expected a number or a string"));
                        }
                        match *op {
                            "gt" => Op::Gt(value),
                            "gte" => Op::Gte(value),
                            "lt" => Op::Lt(value),
                            _ => Op::Lte(value),
                        }
                    }
                    "contains" => Op::Contains(value),
                    "regex" => {
                        let pattern = value.as_str().ok_or_else(|| err(&at_op, "expected a string"))?;
                        Op::Regex(
                            regex::RegexBuilder::new(pattern)
                                .size_limit(1 << 20)
                                .build()
                                .map_err(|_| err(&at_op, "not a valid regular expression"))?,
                        )
                    }
                    _ => Op::Exists(value.as_bool().ok_or_else(|| err(&at_op, "expected a boolean"))?),
                };
                p.field = Some((sel, op));
            }
            (Some(_), _) => return Err(err(&join("field"), "a field needs exactly one operator")),
            (None, _) => return Err(err(at, "an operator needs a field")),
        }
        Ok(p)
    }

    /// Evaluates left to right, short-circuit.
    #[must_use]
    pub fn eval(&self, f: &dyn Facts) -> bool {
        if let Some(all) = &self.all
            && !all.iter().all(|p| p.eval(f))
        {
            return false;
        }
        if let Some(any) = &self.any
            && !any.iter().any(|p| p.eval(f))
        {
            return false;
        }
        if let Some(not) = &self.not
            && not.eval(f)
        {
            return false;
        }
        if let Some(ratio) = self.sample
            && !f.sample_point().is_some_and(|p| p < ratio)
        {
            return false;
        }
        match &self.field {
            Some((sel, op)) => apply(op, select(sel, f).as_ref()),
            None => true,
        }
    }
}

impl Predicate {
    /// Every selector the predicate reads, in evaluation order.
    pub fn selectors(&self) -> Vec<&Selector> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect<'a>(&'a self, out: &mut Vec<&'a Selector>) {
        for p in self.all.iter().chain(self.any.iter()).flatten() {
            p.collect(out);
        }
        if let Some(n) = &self.not {
            n.collect(out);
        }
        if let Some((sel, _)) = &self.field {
            out.push(sel);
        }
    }
}

fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        _ => a == b,
    }
}

fn as_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => serde_json::from_str::<Value>(s.trim())
            .ok()
            .filter(Value::is_number)?
            .as_f64(),
        _ => None,
    }
}

fn compare(field: &Value, configured: &Value) -> Option<Ordering> {
    match configured {
        Value::Number(c) => as_number(field)?.partial_cmp(&c.as_f64()?),
        Value::String(c) => Some(field.as_str()?.chars().cmp(c.chars())),
        _ => None,
    }
}

fn apply(op: &Op, field: Option<&Value>) -> bool {
    let Some(v) = field else {
        return matches!(op, Op::Ne(_) | Op::Nin(_) | Op::Exists(false));
    };
    let some_eq = |list: &[Value]| match v {
        Value::Array(items) => items.iter().any(|i| list.iter().any(|l| json_eq(i, l))),
        _ => list.iter().any(|l| json_eq(v, l)),
    };
    match op {
        Op::Eq(c) => json_eq(v, c),
        Op::Ne(c) => !json_eq(v, c),
        Op::In(list) => some_eq(list),
        Op::Nin(list) => !some_eq(list),
        Op::Gt(c) => compare(v, c) == Some(Ordering::Greater),
        Op::Gte(c) => matches!(compare(v, c), Some(Ordering::Greater | Ordering::Equal)),
        Op::Lt(c) => compare(v, c) == Some(Ordering::Less),
        Op::Lte(c) => matches!(compare(v, c), Some(Ordering::Less | Ordering::Equal)),
        Op::Contains(c) => match (v, c) {
            (Value::String(s), Value::String(sub)) => s.contains(sub.as_str()),
            (Value::Array(items), _) => items.iter().any(|i| json_eq(i, c)),
            _ => false,
        },
        Op::Regex(re) => v.as_str().is_some_and(|s| re.is_match(s)),
        Op::Exists(want) => *want,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    struct Meta(Value);

    impl Facts for Meta {
        fn header(&self, _name: &str) -> Option<String> {
            None
        }
        fn body(&self) -> Option<&Value> {
            None
        }
        fn metadata(&self) -> Option<&Value> {
            Some(&self.0)
        }
        fn request(&self, _field: &str) -> Option<Value> {
            None
        }
        fn identity(&self, _field: &str) -> Option<Value> {
            None
        }
    }

    fn holds(p: Value, meta: Value) -> bool {
        Predicate::compile(&p).unwrap().eval(&Meta(meta))
    }

    #[test]
    fn sibling_keys_and_together() {
        let p = json!({"any": [{"field": "metadata.a", "eq": "1"}, {"field": "metadata.b", "eq": "2"}],
                       "field": "metadata.c", "eq": "3"});
        assert!(!holds(p.clone(), json!({"a": "1", "c": "nope"})));
        assert!(holds(p, json!({"a": "1", "c": "3"})));
        assert!(holds(
            json!({"not": {"field": "metadata.a", "eq": "1"}, "all": []}),
            json!({})
        ));
    }

    #[test]
    fn operators_and_absent_fields() {
        let gte = json!({"field": "metadata.s", "gte": 0.5});
        assert!(holds(gte.clone(), json!({"s": "0.7"})));
        assert!(holds(gte.clone(), json!({"s": 0.5})));
        assert!(!holds(gte.clone(), json!({"s": "abc"})));
        assert!(!holds(gte, json!({})));
        assert!(holds(json!({"field": "metadata.n", "eq": 1}), json!({"n": 1.0})));
        assert!(!holds(json!({"field": "metadata.n", "eq": 1}), json!({"n": "1"})));
        assert!(holds(
            json!({"field": "metadata.x", "gt": "m"}),
            json!({"x": "n"})
        ));
        assert!(holds(json!({"field": "metadata.r", "ne": "eu"}), json!({})));
        assert!(!holds(json!({"field": "metadata.r", "regex": ".*"}), json!({})));
        assert!(holds(
            json!({"field": "metadata.r", "exists": true}),
            json!({"r": null})
        ));
        assert!(holds(
            json!({"field": "metadata.u", "regex": "min"}),
            json!({"u": "admin-7"})
        ));
    }

    #[test]
    fn bad_predicates() {
        assert!(Predicate::compile(&json!({"field": "metadata.t", "$like": "x"})).is_err());
        assert!(Predicate::compile(&json!({"field": "metadata.t", "regex": "("})).is_err());
        assert!(Predicate::compile(&json!({"any": []})).is_err());
        assert_eq!(
            parse_path("a.b[2].c"),
            Some(vec![
                PathSeg::Key("a".into()),
                PathSeg::Key("b".into()),
                PathSeg::Index(2),
                PathSeg::Key("c".into())
            ])
        );
    }
}
