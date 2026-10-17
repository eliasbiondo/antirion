//! `GET /runtime/guardrails`: every defined guardrail (enabled or not) with its severity and its
//! figures over the last 24 hours, cluster-wide.

use antirion_identity::rbac::Scope;
use antirion_runtime::safety::stats::{GuardrailStats, HOURS};
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;
use crate::pagination;

const SCOPE: &str = "runtime/guardrails";

/// The action a definition takes: its own, else its type's default (none for transforms).
fn action_of(d: &Value, ty: &str) -> Option<String> {
    if let Some(a) = d.get("action").and_then(Value::as_str) {
        return Some(a.to_owned());
    }
    match ty {
        "system_prompt" | "decorator" | "template" | "json_repair" => None,
        "pii" | "secrets" | "llm_rewrite" => Some("redact".to_owned()),
        _ => Some("block".to_owned()),
    }
}

/// The severity of a definition: its own, else by action.
fn severity_of(d: &Value, action: Option<&str>) -> Option<String> {
    if let Some(s) = d.get("severity").and_then(Value::as_str) {
        return Some(s.to_owned());
    }
    Some(
        match action? {
            "block" => "high",
            "redact" => "medium",
            _ => "low",
        }
        .to_owned(),
    )
}

fn item(name: &str, d: &Value, s: Option<&GuardrailStats>) -> Value {
    let ty = d.get("type").and_then(Value::as_str).unwrap_or_default();
    let action = action_of(d, ty);
    let empty = GuardrailStats {
        hits_by_hour: vec![0; HOURS],
        ..GuardrailStats::default()
    };
    let s = s.unwrap_or(&empty);
    json!({
        "name": name,
        "type": ty,
        "action": action,
        "severity": severity_of(d, action.as_deref()),
        "enabled": d.get("enabled").and_then(Value::as_bool) != Some(false),
        "evaluations_24h": s.evaluations,
        "hits_24h": s.hits,
        "hits_by_hour": s.hits_by_hour,
        "blocks_24h": s.blocks,
        "redactions_24h": s.redactions,
        "flags_24h": s.flags,
        "errors_24h": s.errors,
        "p95_latency_ms": s.p95_latency_ms,
        "false_positive_rate": s.false_positive_rate,
    })
}

pub(crate) async fn list(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&["limit", "cursor"])?;
    call.require("policies:read", &[Scope::Gateway])?;
    let (limit, after) = pagination::page_params(call, SCOPE)?;
    let stats = call
        .rt
        .guardrail_stats()
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    let defs = call
        .snap
        .config
        .doc
        .pointer("/guardrails/definitions")
        .and_then(Value::as_object);
    let mut names: Vec<(&String, &Value)> = defs.into_iter().flatten().collect();
    names.sort_by(|a, b| a.0.cmp(b.0));
    let page: Vec<(&String, &Value)> = names
        .into_iter()
        .filter(|(n, _)| after.as_deref().is_none_or(|a| n.as_str() > a))
        .take(limit + 1)
        .collect();
    let more = page.len() > limit;
    let items: Vec<Value> = page
        .iter()
        .take(limit)
        .map(|(n, d)| item(n, d, stats.get(n.as_str())))
        .collect();
    let next_cursor = more
        .then(|| page.get(limit.saturating_sub(1)))
        .flatten()
        .map(|(n, _)| pagination::encode(&call.rt.cursor_key, SCOPE, n));
    Ok(call.json(
        StatusCode::OK,
        &json!({"items": items, "next_cursor": next_cursor}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_follow_the_type_and_the_action() {
        let d = json!({"type": "pii"});
        let v = item("p", &d, None);
        assert_eq!(
            (v["action"].as_str(), v["severity"].as_str()),
            (Some("redact"), Some("medium"))
        );
        assert_eq!(v["hits_by_hour"].as_array().map(Vec::len), Some(24));
        let t = item("t", &json!({"type": "system_prompt", "enabled": false}), None);
        assert!(t["action"].is_null() && t["severity"].is_null() && t["enabled"] == false);
        let c = item(
            "c",
            &json!({"type": "regex", "action": "flag", "severity": "critical"}),
            None,
        );
        assert_eq!(c["severity"], "critical");
    }
}
