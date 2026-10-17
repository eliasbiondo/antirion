//! Configuration blocks this build does not implement: rejected at validation with detail code
//! `not_supported_in_this_build` at their path, so no configuration is accepted and then ignored.
//! A block counts only when it asks for behaviour: its defaults (`enabled: false`, empty maps and
//! lists, `null`) are accepted.

use serde_json::Value;

use crate::semantic::policy_blocks;
use crate::{ConfigError, DetailCode, DocPath};

fn gate(errors: &mut Vec<ConfigError>, at: &DocPath, what: &str) {
    errors.push(ConfigError::new(
        at,
        DetailCode::NotSupportedInThisBuild,
        format!("{what} is not supported in this build"),
    ));
}

/// Whether a value asks for something: not null, `false`, an empty map or an empty list.
fn set(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null | Value::Bool(false)) => false,
        Some(Value::Object(m)) => !m.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(_) => true,
    }
}

fn is(v: &Value, pointer: &str, expected: &str) -> bool {
    v.pointer(pointer).and_then(Value::as_str) == Some(expected)
}

fn entries<'a>(v: &'a Value, pointer: &str) -> impl Iterator<Item = (&'a String, &'a Value)> {
    v.pointer(pointer)
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
}

fn node(n: &Value, at: &DocPath, errors: &mut Vec<ConfigError>) {
    let Value::Object(m) = n else {
        return;
    };
    if let Some(p) = m.get("policies") {
        policy(p, &at.key("policies"), errors);
    }
    for list in ["children", "rules", "classes"] {
        for (i, item) in m
            .get(list)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let item_at = at.key(list).index(i);
            if list == "children" {
                node(item, &item_at, errors);
            } else if let Some(t) = item.get("target") {
                node(t, &item_at.key("target"), errors);
            }
        }
    }
    for single in ["default", "strong", "weak"] {
        if let Some(child) = m.get(single) {
            node(child, &at.key(single), errors);
        }
    }
    if let Some(t) = m.get("mirror").and_then(|x| x.get("target")) {
        node(t, &at.key("mirror").key("target"), errors);
    }
}

/// The unsupported members of one `policies` block.
fn policy(p: &Value, at: &DocPath, errors: &mut Vec<ConfigError>) {
    let cache = at.key("cache");
    if is(p, "/cache/hit_usage", "zero") {
        gate(errors, &cache.key("hit_usage"), "hit_usage zero");
    }
    for (i, e) in p
        .pointer("/cache/endpoints")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        if let Some(name @ ("gemini_generate" | "bedrock_converse" | "bedrock_invoke")) = e.as_str() {
            gate(
                errors,
                &cache.key("endpoints").index(i),
                &format!("caching {name}"),
            );
        }
    }
    if set(p.pointer("/a2a/agent_runs")) {
        gate(errors, &at.key("a2a").key("agent_runs"), "agent-run caps");
    }
}

pub(crate) fn check(doc: &Value, errors: &mut Vec<ConfigError>) {
    let root = DocPath::root();
    for (at, p) in policy_blocks(doc) {
        policy(p, &at, errors);
    }
    for (name, r) in entries(doc, "/routes") {
        let at = root.key("routes").key(name);
        if let Some(t) = r.get("tree") {
            node(t, &at.key("tree"), errors);
        }
    }
    for (name, m) in entries(doc, "/models") {
        if let Some(t) = m.get("target") {
            node(t, &root.key("models").key(name).key("target"), errors);
        }
    }
    if let Some(t @ ("redis" | "pgvector")) =
        doc.pointer("/cache/semantic_store/type").and_then(Value::as_str)
    {
        gate(
            errors,
            &root.key("cache").key("semantic_store").key("type"),
            &format!("the {t} semantic store"),
        );
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn paths(doc: &Value) -> Vec<String> {
        let mut errors = Vec::new();
        check(doc, &mut errors);
        assert!(
            errors
                .iter()
                .all(|e| e.code == DetailCode::NotSupportedInThisBuild)
        );
        errors.into_iter().map(|e| e.path).collect()
    }

    #[test]
    fn the_consoles_fields_and_defaults_pass() {
        let doc = json!({
            "smtp": {"enabled": true, "host": "127.0.0.1", "from": "gw@acme.example", "tls": "none"},
            "governance": {"change_control": {"resources": [], "approvals_required": 1},
                           "maintenance_windows": [{"start": "2026-01-01T00:00:00Z", "end": "2026-01-01T01:00:00Z"}],
                           "audit_sinks": {"siem": {"url": "https://siem.acme.example/ingest"}}},
            "observability": {"alerts": {"rules": {"r": {"category": "latency"}}}, "slos": {"s": {"objective": 0.99}}},
            "auth": {"anomaly_detection": {"enabled": true}, "admin": {"max_active_users": null}},
            "organizations": {"acme": {"logo": "https://a/l.png", "timezone": "UTC", "local_login": false,
                                       "regions": ["us-east-1"], "key_policy": {"default_lifetime": "90d"}}},
            "teams": {"t": {"display_name": "T", "color": "#2f6fde"}},
            "models": {"m": {"tier": "fast", "lifecycle": {"state": "ga"}}},
            "cache": {"invalidation_rules": {"r": {"trigger": {"event": "manual"}, "action": {"drop": "older_than", "older_than": "1d"}}}},
            "policies": {"budgets": {"b": {"mode": "approval", "owner": "alice", "downgrade": {"at": 0.9, "to_tier": "fast"}}},
                         "cache": {"read_only": true}}
        });
        assert!(paths(&doc).is_empty(), "{:?}", paths(&doc));
    }

    #[test]
    fn unsupported_blocks_are_named_by_path() {
        let doc = json!({
            "routes": {"r": {"tree": {"type": "fallback", "children": [
                {"type": "cost_quality", "strong": "a", "weak": "b", "threshold": 0.5,
                 "scorer": {"kind": "matrix_factorization"}},
                {"type": "weighted", "algorithm": "thompson", "experiment": "e", "children": [{"id": "a", "deployment": "a"}]},
                {"type": "pool", "children": ["a"], "scheduler": {"lora": true}},
                {"deployment": "a", "mirror": {"target": {"type": "pool", "children": ["a"], "picker": "external"}, "fraction": 1}}
            ]}, "tag_filter": {"enabled": true}}},
            "policies": {"cache": {"hit_usage": "zero", "semantic": {"enabled": true}},
                         "resilience": {"hedging": {"enabled": false}}},
            "cache": {"semantic_store": {"type": "pgvector"}}
        });
        let got = paths(&doc);
        for want in ["policies.cache.hit_usage", "cache.semantic_store.type"] {
            assert!(got.iter().any(|p| p == want), "{want} not in {got:?}");
        }
    }

    #[test]
    fn defaults_and_supported_values_pass() {
        let doc = json!({
            "routes": {"r": {"tree": {"type": "classifier", "kind": "embedding", "classes": [], "default": "a",
                                       "mirror": {"target": {"type": "pool", "children": ["b"]}, "fraction": 0.5}},
                             "tag_filter": {"enabled": true}}},
            "policies": {"cache": {"hit_usage": "original", "fail_mode": "open", "max_messages": null,
                                   "semantic": {"enabled": false}, "endpoints": ["openai.chat_completions"]},
                         "resilience": {"retry": {"class_fallbacks": {"context_length": "big"}, "classes": {}}}},
            "resilience": {"classification": {"status_overrides": {}},
                           "retry_budget": {"ratio": 0.5},
                           "health": {"all_unhealthy": "ignore_health", "active_checks": {"enabled": true}}},
            "observability": {"metrics": {"custom_labels": {}, "tenant_labels": []},
                              "request_logs": {"sinks": {"otlp": {}, "langfuse": {}}}},
            "mcp": {"servers": {"s": {"transport": "streamable_http", "auth": {"type": "none"}}},
                    "gateways": {"g": {"servers": ["s"], "modern_bridge": false}}}
        });
        assert!(paths(&doc).is_empty(), "{:?}", paths(&doc));
    }
}
