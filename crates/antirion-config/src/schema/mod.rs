//! The published config schema, embedded verbatim from `schema/antirion-config.schema.json`, and its
//! evaluation.

mod combinators;
mod compile;
mod validate;

use std::sync::OnceLock;

use serde_json::Value;

use crate::{ConfigError, DetailCode, DocPath};

/// The config schema served at `GET /admin/v1/config/schema` and printed by `antirion schema`.
pub static JSON_SCHEMA: &str = include_str!("../../../../schema/antirion-config.schema.json");

/// The version of the config document this build reads and writes.
pub const CURRENT_VERSION: u64 = 1;

fn compiled() -> Result<&'static compile::Compiled, String> {
    static COMPILED: OnceLock<Result<compile::Compiled, String>> = OnceLock::new();
    COMPILED
        .get_or_init(|| {
            let mut doc: Value = serde_json::from_str(JSON_SCHEMA).map_err(|e| e.to_string())?;
            secret_header_values(&mut doc);
            compile::compile(&doc).map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// The values of a `HeaderValues` map authenticate the gateway to operator services (webhooks,
/// sinks, exporters, MCP servers, agents): they are secrets, redacted on every read.
fn secret_header_values(doc: &mut Value) {
    if let Some(Value::Object(item)) = doc.pointer_mut("/$defs/HeaderValues/additionalProperties") {
        item.insert("x-secret".into(), Value::Bool(true));
    }
}

/// How deep a schema from an operator or a request may nest while it is evaluated.
const USER_SCHEMA_DEPTH: u32 = 128;

/// A JSON Schema written by an operator or sent in a request (draft 2020-12, the keywords this
/// evaluator implements; others are ignored), compiled once. Patterns use the linear-time regex
/// engine, and evaluation nests at most a fixed depth, so a schema cannot make it loop.
#[derive(Debug)]
pub struct UserSchema(compile::Compiled);

impl UserSchema {
    /// Compiles `schema`; an unsupported `$ref` (only `#/$defs/<name>` is) or an invalid pattern
    /// is an error.
    pub fn compile(schema: &Value) -> Result<UserSchema, String> {
        compile::compile(schema)
            .map(UserSchema)
            .map_err(|e| e.to_string())
    }

    /// Whether `value` satisfies the schema.
    #[must_use]
    pub fn accepts(&self, value: &Value) -> bool {
        validate::validate_within(&self.0, &self.0.root, value, &DocPath::root(), USER_SCHEMA_DEPTH)
            .violations
            .is_empty()
    }

    /// The first place `value` fails the schema (its path, empty at the root) and why.
    #[must_use]
    pub fn first_violation(&self, value: &Value) -> Option<(String, String)> {
        validate::validate_within(&self.0, &self.0.root, value, &DocPath::root(), USER_SCHEMA_DEPTH)
            .violations
            .into_iter()
            .next()
            .map(|v| (v.path.to_string(), v.message))
    }
}

/// Compiles the embedded schema; called once at startup so a broken build fails before serving.
pub fn self_check() -> Result<(), String> {
    compiled().map(|_| ())
}

/// The schema's verdict on a document.
#[derive(Debug, Default)]
pub struct SchemaCheck {
    pub errors: Vec<ConfigError>,
    /// Paths of every secret field present in the document.
    pub secret_paths: Vec<DocPath>,
}

/// Checks a whole document against the schema.
#[must_use]
pub fn check_document(doc: &Value) -> SchemaCheck {
    let compiled = match compiled() {
        Ok(c) => c,
        Err(e) => return unavailable(e),
    };
    let out = validate::validate(compiled, &compiled.root, doc, &DocPath::root());
    let version = DocPath::root().key("version");
    let errors = out
        .violations
        .into_iter()
        .map(|v| {
            if v.path == version {
                ConfigError::new(
                    &v.path,
                    DetailCode::UnsupportedVersion,
                    format!("version must be {CURRENT_VERSION}, the version this build supports"),
                )
            } else {
                ConfigError::new(&v.path, v.code, v.message)
            }
        })
        .collect();
    SchemaCheck {
        errors,
        secret_paths: out.secrets,
    }
}

/// Checks one top-level key of a document fragment against that key's own schema, without the rules
/// that tie several top-level keys together.
#[must_use]
pub fn check_top_level(key: &str, value: &Value) -> SchemaCheck {
    let compiled = match compiled() {
        Ok(c) => c,
        Err(e) => return unavailable(e),
    };
    let compile::Schema::Node(root) = &compiled.root else {
        return unavailable(String::from("the schema root is not an object schema"));
    };
    let at = DocPath::root().key(key);
    let Some((_, schema)) = root.properties.iter().find(|(k, _)| k == key) else {
        return SchemaCheck {
            errors: vec![ConfigError::new(
                &at,
                DetailCode::UnknownField,
                format!("unknown field {key:?}"),
            )],
            secret_paths: Vec::new(),
        };
    };
    let out = validate::validate(compiled, schema, value, &at);
    SchemaCheck {
        errors: out
            .violations
            .into_iter()
            .map(|v| ConfigError::new(&v.path, v.code, v.message))
            .collect(),
        secret_paths: out.secrets,
    }
}

/// The top-level keys the schema defines.
#[must_use]
pub fn top_level_keys() -> Vec<String> {
    match compiled() {
        Ok(c) => match &c.root {
            compile::Schema::Node(root) => root.properties.iter().map(|(k, _)| k.clone()).collect(),
            compile::Schema::Bool(_) => Vec::new(),
        },
        Err(_) => Vec::new(),
    }
}

fn unavailable(reason: String) -> SchemaCheck {
    SchemaCheck {
        errors: vec![ConfigError::new(
            &DocPath::root(),
            DetailCode::MalformedDocument,
            reason,
        )],
        secret_paths: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn user_schemas_validate_and_never_recurse_without_end() {
        let s = UserSchema::compile(&json!({"type": "object", "required": ["a"],
            "properties": {"a": {"type": "integer", "minimum": 1}}}))
        .unwrap();
        assert!(s.accepts(&json!({"a": 1})));
        assert!(!s.accepts(&json!({"b": 1})));
        assert!(!s.accepts(&json!({"a": 0})));
        let looped =
            UserSchema::compile(&json!({"$defs": {"x": {"$ref": "#/$defs/x"}}, "$ref": "#/$defs/x"}))
                .unwrap();
        assert!(!looped.accepts(&json!(1)));
        assert!(UserSchema::compile(&json!({"$ref": "https://example.com/s.json"})).is_err());
        assert!(UserSchema::compile(&json!({"pattern": "(a)\\1"})).is_err());
    }

    fn codes(doc: &Value) -> Vec<(String, DetailCode)> {
        check_document(doc)
            .errors
            .into_iter()
            .map(|e| (e.path, e.code))
            .collect()
    }

    fn base() -> Value {
        json!({
            "version": 1,
            "providers": {"p": {"type": "openai", "base_url": "http://127.0.0.1:9/v1",
                                "auth": {"method": "api_key", "api_key": "sk-x"}}},
            "deployments": {"p": {"provider": "p", "model": "up"}},
            "models": {"m": {"target": {"deployment": "p"}}}
        })
    }

    #[test]
    fn the_embedded_schema_compiles() {
        self_check().unwrap();
    }

    #[test]
    fn a_minimal_document_is_valid_and_its_secret_is_found() {
        let check = check_document(&base());
        assert!(check.errors.is_empty(), "{:?}", check.errors);
        let paths: Vec<String> = check.secret_paths.iter().map(ToString::to_string).collect();
        assert_eq!(paths, vec!["providers.p.auth.api_key"]);
    }

    #[test]
    fn version_errors_are_unsupported_version() {
        let mut doc = base();
        doc["version"] = json!(999);
        assert_eq!(
            codes(&doc),
            vec![("version".into(), DetailCode::UnsupportedVersion)]
        );
        doc.as_object_mut().unwrap().remove("version");
        assert_eq!(
            codes(&doc),
            vec![("version".into(), DetailCode::UnsupportedVersion)]
        );
    }

    #[test]
    fn independent_errors_are_all_reported_once() {
        let mut doc = base();
        doc["deployments"]["second"] = json!({"provider": "p", "model": "other"});
        doc["routes"] = json!({"r": {"tree": {"type": "weighted", "children": [
            {"deployment": "p", "weight": 1}, {"deployment": "second", "weight": -1}]}}});
        doc["policies"] = json!({"resilience": {"timeouts": {"connect": "10"}}});
        assert_eq!(
            codes(&doc),
            vec![
                (
                    "policies.resilience.timeouts.connect".into(),
                    DetailCode::InvalidFormat
                ),
                ("routes.r.tree.children[1].weight".into(), DetailCode::OutOfRange),
            ]
        );
    }

    #[test]
    fn scalar_formats() {
        let mut doc = base();
        doc["observability"] = json!({"traces": {"sampler": {"ratio": 1.5}}});
        assert_eq!(
            codes(&doc),
            vec![(
                "observability.traces.sampler.ratio".into(),
                DetailCode::OutOfRange
            )]
        );
        doc["observability"] = json!({"traces": {"sampler": {"ratio": "0.2"}}});
        assert_eq!(
            codes(&doc),
            vec![(
                "observability.traces.sampler.ratio".into(),
                DetailCode::TypeMismatch
            )]
        );
        let mut doc = base();
        doc["policies"] = json!({"budgets": {"b": {"unit": "usd", "amount": 12.5, "period": "monthly"}}});
        let got = codes(&doc);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].0, "policies.budgets.b.amount");
    }

    #[test]
    fn enums_name_the_allowed_values() {
        let mut doc = base();
        doc["routes"] = json!({"r": {"tree": {"type": "weighted", "algorithm": "round_robin",
                                              "children": [{"deployment": "p", "weight": 1}]}}});
        let check = check_document(&doc);
        assert_eq!(check.errors.len(), 1, "{:?}", check.errors);
        assert_eq!(check.errors[0].path, "routes.r.tree.algorithm");
        assert!(check.errors[0].message.contains("smooth_wrr") && check.errors[0].message.contains("random"));
    }

    #[test]
    fn mutually_exclusive_and_required_fields() {
        let mut doc = base();
        doc["routes"] = json!({"r": {"tree": "p"}});
        doc["models"]["both"] = json!({"route": "r", "target": {"deployment": "p"}});
        doc["deployments"]["d2"] = json!({"provider": "p"});
        assert_eq!(
            codes(&doc),
            vec![
                ("deployments.d2.model".into(), DetailCode::Required),
                ("models.both".into(), DetailCode::Conflict),
            ]
        );
    }

    #[test]
    fn former_policy_locations_are_unknown_fields() {
        for (key, value, path) in [
            ("cache", json!({"policy": {"enabled": true}}), "cache.policy"),
            ("limits", json!({"gateway": {}}), "limits.gateway"),
            ("frobnicate", json!({}), "frobnicate"),
        ] {
            let mut doc = base();
            doc[key] = value;
            assert_eq!(
                codes(&doc),
                vec![(path.to_owned(), DetailCode::UnknownField)],
                "{key}"
            );
        }
    }

    #[test]
    fn fractional_weight_is_rejected() {
        let mut doc = base();
        doc["routes"] = json!({"r": {"tree": {"type": "weighted", "children": [
            {"deployment": "p", "weight": 1}, {"deployment": "p", "weight": 0.5}]}}});
        let got = codes(&doc);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].0, "routes.r.tree.children[1].weight");
    }
}
