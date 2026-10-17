//! Rules of the observability policy the schema cannot express: zero data retention only
//! accumulates down the policy chain, and no feature that persists content applies to traffic under
//! it.

use serde_json::Value;

use crate::semantic::policy_levels;
use crate::{ConfigError, DetailCode};

const ZDR: &str = "/observability/zero_data_retention";
const CACHE_ENABLED: &str = "/cache/enabled";

/// Trace-ingestion and OTLP sinks share their name space with the HTTP sinks: the name labels
/// every kind in metrics and in the degraded reports.
fn sink_names(doc: &Value, errors: &mut Vec<ConfigError>) {
    let sinks = doc.pointer("/observability/request_logs/sinks");
    let names = |kind: &str| {
        sinks
            .and_then(|s| s.get(kind))
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default()
    };
    let mut taken: Vec<(String, &str)> = names("http").into_iter().map(|n| (n, "http")).collect();
    for kind in ["langfuse", "otlp"] {
        for name in names(kind) {
            if let Some((_, other)) = taken.iter().find(|(n, _)| *n == name) {
                errors.push(ConfigError::new(
                    &crate::DocPath::root()
                        .key("observability")
                        .key("request_logs")
                        .key("sinks")
                        .key(kind)
                        .key(&name),
                    DetailCode::Conflict,
                    format!("a {other} sink has this name already"),
                ));
            } else {
                taken.push((name, kind));
            }
        }
    }
}

pub(crate) fn check(doc: &Value, errors: &mut Vec<ConfigError>) {
    sink_names(doc, errors);
    let levels = policy_levels(doc);
    for level in &levels {
        let broader = || level.ancestors.iter().filter_map(|p| p.get(doc));
        let inherited = broader().any(|b| b.pointer(ZDR) == Some(&Value::Bool(true)));
        if inherited && level.block.pointer(ZDR) == Some(&Value::Bool(false)) {
            errors.push(ConfigError::new(
                &level.path.key("observability").key("zero_data_retention"),
                DetailCode::LockedByBroaderLevel,
                "zero data retention applies at a broader level and cannot be turned off here",
            ));
        }
        let zdr = inherited || level.block.pointer(ZDR) == Some(&Value::Bool(true));
        if !zdr {
            continue;
        }
        let enabling = std::iter::once((level.path.clone(), level.block))
            .chain(
                level
                    .ancestors
                    .iter()
                    .rev()
                    .filter_map(|p| Some((p.clone(), p.get(doc)?))),
            )
            .find(|(_, b)| b.pointer(CACHE_ENABLED).is_some());
        if let Some((path, block)) = enabling
            && block.pointer(CACHE_ENABLED) == Some(&Value::Bool(true))
        {
            let at = path.key("cache").key("enabled");
            if !errors.iter().any(|e| e.path == at.to_string()) {
                errors.push(ConfigError::new(
                    &at,
                    DetailCode::Conflict,
                    "the response cache stores content, and zero data retention applies to traffic under it",
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
    fn trace_sinks_and_http_sinks_do_not_share_names() {
        let doc = json!({"observability": {"request_logs": {"sinks": {
            "http": {"w": {"url": "https://a"}}, "langfuse": {"w": {}, "t": {}}}}}});
        assert_eq!(paths(&doc), ["observability.request_logs.sinks.langfuse.w"]);
        let doc = json!({"observability": {"request_logs": {"sinks": {
            "http": {"w": {"url": "https://a"}}, "langfuse": {"t": {}}, "otlp": {"w": {}, "t": {}, "o": {}}}}}});
        assert_eq!(
            paths(&doc),
            [
                "observability.request_logs.sinks.otlp.w",
                "observability.request_logs.sinks.otlp.t"
            ]
        );
    }

    #[test]
    fn zdr_accumulates_and_refuses_the_cache() {
        let doc = json!({
            "teams": {"t": {"organization": "default", "policies": {"observability": {"zero_data_retention": true}}}},
            "projects": {"p": {"team": "t", "policies": {
                "observability": {"zero_data_retention": false}, "cache": {"enabled": true}}}},
            "keys": {"k": {"project": "p", "policies": {"observability": {"zero_data_retention": false}}}},
        });
        let got = paths(&doc);
        assert!(
            got.contains(&"projects.p.policies.observability.zero_data_retention".to_owned()),
            "{got:?}"
        );
        assert!(
            got.contains(&"keys.k.policies.observability.zero_data_retention".to_owned()),
            "{got:?}"
        );
        assert!(
            got.contains(&"projects.p.policies.cache.enabled".to_owned()),
            "{got:?}"
        );
        assert!(paths(&json!({"policies": {"observability": {"zero_data_retention": false}}})).is_empty());
        let gateway = json!({"policies": {"cache": {"enabled": true}},
            "projects": {"p": {"policies": {"observability": {"zero_data_retention": true}}}}});
        assert_eq!(paths(&gateway), vec!["policies.cache.enabled".to_owned()]);
    }
}
