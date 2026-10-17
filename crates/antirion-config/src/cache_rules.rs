//! Rules of the cache policy the schema cannot express: the TTL within `max_ttl` as merged down the
//! policy chain, the shared scope and its namespace, and the semantic layer's embedding Model.

use std::time::Duration;

use serde_json::Value;

use crate::scalar::parse_duration;
use crate::semantic::policy_levels;
use crate::{ConfigError, DetailCode};

/// The value of `field` in the narrowest of `blocks` (broadest first) that sets it.
fn merged<'a>(blocks: &[&'a Value], field: &str) -> Option<&'a Value> {
    blocks.iter().rev().find_map(|b| b.pointer(field))
}

fn duration(v: Option<&Value>, default: Duration) -> Duration {
    v.and_then(Value::as_str)
        .and_then(parse_duration)
        .unwrap_or(default)
}

pub(crate) fn check(doc: &Value, errors: &mut Vec<ConfigError>) {
    for level in policy_levels(doc) {
        let Some(own) = level.block.get("cache") else {
            continue;
        };
        let mut chain: Vec<&Value> = level
            .ancestors
            .iter()
            .filter_map(|p| p.get(doc)?.get("cache"))
            .collect();
        chain.push(own);
        let at = level.path.key("cache");
        if own.get("ttl").is_some() || own.get("max_ttl").is_some() {
            let ttl = duration(merged(&chain, "/ttl"), Duration::from_secs(3600));
            let max = duration(merged(&chain, "/max_ttl"), Duration::from_secs(7 * 86_400));
            if ttl > max {
                let field = if own.get("ttl").is_some() {
                    "ttl"
                } else {
                    "max_ttl"
                };
                errors.push(ConfigError::new(
                    &at.key(field),
                    DetailCode::OutOfRange,
                    "ttl must not exceed max_ttl",
                ));
            }
        }
        let shared = merged(&chain, "/scope").and_then(Value::as_str) == Some("shared");
        let gateway_or_org = matches!(
            level.path.segments().first(),
            Some(crate::Seg::Key(k)) if k == "policies" || k == "organizations"
        );
        if own.get("scope").and_then(Value::as_str) == Some("shared") && !gateway_or_org {
            errors.push(ConfigError::new(
                &at.key("scope"),
                DetailCode::Conflict,
                "scope shared is allowed only at the gateway or organization level",
            ));
        }
        let namespace = merged(&chain, "/shared_namespace").is_some_and(|v| !v.is_null());
        if own.get("shared_namespace").is_some_and(|v| !v.is_null()) && !shared {
            errors.push(ConfigError::new(
                &at.key("shared_namespace"),
                DetailCode::Conflict,
                "shared_namespace needs scope shared",
            ));
        }
        if own.get("scope").is_some() && shared && !namespace {
            errors.push(ConfigError::new(
                &at.key("shared_namespace"),
                DetailCode::Required,
                "scope shared needs a shared_namespace",
            ));
        }
        if own.pointer("/semantic/enabled") == Some(&Value::Bool(true)) {
            let model = merged(&chain, "/semantic/embedding_model").and_then(Value::as_str);
            let known = model.is_some_and(|m| doc.get("models").and_then(|ms| ms.get(m)).is_some());
            if !known {
                errors.push(ConfigError::new(
                    &at.key("semantic").key("embedding_model"),
                    DetailCode::Required,
                    "the semantic cache needs an embedding_model naming an existing Model",
                ));
            }
            if doc.pointer("/cache/semantic_store").is_none() {
                errors.push(ConfigError::new(
                    &crate::DocPath::root().key("cache").key("semantic_store"),
                    DetailCode::Required,
                    "the semantic cache needs cache.semantic_store",
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn paths(doc: &Value) -> Vec<String> {
        let mut errors = Vec::new();
        check(doc, &mut errors);
        errors.into_iter().map(|e| e.path).collect()
    }

    #[test]
    fn ttl_stays_within_max_ttl_down_the_chain() {
        assert_eq!(
            paths(&json!({"policies": {"cache": {"ttl": "8d", "max_ttl": "7d"}}})),
            vec!["policies.cache.ttl"]
        );
        assert_eq!(
            paths(&json!({"policies": {"cache": {"max_ttl": "2h"}},
                "projects": {"p": {"policies": {"cache": {"ttl": "3h"}}}}})),
            vec!["projects.p.policies.cache.ttl"]
        );
        assert!(paths(&json!({"policies": {"cache": {"ttl": "1h", "max_ttl": "1h"}}})).is_empty());
    }

    #[test]
    fn shared_scope_rules() {
        assert_eq!(
            paths(
                &json!({"projects": {"p": {"policies": {"cache": {"scope": "shared", "shared_namespace": "x"}}}}})
            ),
            vec!["projects.p.policies.cache.scope"]
        );
        assert_eq!(
            paths(&json!({"policies": {"cache": {"shared_namespace": "x"}}})),
            vec!["policies.cache.shared_namespace"]
        );
        assert_eq!(
            paths(&json!({"policies": {"cache": {"scope": "shared"}}})),
            vec!["policies.cache.shared_namespace"]
        );
        assert!(paths(&json!({"organizations": {"o": {"policies": {"cache": {"scope": "shared", "shared_namespace": "x"}}}}})).is_empty());
    }

    #[test]
    fn semantic_needs_a_model() {
        let got = paths(&json!({"policies": {"cache": {"semantic": {"enabled": true}}}}));
        assert!(
            got.contains(&"policies.cache.semantic.embedding_model".to_owned()),
            "{got:?}"
        );
    }
}
