//! `auth.access_rules`: for a request whose model a rule names, every condition of every such rule
//! must hold. A rule names the model when its patterns match the requested name or the Model it
//! resolved to, so an alias never steps around a rule written for the Model.

use std::collections::BTreeMap;

use antirion_identity::access::pattern_matches;
use serde_json::{Map, Value};

/// One compiled rule.
#[derive(Clone, Debug, PartialEq)]
pub struct AccessRule {
    models: Vec<String>,
    claims: Option<Map<String, Value>>,
    key_tags: Vec<String>,
    /// Lowercase header names and the values any of which satisfies them.
    headers: Vec<(String, Vec<String>)>,
}

/// What a request offers the rules.
#[derive(Debug)]
pub struct RuleFacts<'a> {
    pub requested: &'a str,
    pub model: &'a str,
    /// The claims of the JWT the request was authenticated with; `None` for a secret key.
    pub claims: Option<&'a Map<String, Value>>,
    /// The tags configured on the key and its tenancy levels (never the request's own tags).
    pub key_tags: &'a [String],
    pub headers: &'a http::HeaderMap,
}

fn strings(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s.clone()],
        Value::Array(list) => list.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
        _ => Vec::new(),
    }
}

impl AccessRule {
    pub(crate) fn of(v: &Value, groups: &BTreeMap<String, Vec<String>>) -> Option<AccessRule> {
        let models = v
            .get("models")?
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .flat_map(|p| match p.strip_prefix("group:") {
                Some(g) => groups.get(g).cloned().unwrap_or_default(),
                None => vec![p.to_owned()],
            })
            .collect();
        let require = v.get("require")?;
        Some(AccessRule {
            models,
            claims: require.get("claims").and_then(Value::as_object).cloned(),
            key_tags: require.get("key_tags").map(strings).unwrap_or_default(),
            headers: require
                .get("headers")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .map(|(k, v)| (k.to_ascii_lowercase(), strings(v)))
                .collect(),
        })
    }

    fn names(&self, f: &RuleFacts<'_>) -> bool {
        self.models
            .iter()
            .any(|p| pattern_matches(p, f.requested) || pattern_matches(p, f.model))
    }

    fn holds(&self, f: &RuleFacts<'_>) -> bool {
        let claims_ok = match (&self.claims, f.claims) {
            (None, _) => true,
            (Some(want), Some(have)) => antirion_identity::jwt::matches(have, want),
            (Some(_), None) => false,
        };
        let tags_ok = self.key_tags.iter().all(|t| f.key_tags.contains(t));
        let headers_ok = self.headers.iter().all(|(name, values)| {
            f.headers
                .get_all(name.as_str())
                .iter()
                .filter_map(|v| v.to_str().ok())
                .any(|v| values.iter().any(|want| want == v))
        });
        claims_ok && tags_ok && headers_ok
    }
}

/// Whether some rule that names the request's model is not satisfied.
#[must_use]
pub fn denies(rules: &[AccessRule], f: &RuleFacts<'_>) -> bool {
    rules.iter().any(|r| r.names(f) && !r.holds(f))
}

#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderValue};
    use serde_json::json;

    use super::*;

    fn rule(v: Value) -> AccessRule {
        let groups = BTreeMap::from([("g".to_owned(), vec!["grouped".to_owned()])]);
        AccessRule::of(&v, &groups).unwrap()
    }

    fn facts<'a>(
        headers: &'a HeaderMap,
        claims: Option<&'a Map<String, Value>>,
        tags: &'a [String],
    ) -> RuleFacts<'a> {
        RuleFacts {
            requested: "alias",
            model: "direct-model",
            claims,
            key_tags: tags,
            headers,
        }
    }

    #[test]
    fn headers_claims_and_tags() {
        let rules = vec![rule(
            json!({"models": ["direct-model"], "require": {"headers": {"X-Team": ["ml", "ai"]}}}),
        )];
        let mut h = HeaderMap::new();
        assert!(denies(&rules, &facts(&h, None, &[])));
        h.insert("x-team", HeaderValue::from_static("ai"));
        assert!(!denies(&rules, &facts(&h, None, &[])));
        let claim_rule = vec![rule(
            json!({"models": ["alias"], "require": {"claims": {"groups": "ml"}}}),
        )];
        assert!(denies(&claim_rule, &facts(&h, None, &[])));
        let claims = json!({"groups": ["x", "ml"]});
        assert!(!denies(&claim_rule, &facts(&h, claims.as_object(), &[])));
        let tag_rule = vec![rule(
            json!({"models": ["*"], "require": {"key_tags": ["env:prod"]}}),
        )];
        assert!(denies(&tag_rule, &facts(&h, None, &[])));
        assert!(!denies(&tag_rule, &facts(&h, None, &["env:prod".to_owned()])));
    }

    #[test]
    fn rules_for_other_models_and_groups() {
        let rules = vec![rule(
            json!({"models": ["group:g"], "require": {"key_tags": ["t"]}}),
        )];
        let h = HeaderMap::new();
        assert!(!denies(&rules, &facts(&h, None, &[])));
        let f = RuleFacts {
            requested: "grouped",
            ..facts(&h, None, &[])
        };
        assert!(denies(&rules, &f));
    }
}
