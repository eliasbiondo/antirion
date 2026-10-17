//! Evaluating a compiled schema against a document, reporting every violation with its document path
//! and the detail code of the Admin API, and recording where secret fields are.

use std::cell::Cell;
use std::collections::HashSet;

use serde_json::Value;

use super::combinators::{best_branch, is_reserved_names, required_only};
use super::compile::{Compiled, Node, Schema, quantity};
use crate::{DetailCode, DocPath};

/// One violation found by the schema.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Violation {
    pub(crate) path: DocPath,
    pub(crate) code: DetailCode,
    pub(crate) message: String,
}

/// Errors and secret-field paths of one evaluation.
#[derive(Debug, Default)]
pub(crate) struct Outcome {
    pub(crate) violations: Vec<Violation>,
    pub(crate) secrets: Vec<DocPath>,
}

impl Outcome {
    fn absorb(&mut self, other: Outcome) {
        self.violations.extend(other.violations);
        self.secrets.extend(other.secrets);
    }
}

pub(crate) fn validate(compiled: &Compiled, schema: &Schema, value: &Value, root: &DocPath) -> Outcome {
    validate_within(compiled, schema, value, root, u32::MAX)
}

/// As [`validate`], nesting at most `max_depth` subschemas: a self-referencing schema stops there
/// with a violation instead of recursing without end.
pub(crate) fn validate_within(
    compiled: &Compiled,
    schema: &Schema,
    value: &Value,
    root: &DocPath,
    max_depth: u32,
) -> Outcome {
    let mut path = root.clone();
    let mut out = Outcome::default();
    let eval = Eval {
        compiled,
        depth: Cell::new(0),
        max_depth,
    };
    eval.schema(schema, value, &mut path, &mut out);
    let mut seen = HashSet::new();
    out.violations.retain(|v| seen.insert((v.path.clone(), v.code)));
    out.violations
        .sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.message.cmp(&b.message)));
    let mut seen = HashSet::new();
    out.secrets.retain(|p| seen.insert(p.clone()));
    out
}

struct Eval<'c> {
    compiled: &'c Compiled,
    depth: Cell<u32>,
    max_depth: u32,
}

pub(super) fn push(out: &mut Outcome, path: &DocPath, code: DetailCode, message: String) {
    out.violations.push(Violation {
        path: path.clone(),
        code,
        message,
    });
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn render(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn bound(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        x.to_string()
    }
}

impl Eval<'_> {
    fn sub(&self, schema: &Schema, value: &Value, path: &mut DocPath) -> Outcome {
        let mut out = Outcome::default();
        self.schema(schema, value, path, &mut out);
        out
    }

    fn valid(&self, schema: &Schema, value: &Value, path: &mut DocPath) -> bool {
        self.sub(schema, value, path).violations.is_empty()
    }

    fn schema(&self, schema: &Schema, value: &Value, path: &mut DocPath, out: &mut Outcome) {
        let depth = self.depth.get();
        if depth >= self.max_depth {
            push(
                out,
                path,
                DetailCode::Conflict,
                "the schema nests too deeply".to_owned(),
            );
            return;
        }
        self.depth.set(depth + 1);
        self.schema_at(schema, value, path, out);
        self.depth.set(depth);
    }

    fn schema_at(&self, schema: &Schema, value: &Value, path: &mut DocPath, out: &mut Outcome) {
        match schema {
            Schema::Bool(true) => {}
            Schema::Bool(false) => {
                let name = match path.segments().last() {
                    Some(crate::path::Seg::Key(k)) => format!("field {k:?} is not allowed here"),
                    _ => String::from("no value is allowed here"),
                };
                push(out, path, DetailCode::UnknownField, name);
            }
            Schema::Node(node) => self.node(node, value, path, out),
        }
    }

    fn node(&self, n: &Node, value: &Value, path: &mut DocPath, out: &mut Outcome) {
        if n.secret && value.is_string() {
            out.secrets.push(path.clone());
        }
        if let Some(r) = n.reference
            && let Some(def) = self.compiled.defs.get(r)
        {
            self.schema(def, value, path, out);
        }
        if let Some(types) = &n.types
            && !types.iter().any(|t| t.matches(value))
        {
            let expected: Vec<&str> = types.iter().map(|t| t.name()).collect();
            push(
                out,
                path,
                DetailCode::TypeMismatch,
                format!("expected {}, got {}", expected.join(" or "), kind_of(value)),
            );
            return;
        }
        if let Some(c) = &n.constant
            && c != value
        {
            push(
                out,
                path,
                DetailCode::InvalidEnum,
                format!("must be {}", render(c)),
            );
        }
        if let Some(allowed) = &n.enumeration
            && !allowed.contains(value)
        {
            let names: Vec<String> = allowed.iter().map(render).collect();
            push(
                out,
                path,
                DetailCode::InvalidEnum,
                format!("must be one of: {}", names.join(", ")),
            );
        }
        match value {
            Value::Object(map) => self.object(n, map, path, out),
            Value::Array(items) => self.array(n, items, path, out),
            Value::String(s) => self.string(n, s, path, out),
            Value::Number(num) => self.number(n, num.as_f64().unwrap_or(0.0), path, out),
            _ => {}
        }
        for s in &n.all_of {
            self.schema(s, value, path, out);
        }
        if !n.any_of.is_empty() {
            self.any_of(&n.any_of, value, path, out);
        }
        if !n.one_of.is_empty() {
            self.one_of(&n.one_of, value, path, out);
        }
        if let Some(not) = &n.not
            && self.valid(not, value, path)
        {
            let code = if is_reserved_names(not) {
                DetailCode::ReservedName
            } else {
                DetailCode::InvalidFormat
            };
            let message = if code == DetailCode::ReservedName {
                format!("{} is a reserved name", render(value))
            } else {
                String::from("this value is not allowed")
            };
            push(out, path, code, message);
        }
        if let Some(cond) = &n.if_ {
            if self.valid(cond, value, path) {
                if let Some(t) = &n.then {
                    self.schema(t, value, path, out);
                }
            } else if let Some(e) = &n.else_ {
                self.schema(e, value, path, out);
            }
        }
    }

    fn object(&self, n: &Node, map: &serde_json::Map<String, Value>, path: &mut DocPath, out: &mut Outcome) {
        for key in &n.required {
            if !map.contains_key(key) {
                let at = path.key(key);
                push(
                    out,
                    &at,
                    DetailCode::Required,
                    format!("required field {key:?} is missing"),
                );
            }
        }
        if let Some(min) = n.min_properties
            && map.len() < min
        {
            push(
                out,
                path,
                DetailCode::OutOfRange,
                format!("must have at least {min} entries"),
            );
        }
        if let Some(max) = n.max_properties
            && map.len() > max
        {
            push(
                out,
                path,
                DetailCode::OutOfRange,
                format!("must have at most {max} entries"),
            );
        }
        for (key, value) in map {
            path.push_key(key);
            if let Some(names) = &n.property_names {
                let key_value = Value::String(key.clone());
                let sub = self.sub(names, &key_value, path);
                for v in sub.violations {
                    let code = match v.code {
                        DetailCode::ReservedName | DetailCode::InvalidEnum => v.code,
                        _ => DetailCode::InvalidFormat,
                    };
                    push(out, path, code, format!("invalid name {key:?}: {}", v.message));
                }
            }
            let mut matched = false;
            if let Some((_, s)) = n.properties.iter().find(|(k, _)| k == key) {
                matched = true;
                self.schema(s, value, path, out);
            }
            for (re, s) in &n.pattern_properties {
                if re.is_match(key) {
                    matched = true;
                    self.schema(s, value, path, out);
                }
            }
            if !matched {
                match &n.additional {
                    Some(Schema::Bool(false)) => {
                        push(
                            out,
                            path,
                            DetailCode::UnknownField,
                            format!("unknown field {key:?}"),
                        );
                    }
                    Some(s) => self.schema(s, value, path, out),
                    None => {}
                }
            }
            path.pop();
        }
        for (key, deps) in &n.dependent_required {
            if map.contains_key(key) {
                for dep in deps.iter().filter(|d| !map.contains_key(*d)) {
                    let at = path.key(dep);
                    push(
                        out,
                        &at,
                        DetailCode::Conflict,
                        format!("{dep:?} is required when {key:?} is set"),
                    );
                }
            }
        }
        let whole = Value::Object(map.clone());
        for (key, s) in &n.dependent_schemas {
            if map.contains_key(key) {
                self.schema(s, &whole, path, out);
            }
        }
    }

    fn array(&self, n: &Node, items: &[Value], path: &mut DocPath, out: &mut Outcome) {
        if let Some(min) = n.min_items
            && items.len() < min
        {
            push(
                out,
                path,
                DetailCode::OutOfRange,
                format!("must have at least {min} items"),
            );
        }
        if let Some(max) = n.max_items
            && items.len() > max
        {
            push(
                out,
                path,
                DetailCode::OutOfRange,
                format!("must have at most {max} items"),
            );
        }
        if n.unique_items {
            for (i, item) in items.iter().enumerate() {
                if items.iter().take(i).any(|earlier| earlier == item) {
                    let at = path.index(i);
                    push(
                        out,
                        &at,
                        DetailCode::InvalidFormat,
                        String::from("duplicate item"),
                    );
                }
            }
        }
        if let Some(s) = &n.items {
            for (i, item) in items.iter().enumerate() {
                path.push_index(i);
                self.schema(s, item, path, out);
                path.pop();
            }
        }
        if let Some(c) = &n.contains {
            let found = items.iter().enumerate().any(|(i, item)| {
                path.push_index(i);
                let ok = self.valid(c, item, path);
                path.pop();
                ok
            });
            if !found {
                push(
                    out,
                    path,
                    DetailCode::Required,
                    String::from("no item matches what is required here"),
                );
            }
        }
    }

    fn string(&self, n: &Node, s: &str, path: &mut DocPath, out: &mut Outcome) {
        let len = s.chars().count();
        if let Some(min) = n.min_length
            && len < min
        {
            push(
                out,
                path,
                DetailCode::OutOfRange,
                format!("must be at least {min} characters"),
            );
        }
        if let Some(max) = n.max_length
            && len > max
        {
            push(
                out,
                path,
                DetailCode::OutOfRange,
                format!("must be at most {max} characters"),
            );
        }
        if let Some((re, src)) = &n.pattern
            && !re.is_match(s)
        {
            push(
                out,
                path,
                DetailCode::InvalidFormat,
                format!("does not match the format {src}"),
            );
            return;
        }
        if let (Some(range), Some((is_duration, q))) = (&n.x_range, quantity(s)) {
            let same_kind = |b: &str| quantity(b).is_some_and(|(k, _)| k == is_duration);
            if let Some((min, text)) = &range.min
                && same_kind(text)
                && q < *min
            {
                push(
                    out,
                    path,
                    DetailCode::OutOfRange,
                    format!("must be at least {text}"),
                );
            }
            if let Some((max, text)) = &range.max
                && same_kind(text)
                && q > *max
            {
                push(
                    out,
                    path,
                    DetailCode::OutOfRange,
                    format!("must be at most {text}"),
                );
            }
            if let Some((gt, text)) = &range.gt
                && same_kind(text)
                && q <= *gt
            {
                push(
                    out,
                    path,
                    DetailCode::OutOfRange,
                    format!("must be greater than {text}"),
                );
            }
        }
    }

    fn number(&self, n: &Node, x: f64, path: &mut DocPath, out: &mut Outcome) {
        if let Some(min) = n.minimum
            && x < min
        {
            push(
                out,
                path,
                DetailCode::OutOfRange,
                format!("must be >= {}", bound(min)),
            );
        }
        if let Some(max) = n.maximum
            && x > max
        {
            push(
                out,
                path,
                DetailCode::OutOfRange,
                format!("must be <= {}", bound(max)),
            );
        }
        if let Some(min) = n.exclusive_minimum
            && x <= min
        {
            push(
                out,
                path,
                DetailCode::OutOfRange,
                format!("must be > {}", bound(min)),
            );
        }
        if let Some(max) = n.exclusive_maximum
            && x >= max
        {
            push(
                out,
                path,
                DetailCode::OutOfRange,
                format!("must be < {}", bound(max)),
            );
        }
    }

    fn any_of(&self, branches: &[Schema], value: &Value, path: &mut DocPath, out: &mut Outcome) {
        let mut results = Vec::with_capacity(branches.len());
        for b in branches {
            let r = self.sub(b, value, path);
            if r.violations.is_empty() {
                out.secrets.extend(r.secrets);
                return;
            }
            results.push(r);
        }
        out.absorb(best_branch(results, path));
    }

    fn one_of(&self, branches: &[Schema], value: &Value, path: &mut DocPath, out: &mut Outcome) {
        let results: Vec<Outcome> = branches.iter().map(|b| self.sub(b, value, path)).collect();
        let valid: Vec<usize> = results
            .iter()
            .enumerate()
            .filter(|(_, r)| r.violations.is_empty())
            .map(|(i, _)| i)
            .collect();
        let required = required_only(branches);
        match valid.len() {
            1 => {
                if let Some(r) = valid.first().and_then(|&i| results.into_iter().nth(i)) {
                    out.secrets.extend(r.secrets);
                }
            }
            0 => match required {
                Some(names) => push(
                    out,
                    path,
                    DetailCode::Required,
                    format!("one of {} is required", names.join(", ")),
                ),
                None => out.absorb(best_branch(results, path)),
            },
            _ => {
                let message = match required {
                    Some(names) => format!("{} are mutually exclusive; set exactly one", names.join(" and ")),
                    None => String::from("the value matches more than one alternative"),
                };
                push(out, path, DetailCode::Conflict, message);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::compile::compile;
    use super::*;

    fn check(schema: Value, value: Value) -> Vec<(String, DetailCode)> {
        let c = compile(&schema).unwrap();
        validate(&c, &c.root, &value, &DocPath::root())
            .violations
            .into_iter()
            .map(|v| (v.path.to_string(), v.code))
            .collect()
    }

    #[test]
    fn unknown_fields_and_types() {
        let s = json!({"type": "object", "properties": {"a": {"type": "integer"}}, "additionalProperties": false});
        assert_eq!(
            check(s.clone(), json!({"a": 0.5, "b": 1})),
            vec![
                ("a".into(), DetailCode::TypeMismatch),
                ("b".into(), DetailCode::UnknownField)
            ]
        );
        assert!(check(s, json!({"a": 2.0})).is_empty());
    }

    #[test]
    fn one_of_required_conflict() {
        let s = json!({"type": "object", "oneOf": [{"required": ["route"]}, {"required": ["target"]}]});
        assert_eq!(
            check(s.clone(), json!({"route": 1, "target": 2})),
            vec![("".into(), DetailCode::Conflict)]
        );
        assert_eq!(check(s, json!({})), vec![("".into(), DetailCode::Required)]);
    }

    #[test]
    fn any_of_reports_the_branch_that_fits_the_type() {
        let s = json!({"$defs": {"D": {"type": "string", "pattern": "^[0-9]+s$"}},
                       "properties": {"t": {"anyOf": [{"$ref": "#/$defs/D"}, {"const": "none"}]},
                                      "m": {"anyOf": [{"type": "string"}, {"type": "integer"}]}}});
        assert_eq!(
            check(s.clone(), json!({"t": "10"})),
            vec![("t".into(), DetailCode::InvalidFormat)]
        );
        assert_eq!(
            check(s, json!({"m": 12.5})),
            vec![("m".into(), DetailCode::TypeMismatch)]
        );
    }

    #[test]
    fn x_range_and_reserved_names() {
        let s = json!({"properties": {
            "d": {"type": "string", "x-range": {"min": "1s"}},
            "n": {"type": "string", "not": {"enum": ["locked", "request_overridable"]}}}});
        assert_eq!(
            check(s, json!({"d": "500ms", "n": "locked"})),
            vec![
                ("d".into(), DetailCode::OutOfRange),
                ("n".into(), DetailCode::ReservedName)
            ]
        );
    }

    #[test]
    fn false_schema_names_the_field() {
        let s = json!({"properties": {"weight": false}});
        assert_eq!(
            check(s, json!({"weight": 1})),
            vec![("weight".into(), DetailCode::UnknownField)]
        );
    }

    #[test]
    fn secrets_are_recorded() {
        let s = json!({"$defs": {"S": {"type": "string", "x-secret": true}},
                       "properties": {"k": {"$ref": "#/$defs/S"},
                                      "o": {"anyOf": [{"$ref": "#/$defs/S"}, {"type": "null"}]}}});
        let c = compile(&s).unwrap();
        let out = validate(&c, &c.root, &json!({"k": "x", "o": "y"}), &DocPath::root());
        let paths: Vec<String> = out.secrets.iter().map(ToString::to_string).collect();
        assert_eq!(paths, vec!["k", "o"]);
    }
}
