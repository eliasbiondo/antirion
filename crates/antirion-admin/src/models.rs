//! The Model catalog's reads: list filters by tier, label and lifecycle state with the counts the
//! catalog header shows, each Model's effective lifecycle, and the tiers with their Model counts.

use std::collections::BTreeMap;

use antirion_core::UtcMicros;
use antirion_identity::rbac::Scope;
use antirion_router::{BUILTIN_TIERS, Lifecycle};
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;
use crate::pagination;

const STATES: [&str; 5] = ["preview", "ga", "restricted", "deprecated", "retired"];

/// What `GET /models` selects beyond pagination.
#[derive(Debug, Default)]
pub(crate) struct ListFilter {
    tiers: Vec<String>,
    labels: Vec<String>,
    states: Vec<String>,
    now: UtcMicros,
}

impl ListFilter {
    pub(crate) fn from_call(call: &Call) -> Result<ListFilter, AdminError> {
        call.allow_params(&["limit", "cursor", "tier", "label", "state"])?;
        let all = |name: &str| -> Vec<String> {
            call.query
                .iter()
                .filter(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
                .collect()
        };
        let states = all("state");
        if let Some(bad) = states.iter().find(|s| !STATES.contains(&s.as_str())) {
            return Err(AdminError::invalid_parameter(format!(
                "state {bad:?} is not one of {}",
                STATES.join(", ")
            )));
        }
        Ok(ListFilter {
            tiers: all("tier"),
            labels: all("label"),
            states,
            now: call.rt.now(),
        })
    }

    /// The Model passes every filter given: one of the tiers, every label, one of the states.
    pub(crate) fn keeps(&self, spec: &Value) -> bool {
        let tier = spec.get("tier").and_then(Value::as_str);
        if !self.tiers.is_empty() && !tier.is_some_and(|t| self.tiers.iter().any(|x| x == t)) {
            return false;
        }
        let labels: Vec<&str> = spec
            .get("labels")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if !self.labels.iter().all(|l| labels.contains(&l.as_str())) {
            return false;
        }
        self.states.is_empty() || self.states.iter().any(|s| s == state_of(spec, self.now))
    }

    /// Counts over every Model the filters keep: per lifecycle state, enabled and disabled.
    pub(crate) fn counts<'a>(&self, kept: impl Iterator<Item = &'a Value>) -> Value {
        let mut states: BTreeMap<&str, u64> = STATES.iter().map(|s| (*s, 0)).collect();
        let (mut enabled, mut disabled) = (0u64, 0u64);
        for spec in kept {
            *states.entry(state_of(spec, self.now)).or_default() += 1;
            if spec.get("enabled") == Some(&Value::Bool(false)) {
                disabled += 1;
            } else {
                enabled += 1;
            }
        }
        json!({"lifecycle": states, "enabled": enabled, "disabled": disabled})
    }
}

/// The lifecycle state a reader sees: the configured state, or `retired` past the end of life.
fn state_of(spec: &Value, now: UtcMicros) -> &'static str {
    lifecycle_of(spec).shown(now)
}

fn lifecycle_of(spec: &Value) -> Lifecycle {
    let state = spec
        .pointer("/lifecycle/state")
        .and_then(Value::as_str)
        .and_then(antirion_router::LifecycleState::parse)
        .unwrap_or_default();
    Lifecycle {
        state,
        eol_at: spec
            .pointer("/lifecycle/eol_at")
            .and_then(Value::as_str)
            .and_then(|t| UtcMicros::parse_rfc3339(t).ok()),
        replacement: None,
        since: None,
    }
}

/// Adds `metadata.lifecycle` to a Model's representation: the state a reader sees and when the
/// Model entered its configured state.
pub(crate) fn decorate(call: &Call, name: &str, item: &mut Value) {
    let now = call.rt.now();
    let since = call
        .snap
        .routing
        .models
        .get(name)
        .and_then(|m| m.lifecycle.since)
        .map(UtcMicros::to_rfc3339_seconds);
    let state = item.get("spec").map_or("ga", |s| state_of(s, now));
    if let Some(meta) = item.get_mut("metadata").and_then(Value::as_object_mut) {
        meta.insert("lifecycle".into(), json!({"state": state, "since": since}));
    }
}

/// `GET /routing/tiers`: built-in tiers, then custom ones in name order, each with how many
/// Models carry it.
pub(crate) fn tiers(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&["limit", "cursor"])?;
    call.require("models:read", &[Scope::Gateway])?;
    let (limit, after) = pagination::page_params(call, "routing/tiers")?;
    let doc = &call.snap.config.doc;
    let mut counts: BTreeMap<&str, u64> = BTreeMap::new();
    for m in doc
        .get("models")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|m| m.values())
    {
        if let Some(t) = m.get("tier").and_then(Value::as_str) {
            *counts.entry(t).or_default() += 1;
        }
    }
    let custom = doc.pointer("/routing/tiers").and_then(Value::as_object);
    let mut all: Vec<Value> = BUILTIN_TIERS
        .iter()
        .map(|t| json!({"name": t, "builtin": true, "models": counts.get(t).copied().unwrap_or(0)}))
        .collect();
    for (name, spec) in custom.into_iter().flatten() {
        if BUILTIN_TIERS.contains(&name.as_str()) {
            continue;
        }
        let mut t = json!({"name": name, "builtin": false, "models": counts.get(name.as_str()).copied().unwrap_or(0)});
        if let (Some(d), Some(o)) = (spec.get("description"), t.as_object_mut()) {
            o.insert("description".into(), d.clone());
        }
        all.push(t);
    }
    let position = |v: &Value| {
        v.get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let start = after
        .as_ref()
        .and_then(|a| all.iter().position(|t| &position(t) == a).map(|i| i + 1))
        .unwrap_or(0);
    let page: Vec<Value> = all.iter().skip(start).take(limit).cloned().collect();
    let next = (start + page.len() < all.len())
        .then(|| page.last().map(position))
        .flatten()
        .map(|p| pagination::encode(&call.rt.cursor_key, "routing/tiers", &p));
    Ok(call.json(StatusCode::OK, &json!({"items": page, "next_cursor": next})))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(tiers: &[&str], labels: &[&str], states: &[&str]) -> ListFilter {
        ListFilter {
            tiers: tiers.iter().map(|s| (*s).to_owned()).collect(),
            labels: labels.iter().map(|s| (*s).to_owned()).collect(),
            states: states.iter().map(|s| (*s).to_owned()).collect(),
            now: UtcMicros::parse_rfc3339("2026-06-01T00:00:00Z").unwrap(),
        }
    }

    #[test]
    fn filters_and_counts() {
        let fast = json!({"tier": "fast", "labels": ["eu", "tools"]});
        let retired = json!({"tier": "fast", "lifecycle": {"state": "deprecated", "eol_at": "2026-01-01T00:00:00Z"}, "enabled": false});
        let plain = json!({});
        assert!(filter(&["fast"], &[], &[]).keeps(&fast));
        assert!(!filter(&["fast"], &[], &[]).keeps(&plain));
        assert!(filter(&[], &["eu", "tools"], &[]).keeps(&fast));
        assert!(!filter(&[], &["eu", "vision"], &[]).keeps(&fast));
        assert!(filter(&[], &[], &["retired"]).keeps(&retired));
        assert!(!filter(&[], &[], &["deprecated"]).keeps(&retired));
        let c = filter(&[], &[], &[]).counts([&fast, &retired, &plain].into_iter());
        assert_eq!(c["lifecycle"]["ga"], 2);
        assert_eq!(c["lifecycle"]["retired"], 1);
        assert_eq!(
            (c["enabled"].as_u64(), c["disabled"].as_u64()),
            (Some(2), Some(1))
        );
    }
}
