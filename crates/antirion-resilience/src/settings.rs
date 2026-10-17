//! The gateway-wide health settings (`resilience.health`) and their per-deployment overrides.

use std::collections::HashMap;
use std::time::Duration;

use antirion_config::scalar::parse_duration;
use serde_json::{Map, Value};

use crate::budget::BudgetSettings;

/// Circuit-breaker settings.
#[derive(Clone, Debug, PartialEq)]
pub struct BreakerSettings {
    pub enabled: bool,
    /// `None` disables the consecutive-failure trigger.
    pub consecutive_failures: Option<u32>,
    pub failure_rate: f64,
    pub min_requests: u32,
    pub window: Duration,
    pub buckets: u32,
    pub open_duration: Duration,
    pub max_open_duration: Duration,
    pub half_open_probes: u32,
    pub success_threshold: u32,
    /// `probe_scope: cluster`: `half_open_probes` is held across the cluster through leases.
    pub cluster_probes: bool,
}

/// What an active health check sends.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CheckRequest {
    /// `GET <base>/models`.
    ModelsList,
    /// A generation of one output token.
    MinimalGeneration,
}

/// Active health checks of a deployment (`active_checks`).
#[derive(Clone, Debug, PartialEq)]
pub struct CheckSettings {
    pub enabled: bool,
    pub interval: Duration,
    pub timeout: Duration,
    /// Only `OPEN` deployments are checked.
    pub open_only: bool,
}

/// The health settings that apply to one deployment.
#[derive(Clone, Debug, PartialEq)]
pub struct HealthSettings {
    pub breaker: BreakerSettings,
    pub max_cooldown: Duration,
    pub rate_limit_cooldown: Duration,
    pub cooldown_buffer: Duration,
    pub dead_key_recheck: Duration,
    pub max_dead_key_recheck: Duration,
    /// `all_unhealthy: ignore_health`.
    pub ignore_when_all_unhealthy: bool,
    pub checks: CheckSettings,
}

/// The gateway's health settings and every deployment override.
#[derive(Clone, Debug, PartialEq)]
pub struct HealthConfig {
    gateway: HealthSettings,
    overrides: HashMap<String, HealthSettings>,
    /// `active_checks.request`, gateway-wide.
    pub check_request: CheckRequest,
    /// `resilience.retry_budget`.
    pub budget: BudgetSettings,
    /// `resilience.classification.status_overrides`: the class an upstream status is taken as.
    pub status_overrides: HashMap<u16, crate::Class>,
}

fn dur(v: &Value, key: &str, default: Duration) -> Duration {
    v.get(key)
        .and_then(Value::as_str)
        .and_then(parse_duration)
        .unwrap_or(default)
}

fn int(v: &Value, key: &str, default: u32) -> u32 {
    v.get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(default)
}

impl HealthSettings {
    fn from_value(v: &Value) -> HealthSettings {
        let b = v.get("breaker").unwrap_or(&Value::Null);
        let c = v.get("active_checks").unwrap_or(&Value::Null);
        let consecutive_failures = match b.get("consecutive_failures") {
            Some(Value::String(s)) if s == "none" => None,
            Some(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()).or(Some(5)),
            None => Some(5),
        };
        HealthSettings {
            breaker: BreakerSettings {
                enabled: b.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                consecutive_failures,
                failure_rate: b.get("failure_rate").and_then(Value::as_f64).unwrap_or(0.5),
                min_requests: int(b, "min_requests", 20),
                window: dur(b, "window", Duration::from_secs(60)),
                buckets: int(b, "buckets", 10).max(1),
                open_duration: dur(b, "open_duration", Duration::from_secs(30)),
                max_open_duration: dur(b, "max_open_duration", Duration::from_secs(300)),
                half_open_probes: int(b, "half_open_probes", 1).max(1),
                success_threshold: int(b, "success_threshold", 1).max(1),
                cluster_probes: b.get("probe_scope").and_then(Value::as_str) == Some("cluster"),
            },
            max_cooldown: dur(v, "max_cooldown", Duration::from_secs(300)),
            rate_limit_cooldown: dur(v, "rate_limit_cooldown", Duration::from_secs(5)),
            cooldown_buffer: dur(v, "cooldown_buffer", Duration::ZERO),
            dead_key_recheck: dur(v, "dead_key_recheck", Duration::from_secs(300)),
            max_dead_key_recheck: dur(v, "max_dead_key_recheck", Duration::from_secs(3600)),
            ignore_when_all_unhealthy: v.get("all_unhealthy").and_then(Value::as_str)
                == Some("ignore_health"),
            checks: CheckSettings {
                enabled: c.get("enabled").and_then(Value::as_bool).unwrap_or(false),
                interval: dur(c, "interval", Duration::from_secs(10)),
                timeout: dur(c, "timeout", Duration::from_secs(5)),
                open_only: c.get("when").and_then(Value::as_str) != Some("always"),
            },
        }
    }
}

fn merged(base: &Value, over: &Value) -> Value {
    match (base, over) {
        (Value::Object(a), Value::Object(b)) => {
            let mut out: Map<String, Value> = a.clone();
            for (k, v) in b {
                let next = match out.get(k) {
                    Some(prev) => merged(prev, v),
                    None => v.clone(),
                };
                out.insert(k.clone(), next);
            }
            Value::Object(out)
        }
        (_, v) => v.clone(),
    }
}

impl HealthConfig {
    /// Reads `resilience.health` and `resilience.health_overrides` from a resolved document.
    #[must_use]
    pub fn from_doc(doc: &Value) -> HealthConfig {
        let health = doc.pointer("/resilience/health").cloned().unwrap_or(Value::Null);
        let overrides = doc
            .pointer("/resilience/health_overrides")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .map(|(name, o)| (name.clone(), HealthSettings::from_value(&merged(&health, o))))
            .collect();
        let check_request = match health.pointer("/active_checks/request").and_then(Value::as_str) {
            Some("minimal_generation") => CheckRequest::MinimalGeneration,
            _ => CheckRequest::ModelsList,
        };
        HealthConfig {
            gateway: HealthSettings::from_value(&health),
            overrides,
            check_request,
            budget: BudgetSettings::from_doc(doc),
            status_overrides: doc
                .pointer("/resilience/classification/status_overrides")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .filter_map(|(k, v)| Some((k.parse().ok()?, crate::Class::parse(v.as_str()?)?)))
                .collect(),
        }
    }

    /// The class of an upstream answer's status: its override, else the status table's.
    #[must_use]
    pub fn classify_status(&self, status: u16, generation: bool) -> crate::Class {
        self.status_overrides
            .get(&status)
            .copied()
            .unwrap_or_else(|| crate::classify::status(status, generation))
    }

    /// Whether any deployment ignores health when every candidate is unhealthy.
    #[must_use]
    pub fn ignores_any(&self) -> bool {
        self.gateway.ignore_when_all_unhealthy || self.overrides.values().any(|o| o.ignore_when_all_unhealthy)
    }

    /// Whether any deployment may be actively checked.
    #[must_use]
    pub fn checks_enabled(&self) -> bool {
        self.gateway.checks.enabled || self.overrides.values().any(|o| o.checks.enabled)
    }

    /// The settings that apply to a deployment.
    #[must_use]
    pub fn for_deployment(&self, deployment: &str) -> &HealthSettings {
        self.overrides.get(deployment).unwrap_or(&self.gateway)
    }
}

impl Default for HealthConfig {
    fn default() -> HealthConfig {
        HealthConfig::from_doc(&Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn overrides_merge_over_the_gateway() {
        let c = HealthConfig::from_doc(&json!({"resilience": {
            "health": {"breaker": {"consecutive_failures": 3, "open_duration": "1s"}},
            "health_overrides": {"d": {"breaker": {"enabled": false}}}
        }}));
        assert_eq!(c.for_deployment("x").breaker.consecutive_failures, Some(3));
        let d = c.for_deployment("d");
        assert!(!d.breaker.enabled);
        assert_eq!(d.breaker.open_duration, Duration::from_secs(1));
        let none = HealthConfig::from_doc(
            &json!({"resilience": {"health": {"breaker": {"consecutive_failures": "none"}}}}),
        );
        assert_eq!(none.for_deployment("x").breaker.consecutive_failures, None);
        let checks = HealthConfig::from_doc(&json!({"resilience": {
            "health": {"all_unhealthy": "ignore_health",
                       "active_checks": {"interval": "200ms", "request": "minimal_generation"}},
            "health_overrides": {"d": {"active_checks": {"enabled": true, "when": "always"}}}
        }}));
        assert!(checks.checks_enabled());
        assert!(!checks.for_deployment("x").checks.enabled);
        let d = &checks.for_deployment("d").checks;
        assert!(d.enabled && !d.open_only);
        assert_eq!(d.interval, Duration::from_millis(200));
        assert_eq!(checks.check_request, CheckRequest::MinimalGeneration);
        assert!(checks.for_deployment("x").ignore_when_all_unhealthy);
    }
}
