//! Rules of the identity configuration the schema cannot express: declared key secrets must be
//! references, IP ranges must have their host bits zero, `set:` entries must name an IP set,
//! `group:` patterns must name a model group, and tags in the gateway's reserved `antirion.`
//! namespace cannot be configured.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::scalar::{Cidr, CidrError};
use crate::secrets::has_reference;
use crate::{ConfigError, DetailCode, DocPath};

const TENANCY: [&str; 4] = ["organizations", "teams", "projects", "keys"];
const MODEL_LISTS: [&str; 2] = ["models", "deny_models"];
const RESERVED_TAG_PREFIX: &str = "antirion.";

fn reserved_tag(tag: &str) -> bool {
    tag.get(..RESERVED_TAG_PREFIX.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(RESERVED_TAG_PREFIX))
}

fn unknown_group(groups: Option<&serde_json::Map<String, Value>>, pattern: &str) -> Option<String> {
    let group = pattern.strip_prefix("group:")?;
    (!groups.is_some_and(|g| g.contains_key(group))).then(|| format!("model group {group:?} does not exist"))
}

fn strings(v: Option<&Value>) -> impl Iterator<Item = (usize, &str)> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(i, s)| s.as_str().map(|s| (i, s)))
}

/// The prefix of an `allowed_ips` entry that names an IP set of `auth.ip_sets`.
pub const IP_SET_PREFIX: &str = "set:";

fn range_error(range: &str, at: &DocPath, errors: &mut Vec<ConfigError>) {
    let message = match Cidr::parse(range) {
        Ok(_) => return,
        Err(CidrError::HostBitsSet) => "the range has host bits set",
        Err(CidrError::Malformed) => "not an IP address or CIDR range",
    };
    errors.push(ConfigError::new(at, DetailCode::InvalidFormat, message));
}

/// The members of `auth` that organizations, teams, projects and keys name: IP sets (`set:<name>`
/// in `allowed_ips`) as `("ip_sets", name)` and model groups (`group:<name>` in their Model lists)
/// as `("model_groups", name)`.
#[must_use]
pub fn auth_references(doc: &Value) -> BTreeSet<(&'static str, String)> {
    let mut named = BTreeSet::new();
    for collection in TENANCY {
        for spec in doc
            .get(collection)
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|m| m.values())
        {
            for (_, entry) in strings(spec.get("allowed_ips")) {
                if let Some(set) = entry.strip_prefix(IP_SET_PREFIX) {
                    named.insert(("ip_sets", set.to_owned()));
                }
            }
            for list in MODEL_LISTS {
                for (_, pattern) in strings(spec.get(list)) {
                    if let Some(group) = pattern.strip_prefix("group:") {
                        named.insert(("model_groups", group.to_owned()));
                    }
                }
            }
        }
    }
    named
}

pub(crate) fn check(doc: &Value, errors: &mut Vec<ConfigError>) {
    let root = DocPath::root();
    let groups = doc.pointer("/auth/model_groups").and_then(Value::as_object);
    let ip_sets = doc.pointer("/auth/ip_sets").and_then(Value::as_object);
    for (name, ranges) in ip_sets.into_iter().flatten() {
        for (i, range) in strings(Some(ranges)) {
            range_error(range, &root.key("auth").key("ip_sets").key(name).index(i), errors);
        }
    }
    for collection in TENANCY {
        for (name, spec) in doc
            .get(collection)
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            let at = root.key(collection).key(name);
            if collection == "keys"
                && let Some(secret) = spec.get("secret").and_then(Value::as_str)
                && !has_reference(secret)
            {
                errors.push(ConfigError::new(
                    &at.key("secret"),
                    DetailCode::SecretRequired,
                    "a declared key's secret must be a secret reference, never a literal value",
                ));
            }
            for (i, range) in strings(spec.get("allowed_ips")) {
                let at = at.key("allowed_ips").index(i);
                match range.strip_prefix(IP_SET_PREFIX) {
                    Some(set) if !ip_sets.is_some_and(|s| s.contains_key(set)) => {
                        errors.push(ConfigError::new(
                            &at,
                            DetailCode::UnresolvedReference,
                            format!("IP set {set:?} does not exist"),
                        ))
                    }
                    Some(_) => {}
                    None => range_error(range, &at, errors),
                }
            }
            for list in MODEL_LISTS {
                for (i, pattern) in strings(spec.get(list)) {
                    if let Some(message) = unknown_group(groups, pattern) {
                        errors.push(ConfigError::new(
                            &at.key(list).index(i),
                            DetailCode::UnresolvedReference,
                            message,
                        ));
                    }
                }
            }
            for (i, tag) in strings(spec.get("tags")) {
                if reserved_tag(tag) {
                    errors.push(ConfigError::new(
                        &at.key("tags").index(i),
                        DetailCode::ReservedName,
                        "tags in the antirion. namespace are reserved for the gateway",
                    ));
                }
            }
        }
    }
    let rules = doc.pointer("/auth/access_rules").and_then(Value::as_array);
    for (r, rule) in rules.into_iter().flatten().enumerate() {
        for (i, pattern) in strings(rule.get("models")) {
            if let Some(message) = unknown_group(groups, pattern) {
                errors.push(ConfigError::new(
                    &root
                        .key("auth")
                        .key("access_rules")
                        .index(r)
                        .key("models")
                        .index(i),
                    DetailCode::UnresolvedReference,
                    message,
                ));
            }
        }
    }
    for (name, patterns) in groups.into_iter().flatten() {
        for (i, pattern) in strings(Some(patterns)) {
            if pattern.starts_with("group:") {
                errors.push(ConfigError::new(
                    &root.key("auth").key("model_groups").key(name).index(i),
                    DetailCode::InvalidFormat,
                    "model groups cannot contain other groups",
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn references_into_auth_are_listed_by_map() {
        let doc = json!({
            "auth": {"access_rules": [{"models": ["group:only-rules"]}]},
            "keys": {"k": {"allowed_ips": ["set:office", "192.0.2.0/24"], "models": ["group:fast", "m"]}},
            "teams": {"t": {"deny_models": ["group:slow"]}}
        });
        let named: Vec<(&str, String)> = auth_references(&doc).into_iter().collect();
        assert_eq!(
            named,
            vec![
                ("ip_sets", "office".to_owned()),
                ("model_groups", "fast".to_owned()),
                ("model_groups", "slow".to_owned()),
            ]
        );
    }

    fn paths(doc: &Value) -> Vec<String> {
        let mut errors = Vec::new();
        check(doc, &mut errors);
        errors.iter().map(|e| e.path.clone()).collect()
    }

    #[test]
    fn ip_set_entries_must_name_a_set_and_sets_hold_ranges() {
        let doc = json!({
            "auth": {"ip_sets": {"office": ["203.0.113.0/24"], "bad": ["10.0.0.1/8"]}},
            "keys": {"k": {"allowed_ips": ["set:office", "set:ghost", "192.0.2.0/24"]}}
        });
        assert_eq!(
            paths(&doc),
            vec![
                "auth.ip_sets.bad[0]".to_owned(),
                "keys.k.allowed_ips[1]".to_owned()
            ]
        );
    }

    #[test]
    fn literal_key_secrets_are_rejected_without_the_value() {
        let doc = json!({"keys": {"k": {"secret": "sk-antirion-literal"}, "r": {"secret": "${env:K}"}}});
        let mut errors = Vec::new();
        check(&doc, &mut errors);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].path, "keys.k.secret");
        assert!(!errors[0].message.contains("literal-"));
    }

    #[test]
    fn ranges_and_groups() {
        let doc = json!({
            "auth": {"model_groups": {"cheap": ["m1"], "bad": ["group:cheap"]}},
            "organizations": {"o": {"allowed_ips": ["10.0.0.0/8", "10.0.0.1/8"], "models": ["group:cheap", "group:nope"]}}
        });
        assert_eq!(
            paths(&doc),
            vec![
                "organizations.o.allowed_ips[1]",
                "organizations.o.models[1]",
                "auth.model_groups.bad[0]"
            ]
        );
    }

    #[test]
    fn reserved_tags_and_rule_groups() {
        let doc = json!({
            "auth": {"model_groups": {"g": ["m"]},
                     "access_rules": [{"models": ["group:g", "group:x"], "require": {}}]},
            "keys": {"k": {"tags": ["env:prod", "Antirion.cost"]}}
        });
        assert_eq!(
            paths(&doc),
            vec!["keys.k.tags[1]", "auth.access_rules[0].models[1]"]
        );
    }
}
