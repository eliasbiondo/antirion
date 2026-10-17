//! Compiling the published JSON Schema (draft 2020-12, the subset of keywords the document uses)
//! into a tree that can be evaluated many times.

use std::collections::HashMap;

use regex::Regex;
use serde_json::{Map, Value};

use crate::scalar;

#[derive(Debug)]
pub(crate) enum Schema {
    Bool(bool),
    Node(Box<Node>),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum JsonType {
    Null,
    Boolean,
    Integer,
    Number,
    String,
    Array,
    Object,
}

impl JsonType {
    fn parse(s: &str) -> Option<JsonType> {
        Some(match s {
            "null" => JsonType::Null,
            "boolean" => JsonType::Boolean,
            "integer" => JsonType::Integer,
            "number" => JsonType::Number,
            "string" => JsonType::String,
            "array" => JsonType::Array,
            "object" => JsonType::Object,
            _ => return None,
        })
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            JsonType::Null => "null",
            JsonType::Boolean => "boolean",
            JsonType::Integer => "integer",
            JsonType::Number => "number",
            JsonType::String => "string",
            JsonType::Array => "array",
            JsonType::Object => "object",
        }
    }

    pub(crate) fn matches(self, v: &Value) -> bool {
        match (self, v) {
            (JsonType::Null, Value::Null)
            | (JsonType::Boolean, Value::Bool(_))
            | (JsonType::Number, Value::Number(_))
            | (JsonType::String, Value::String(_))
            | (JsonType::Array, Value::Array(_))
            | (JsonType::Object, Value::Object(_)) => true,
            (JsonType::Integer, Value::Number(n)) => {
                n.is_i64() || n.is_u64() || n.as_f64().is_some_and(|f| f.fract() == 0.0 && f.is_finite())
            }
            _ => false,
        }
    }
}

/// Bounds of a duration or size field, in nanoseconds or bytes.
#[derive(Debug, Default)]
pub(crate) struct XRange {
    pub(crate) min: Option<(u128, String)>,
    pub(crate) max: Option<(u128, String)>,
    pub(crate) gt: Option<(u128, String)>,
}

#[derive(Debug, Default)]
pub(crate) struct Node {
    pub(crate) reference: Option<usize>,
    pub(crate) types: Option<Vec<JsonType>>,
    pub(crate) enumeration: Option<Vec<Value>>,
    pub(crate) constant: Option<Value>,
    pub(crate) properties: Vec<(String, Schema)>,
    pub(crate) pattern_properties: Vec<(Regex, Schema)>,
    pub(crate) additional: Option<Schema>,
    pub(crate) property_names: Option<Schema>,
    pub(crate) required: Vec<String>,
    pub(crate) min_properties: Option<usize>,
    pub(crate) max_properties: Option<usize>,
    pub(crate) items: Option<Schema>,
    pub(crate) min_items: Option<usize>,
    pub(crate) max_items: Option<usize>,
    pub(crate) unique_items: bool,
    pub(crate) contains: Option<Schema>,
    pub(crate) pattern: Option<(Regex, String)>,
    pub(crate) min_length: Option<usize>,
    pub(crate) max_length: Option<usize>,
    pub(crate) minimum: Option<f64>,
    pub(crate) maximum: Option<f64>,
    pub(crate) exclusive_minimum: Option<f64>,
    pub(crate) exclusive_maximum: Option<f64>,
    pub(crate) all_of: Vec<Schema>,
    pub(crate) any_of: Vec<Schema>,
    pub(crate) one_of: Vec<Schema>,
    pub(crate) not: Option<Schema>,
    pub(crate) if_: Option<Schema>,
    pub(crate) then: Option<Schema>,
    pub(crate) else_: Option<Schema>,
    pub(crate) dependent_required: Vec<(String, Vec<String>)>,
    pub(crate) dependent_schemas: Vec<(String, Schema)>,
    pub(crate) x_range: Option<XRange>,
    pub(crate) secret: bool,
}

/// A compiled schema: the root and every `$defs` entry, referenced by index.
#[derive(Debug)]
pub(crate) struct Compiled {
    pub(crate) root: Schema,
    pub(crate) defs: Vec<Schema>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CompileError {
    #[error("unsupported $ref {0}")]
    Ref(String),
    #[error("invalid pattern {0}: {1}")]
    Pattern(String, regex::Error),
    #[error("invalid x-range bound {0}")]
    Range(String),
}

pub(crate) fn compile(doc: &Value) -> Result<Compiled, CompileError> {
    let empty = Map::new();
    let defs_src = doc.get("$defs").and_then(Value::as_object).unwrap_or(&empty);
    let def_index: HashMap<String, usize> =
        defs_src.keys().enumerate().map(|(i, k)| (k.clone(), i)).collect();
    let cx = Cx {
        def_index: &def_index,
    };
    let defs = defs_src
        .values()
        .map(|d| cx.schema(d))
        .collect::<Result<Vec<_>, _>>()?;
    let root = cx.schema(doc)?;
    Ok(Compiled { root, defs })
}

struct Cx<'a> {
    def_index: &'a HashMap<String, usize>,
}

impl Cx<'_> {
    fn schema(&self, v: &Value) -> Result<Schema, CompileError> {
        match v {
            Value::Bool(b) => Ok(Schema::Bool(*b)),
            Value::Object(m) => Ok(Schema::Node(Box::new(self.node(m)?))),
            _ => Ok(Schema::Bool(true)),
        }
    }

    fn opt(&self, m: &Map<String, Value>, key: &str) -> Result<Option<Schema>, CompileError> {
        m.get(key).map(|v| self.schema(v)).transpose()
    }

    fn list(&self, m: &Map<String, Value>, key: &str) -> Result<Vec<Schema>, CompileError> {
        match m.get(key).and_then(Value::as_array) {
            Some(items) => items.iter().map(|v| self.schema(v)).collect(),
            None => Ok(Vec::new()),
        }
    }

    fn node(&self, m: &Map<String, Value>) -> Result<Node, CompileError> {
        let mut n = Node::default();
        if let Some(r) = m.get("$ref").and_then(Value::as_str) {
            let name = r
                .strip_prefix("#/$defs/")
                .ok_or_else(|| CompileError::Ref(r.to_owned()))?;
            n.reference = Some(
                *self
                    .def_index
                    .get(name)
                    .ok_or_else(|| CompileError::Ref(r.to_owned()))?,
            );
        }
        n.types = match m.get("type") {
            Some(Value::String(t)) => JsonType::parse(t).map(|t| vec![t]),
            Some(Value::Array(ts)) => Some(
                ts.iter()
                    .filter_map(Value::as_str)
                    .filter_map(JsonType::parse)
                    .collect(),
            ),
            _ => None,
        };
        n.enumeration = m.get("enum").and_then(Value::as_array).cloned();
        n.constant = m.get("const").cloned();
        if let Some(props) = m.get("properties").and_then(Value::as_object) {
            for (k, v) in props {
                n.properties.push((k.clone(), self.schema(v)?));
            }
        }
        if let Some(pats) = m.get("patternProperties").and_then(Value::as_object) {
            for (p, v) in pats {
                let re = Regex::new(p).map_err(|e| CompileError::Pattern(p.clone(), e))?;
                n.pattern_properties.push((re, self.schema(v)?));
            }
        }
        n.additional = self.opt(m, "additionalProperties")?;
        n.property_names = self.opt(m, "propertyNames")?;
        n.required = m
            .get("required")
            .and_then(Value::as_array)
            .map(|r| r.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default();
        n.min_properties = usize_of(m, "minProperties");
        n.max_properties = usize_of(m, "maxProperties");
        n.items = self.opt(m, "items")?;
        n.min_items = usize_of(m, "minItems");
        n.max_items = usize_of(m, "maxItems");
        n.unique_items = m.get("uniqueItems").and_then(Value::as_bool).unwrap_or(false);
        n.contains = self.opt(m, "contains")?;
        if let Some(p) = m.get("pattern").and_then(Value::as_str) {
            let re = Regex::new(p).map_err(|e| CompileError::Pattern(p.to_owned(), e))?;
            n.pattern = Some((re, p.to_owned()));
        }
        n.min_length = usize_of(m, "minLength");
        n.max_length = usize_of(m, "maxLength");
        n.minimum = m.get("minimum").and_then(Value::as_f64);
        n.maximum = m.get("maximum").and_then(Value::as_f64);
        n.exclusive_minimum = m.get("exclusiveMinimum").and_then(Value::as_f64);
        n.exclusive_maximum = m.get("exclusiveMaximum").and_then(Value::as_f64);
        n.all_of = self.list(m, "allOf")?;
        n.any_of = self.list(m, "anyOf")?;
        n.one_of = self.list(m, "oneOf")?;
        n.not = self.opt(m, "not")?;
        n.if_ = self.opt(m, "if")?;
        n.then = self.opt(m, "then")?;
        n.else_ = self.opt(m, "else")?;
        if let Some(dr) = m.get("dependentRequired").and_then(Value::as_object) {
            for (k, v) in dr {
                let deps = v
                    .as_array()
                    .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect());
                n.dependent_required.push((k.clone(), deps.unwrap_or_default()));
            }
        }
        if let Some(ds) = m.get("dependentSchemas").and_then(Value::as_object) {
            for (k, v) in ds {
                n.dependent_schemas.push((k.clone(), self.schema(v)?));
            }
        }
        if let Some(r) = m.get("x-range").and_then(Value::as_object) {
            n.x_range = Some(x_range(r)?);
        }
        n.secret = m.get("x-secret").and_then(Value::as_bool).unwrap_or(false);
        Ok(n)
    }
}

fn usize_of(m: &Map<String, Value>, key: &str) -> Option<usize> {
    m.get(key)
        .and_then(Value::as_u64)
        .and_then(|v| usize::try_from(v).ok())
}

/// A duration or size string as a comparable quantity (nanoseconds or bytes) and its kind.
pub(crate) fn quantity(s: &str) -> Option<(bool, u128)> {
    if let Some(n) = scalar::duration_nanos(s) {
        return Some((true, n));
    }
    scalar::parse_size(s).map(|b| (false, u128::from(b)))
}

fn x_range(m: &Map<String, Value>) -> Result<XRange, CompileError> {
    let bound = |key: &str| -> Result<Option<(u128, String)>, CompileError> {
        match m.get(key).and_then(Value::as_str) {
            None => Ok(None),
            Some(s) => {
                let (_, q) = quantity(s).ok_or_else(|| CompileError::Range(s.to_owned()))?;
                Ok(Some((q, s.to_owned())))
            }
        }
    };
    Ok(XRange {
        min: bound("min")?,
        max: bound("max")?,
        gt: bound("gt")?,
    })
}
