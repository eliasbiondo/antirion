//! The operator URLs inside route trees: the webhooks of `external` classifiers and of `external`
//! cost-quality scorers, wherever a tree nests them (routes, Models' inline targets, mirrors).

use antirion_config::{ConfigError, DetailCode, DocPath};
use serde_json::Value;

use crate::ssrf::{EgressPolicy, TrustClass, validate_url};

fn check(url: Option<&Value>, at: DocPath, policy: &EgressPolicy, errors: &mut Vec<ConfigError>) {
    let Some(url) = url.and_then(Value::as_str) else {
        return;
    };
    if let Err(denied) = validate_url(url, TrustClass::Operator, policy) {
        errors.push(ConfigError::new(
            &at,
            DetailCode::InvalidFormat,
            format!("the URL is not allowed: {denied}"),
        ));
    }
}

fn walk(v: &Value, at: &DocPath, policy: &EgressPolicy, errors: &mut Vec<ConfigError>) {
    match v {
        Value::Object(o) => {
            match o.get("type").and_then(Value::as_str) {
                Some("classifier") => check(
                    o.get("external").and_then(|e| e.get("url")),
                    at.key("external").key("url"),
                    policy,
                    errors,
                ),
                Some("cost_quality") => check(
                    o.get("scorer").and_then(|s| s.get("url")),
                    at.key("scorer").key("url"),
                    policy,
                    errors,
                ),
                _ => {}
            }
            for (k, child) in o {
                if matches!(child, Value::Object(_) | Value::Array(_)) {
                    walk(child, &at.key(k), policy, errors);
                }
            }
        }
        Value::Array(a) => {
            for (i, child) in a.iter().enumerate() {
                walk(child, &at.index(i), policy, errors);
            }
        }
        _ => {}
    }
}

/// Checks every classifier and scorer webhook of the document's route trees.
pub(crate) fn check_document(doc: &Value, policy: &EgressPolicy, errors: &mut Vec<ConfigError>) {
    for (top, field) in [("routes", "tree"), ("models", "target")] {
        for (name, item) in doc.get(top).and_then(Value::as_object).into_iter().flatten() {
            if let Some(tree) = item.get(field) {
                walk(
                    tree,
                    &DocPath::root().key(top).key(name).key(field),
                    policy,
                    errors,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::config::compile_policy;

    #[test]
    fn webhooks_nested_anywhere_in_a_tree_are_checked() {
        let doc = json!({
            "routes": {"r": {"tree": {"type": "fallback", "children": [
                {"type": "classifier", "kind": "external", "external": {"url": "http://169.254.169.254/x"}},
                {"deployment": "d", "mirror": {"target": {"type": "cost_quality",
                    "scorer": {"kind": "external", "url": "http://metadata.google.internal/s"}}}}
            ]}}},
            "models": {"m": {"target": {"type": "classifier", "kind": "external",
                                        "external": {"url": "http://127.0.0.1:9100/ok"}}}}
        });
        let policy = compile_policy(&doc).unwrap();
        let mut errors = Vec::new();
        check_document(&doc, &policy, &mut errors);
        let paths: Vec<String> = errors.iter().map(|e| e.path.clone()).collect();
        assert_eq!(
            paths,
            [
                "routes.r.tree.children[0].external.url",
                "routes.r.tree.children[1].mirror.target.scorer.url"
            ]
        );
    }
}
