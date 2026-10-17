//! JSON Schema converted to the OpenAPI subset of Gemini's `parameters` and `responseSchema`, and
//! that subset read back as JSON Schema.

use serde_json::{Map, Value};

const MAX_DEPTH: usize = 16;
const REMOVED: [&str; 9] = [
    "$schema",
    "$id",
    "$comment",
    "additionalProperties",
    "patternProperties",
    "$defs",
    "definitions",
    "examples",
    "default",
];

/// A schema the subset cannot express (a cyclic or too deep `$ref`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inexpressible;

/// The converted schema, and whether anything changed.
pub fn to_subset(schema: &Value) -> Result<(Value, bool), Inexpressible> {
    let defs = schema
        .get("$defs")
        .or_else(|| schema.get("definitions"))
        .cloned()
        .unwrap_or(Value::Null);
    let mut changed = false;
    let out = convert(schema, &defs, &mut Vec::new(), &mut changed)?;
    Ok((out, changed))
}

fn resolve<'a>(reference: &str, defs: &'a Value) -> Option<&'a Value> {
    let name = reference
        .strip_prefix("#/$defs/")
        .or_else(|| reference.strip_prefix("#/definitions/"))?;
    defs.get(name)
}

fn convert(
    v: &Value,
    defs: &Value,
    stack: &mut Vec<String>,
    changed: &mut bool,
) -> Result<Value, Inexpressible> {
    let Value::Object(map) = v else {
        return Ok(v.clone());
    };
    if let Some(r) = map.get("$ref").and_then(Value::as_str) {
        *changed = true;
        if stack.len() >= MAX_DEPTH || stack.iter().any(|s| s == r) {
            return Err(Inexpressible);
        }
        let target = resolve(r, defs).ok_or(Inexpressible)?;
        stack.push(r.to_owned());
        let out = convert(target, defs, stack, changed);
        stack.pop();
        return out;
    }
    let mut out = Map::new();
    for (k, val) in map {
        if REMOVED.contains(&k.as_str()) {
            *changed = true;
            continue;
        }
        match k.as_str() {
            "const" => {
                *changed = true;
                out.insert("enum".into(), Value::Array(vec![val.clone()]));
            }
            "anyOf" | "oneOf" => {
                let branches = val.as_array().cloned().unwrap_or_default();
                let null = |b: &Value| b.get("type").and_then(Value::as_str) == Some("null");
                let rest: Vec<&Value> = branches.iter().filter(|b| !null(b)).collect();
                if rest.len() < branches.len() {
                    *changed = true;
                    out.insert("nullable".into(), Value::Bool(true));
                    if let [only] = rest.as_slice() {
                        if let Value::Object(inner) = convert(only, defs, stack, changed)? {
                            for (ik, iv) in inner {
                                out.insert(ik, iv);
                            }
                        }
                        continue;
                    }
                }
                let converted = rest
                    .into_iter()
                    .map(|b| convert(b, defs, stack, changed))
                    .collect::<Result<Vec<_>, _>>()?;
                out.insert("anyOf".into(), Value::Array(converted));
            }
            "type" if val.is_array() => {
                let types: Vec<&str> = val
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                let non_null: Vec<&str> = types.iter().copied().filter(|t| *t != "null").collect();
                if non_null.len() < types.len() {
                    *changed = true;
                    out.insert("nullable".into(), Value::Bool(true));
                }
                match non_null.as_slice() {
                    [one] => {
                        out.insert("type".into(), Value::from(*one));
                    }
                    _ => {
                        out.insert("type".into(), val.clone());
                    }
                }
            }
            "properties" => {
                let mut props = Map::new();
                for (pk, pv) in val.as_object().into_iter().flatten() {
                    props.insert(pk.clone(), convert(pv, defs, stack, changed)?);
                }
                out.insert(k.clone(), Value::Object(props));
            }
            "items" | "not" => {
                out.insert(k.clone(), convert(val, defs, stack, changed)?);
            }
            "allOf" | "prefixItems" => {
                let converted = val
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|b| convert(b, defs, stack, changed))
                    .collect::<Result<Vec<_>, _>>()?;
                out.insert(k.clone(), Value::Array(converted));
            }
            _ => {
                out.insert(k.clone(), val.clone());
            }
        }
    }
    Ok(Value::Object(out))
}

/// A schema of Gemini's OpenAPI subset as JSON Schema: type names in lower case, `nullable` as a
/// `null` type, and the subset's own ordering hint removed.
#[must_use]
pub fn from_subset(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let nullable = map.get("nullable") == Some(&Value::Bool(true));
            let mut out = Map::new();
            for (k, val) in map {
                match k.as_str() {
                    "nullable" | "propertyOrdering" => {}
                    "type" => {
                        let t = val
                            .as_str()
                            .map_or_else(|| val.clone(), |t| Value::from(t.to_ascii_lowercase()));
                        let t = match t {
                            Value::String(s) if nullable => {
                                Value::Array(vec![Value::from(s), Value::from("null")])
                            }
                            other => other,
                        };
                        out.insert(k.clone(), t);
                    }
                    "properties" => {
                        let props = val
                            .as_object()
                            .into_iter()
                            .flatten()
                            .map(|(pk, pv)| (pk.clone(), from_subset(pv)))
                            .collect();
                        out.insert(k.clone(), Value::Object(props));
                    }
                    "items" | "anyOf" => {
                        out.insert(k.clone(), from_subset(val));
                    }
                    _ => {
                        out.insert(k.clone(), val.clone());
                    }
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(from_subset).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn subset() {
        let s = json!({"type": "object", "$defs": {"City": {"type": "string"}},
            "properties": {"city": {"$ref": "#/$defs/City"}, "unit": {"anyOf": [{"type": "string"}, {"type": "null"}]},
                           "kind": {"const": "x"}},
            "additionalProperties": false, "required": ["city"]});
        let (out, changed) = to_subset(&s).unwrap();
        assert!(changed);
        assert_eq!(out["properties"]["city"], json!({"type": "string"}));
        assert_eq!(out["properties"]["kind"], json!({"enum": ["x"]}));
        assert_eq!(
            out["properties"]["unit"],
            json!({"nullable": true, "type": "string"})
        );
        assert!(out.get("$defs").is_none() && out.get("additionalProperties").is_none());
        let cyclic = json!({"$defs": {"N": {"properties": {"n": {"$ref": "#/$defs/N"}}}}, "properties": {"r": {"$ref": "#/$defs/N"}}});
        assert_eq!(to_subset(&cyclic), Err(Inexpressible));
        let (same, changed) = to_subset(&json!({"type": "object"})).unwrap();
        assert!(!changed && same == json!({"type": "object"}));
    }

    #[test]
    fn subset_read_back() {
        let s = json!({"type": "OBJECT", "properties": {"city": {"type": "STRING", "nullable": true},
                       "tags": {"type": "ARRAY", "items": {"type": "STRING"}}}, "propertyOrdering": ["city"],
                       "required": ["city"]});
        assert_eq!(
            from_subset(&s),
            json!({"type": "object", "properties": {"city": {"type": ["string", "null"]},
                   "tags": {"type": "array", "items": {"type": "string"}}}, "required": ["city"]})
        );
    }
}
