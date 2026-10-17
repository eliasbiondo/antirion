//! Rules of the resilience configuration the schema cannot express: relations between fields of
//! one block (`tier_retries` against `max_attempts`, backoff bounds, the deadline against the
//! connect timer, breaker windows and durations) and the status overrides that would retry caller
//! errors.

use std::time::Duration;

use serde_json::Value;

use crate::scalar::parse_duration;
use crate::semantic::policy_blocks;
use crate::{ConfigError, DetailCode, DocPath};

fn dur(v: &Value, key: &str) -> Option<Duration> {
    v.get(key).and_then(Value::as_str).and_then(parse_duration)
}

fn out_of_range(errors: &mut Vec<ConfigError>, at: &DocPath, message: &str) {
    errors.push(ConfigError::new(at, DetailCode::OutOfRange, message));
}

fn policy(block: &Value, at: &DocPath, errors: &mut Vec<ConfigError>) {
    if let Some(retry) = block.get("retry") {
        let rat = at.key("retry");
        if let (Some(t), Some(m)) = (
            retry.get("tier_retries").and_then(Value::as_u64),
            retry.get("max_attempts").and_then(Value::as_u64),
        ) && t + 1 > m
        {
            out_of_range(
                errors,
                &rat.key("tier_retries"),
                "tier_retries must be at most max_attempts - 1",
            );
        }
        for (name, _) in retry
            .get("classes")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            if FIXED_CLASSES.contains(&name.as_str()) {
                errors.push(ConfigError::new(
                    &rat.key("classes").key(name),
                    DetailCode::Conflict,
                    "this class accepts no override",
                ));
            } else if !OVERRIDABLE_CLASSES.contains(&name.as_str()) {
                errors.push(ConfigError::new(
                    &rat.key("classes").key(name),
                    DetailCode::InvalidEnum,
                    "not a failure class",
                ));
            }
        }
        if let Some(b) = retry.get("backoff") {
            let initial = dur(b, "initial").unwrap_or(Duration::from_millis(500));
            if let Some(max) = dur(b, "max")
                && max < initial
            {
                out_of_range(
                    errors,
                    &rat.key("backoff").key("max"),
                    "backoff.max must be at least backoff.initial",
                );
            }
        }
    }
    if let Some(t) = block.get("timeouts") {
        let connect = dur(t, "connect").unwrap_or(Duration::from_secs(10));
        if let Some(deadline) = dur(t, "deadline")
            && deadline < connect
        {
            out_of_range(
                errors,
                &at.key("timeouts").key("deadline"),
                "the deadline must be at least the connect timeout",
            );
        }
    }
}

fn health(h: &Value, at: &DocPath, errors: &mut Vec<ConfigError>) {
    if let Some(b) = h.get("breaker") {
        let bat = at.key("breaker");
        let window = dur(b, "window").unwrap_or(Duration::from_secs(60));
        let buckets = b.get("buckets").and_then(Value::as_u64).unwrap_or(10).max(1);
        if window.as_micros() / u128::from(buckets) < 10_000 {
            out_of_range(errors, &bat, "window / buckets must be at least 10ms");
        }
        let open = dur(b, "open_duration").unwrap_or(Duration::from_secs(30));
        if let Some(max) = dur(b, "max_open_duration")
            && max < open
        {
            out_of_range(
                errors,
                &bat.key("max_open_duration"),
                "max_open_duration must be at least open_duration",
            );
        }
    }
    let recheck = dur(h, "dead_key_recheck").unwrap_or(Duration::from_secs(300));
    if let Some(max) = dur(h, "max_dead_key_recheck")
        && max < recheck
    {
        out_of_range(
            errors,
            &at.key("max_dead_key_recheck"),
            "max_dead_key_recheck must be at least dead_key_recheck",
        );
    }
}

/// Classes a status may map to when it is a caller's error by nature.
const CALLER_CLASSES: [&str; 3] = ["caller_error", "context_length", "content_policy"];

/// Classes `retry.classes` may not override.
const FIXED_CLASSES: [&str; 5] = [
    "caller_error",
    "guardrail_blocked",
    "client_cancelled",
    "context_length",
    "content_policy",
];

/// Classes `retry.classes` may override.
const OVERRIDABLE_CLASSES: [&str; 17] = [
    "dns_error",
    "connect_error",
    "connect_timeout",
    "tls_error",
    "refused_stream",
    "upstream_reset",
    "timeout",
    "rate_limited",
    "quota_exhausted",
    "credential_rejected",
    "overloaded",
    "server_error",
    "conflict",
    "not_implemented",
    "model_unavailable",
    "malformed_response",
    "reasoning_replay_rejected",
];

pub(crate) fn check(doc: &Value, errors: &mut Vec<ConfigError>) {
    for (at, block) in policy_blocks(doc) {
        if let Some(r) = block.get("resilience") {
            policy(r, &at.key("resilience"), errors);
        }
    }
    let root = DocPath::root().key("resilience");
    let Some(res) = doc.get("resilience") else { return };
    if let Some(h) = res.get("health") {
        health(h, &root.key("health"), errors);
    }
    for (name, h) in res
        .get("health_overrides")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        health(h, &root.key("health_overrides").key(name), errors);
    }
    for (status, class) in res
        .pointer("/classification/status_overrides")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let at = root.key("classification").key("status_overrides").key(status);
        let code: u16 = status.parse().unwrap_or(0);
        let class = class.as_str().unwrap_or_default();
        let forbidden = (200..=299).contains(&code)
            || (matches!(code, 400 | 404 | 413 | 422) && !CALLER_CLASSES.contains(&class));
        if forbidden {
            errors.push(ConfigError::new(
                &at,
                DetailCode::Conflict,
                "this status cannot be mapped to a retried class",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn paths(doc: &Value) -> Vec<String> {
        let mut e = Vec::new();
        check(doc, &mut e);
        e.into_iter().map(|e| e.path).collect()
    }

    #[test]
    fn relations_between_fields() {
        assert_eq!(
            paths(&json!({"policies": {"resilience": {"retry": {"max_attempts": 3, "tier_retries": 3}}}})),
            vec!["policies.resilience.retry.tier_retries"]
        );
        assert_eq!(
            paths(&json!({"policies": {"resilience": {"timeouts": {"connect": "10s", "deadline": "5s"}}}})),
            vec!["policies.resilience.timeouts.deadline"]
        );
        assert_eq!(
            paths(&json!({"resilience": {"health": {"breaker": {"window": "100ms", "buckets": 20}}}})),
            vec!["resilience.health.breaker"]
        );
        assert_eq!(
            paths(
                &json!({"resilience": {"classification": {"status_overrides": {"400": "server_error", "490": "server_error"}}}})
            ),
            vec!["resilience.classification.status_overrides.400"]
        );
    }
}
