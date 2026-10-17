//! The closed metric set of alert rules, computed from request-log records: one pass over the
//! records of a window and scope, so evaluation reads what the request path already wrote and adds
//! nothing to it.

use std::collections::BTreeMap;

use antirion_core::UtcMicros;
use serde_json::Value;

const MINUTE_US: i64 = 60_000_000;
const HOUR_US: i64 = 60 * MINUTE_US;
const WEEK_US: i64 = 7 * 24 * HOUR_US;

/// The record's start, in microseconds.
#[must_use]
pub fn started_us(r: &Value) -> Option<i64> {
    r.get("started_at")
        .and_then(Value::as_str)
        .and_then(|s| UtcMicros::parse_rfc3339(s).ok())
        .map(|t| t.0)
}

/// The dimensions of a scope that records carry: an instance's scope may also name what it is about
/// (`anomaly`, `budget`, `store`), which no record has.
const RECORD_DIMENSIONS: &[&str] = &[
    "organization",
    "team",
    "project",
    "key",
    "user",
    "end_user",
    "provider",
    "deployment",
    "model",
    "route",
    "environment",
];

/// `scope` without the dimensions records do not carry; `None` when nothing is left to match, so
/// that a scope about something else counts no request.
#[must_use]
pub fn record_scope(scope: &BTreeMap<String, String>) -> Option<BTreeMap<String, String>> {
    let kept: BTreeMap<String, String> = scope
        .iter()
        .filter(|(k, _)| RECORD_DIMENSIONS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    (!kept.is_empty() || scope.is_empty()).then_some(kept)
}

/// Whether a record falls in `scope` (every dimension must match the record's own field).
#[must_use]
pub fn in_scope(r: &Value, scope: &BTreeMap<String, String>) -> bool {
    scope
        .iter()
        .all(|(k, v)| r.get(k.as_str()).and_then(Value::as_str) == Some(v.as_str()))
}

fn status(r: &Value) -> u64 {
    r.get("status_code").and_then(Value::as_u64).unwrap_or(0)
}

fn cost(r: &Value) -> f64 {
    r.pointer("/cost/total")
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0)
}

/// The `p`-th percentile (nearest rank) of `values`, which it sorts.
#[must_use]
pub fn percentile(values: &mut [f64], p: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let rank = ((p / 100.0) * values.len() as f64).ceil() as usize;
    values.get(rank.saturating_sub(1)).copied()
}

#[allow(clippy::cast_precision_loss)]
fn ratio(n: usize, d: usize) -> Option<f64> {
    (d > 0).then(|| n as f64 / d as f64)
}

fn mean(values: &[f64]) -> Option<f64> {
    #[allow(clippy::cast_precision_loss)]
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

/// Requests, errors and throttled requests of a set of records.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Impact {
    pub requests: u64,
    pub errors: u64,
    pub throttled: u64,
}

#[must_use]
pub fn impact<'a>(records: impl Iterator<Item = &'a Value>) -> Impact {
    let mut i = Impact::default();
    for r in records {
        i.requests += 1;
        match status(r) {
            s if s >= 500 => i.errors += 1,
            429 => i.throttled += 1,
            _ => {}
        }
    }
    i
}

/// Per-minute request and token limits that apply to a scope, for the utilization metrics.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Limits {
    pub rpm: Option<f64>,
    pub tpm: Option<f64>,
}

/// What a metric needs besides the window's records.
#[derive(Debug)]
pub struct Inputs<'a> {
    /// Records of the window, in scope.
    pub window: &'a [&'a Value],
    pub window_us: i64,
    /// Records of the baseline hour (the same hour of the week, a week earlier), in scope.
    pub baseline: &'a [&'a Value],
    pub limits: Limits,
    /// `(spent in the period so far, amount, elapsed of the period, remaining of the period)` of each
    /// budget in scope, in USD and microseconds.
    pub budgets: &'a [(f64, f64, i64, i64)],
}

fn latencies(records: &[&Value], pointer: &str) -> Vec<f64> {
    records
        .iter()
        .filter_map(|r| r.pointer(pointer).and_then(Value::as_f64))
        .collect()
}

/// The value of `metric`, or `None` when the window holds nothing to compute it from.
#[must_use]
pub fn compute(metric: &str, x: &Inputs<'_>) -> Option<f64> {
    let w = x.window;
    #[allow(clippy::cast_precision_loss)]
    let minutes = (x.window_us as f64 / MINUTE_US as f64).max(1.0 / 60.0);
    match metric {
        "requests" =>
        {
            #[allow(clippy::cast_precision_loss)]
            Some(w.len() as f64)
        }
        "p95_latency_ms" => percentile(&mut latencies(w, "/timing/total_ms"), 95.0),
        "p99_latency_ms" => percentile(&mut latencies(w, "/timing/total_ms"), 99.0),
        "ttft_p95_ms" => percentile(&mut latencies(w, "/timing/ttft_ms"), 95.0),
        "error_rate" => ratio(w.iter().filter(|r| status(r) >= 500).count(), w.len()),
        "upstream_5xx_rate" => {
            let attempts: Vec<&Value> = w
                .iter()
                .filter_map(|r| r.get("attempts").and_then(Value::as_array))
                .flatten()
                .collect();
            let failed = attempts
                .iter()
                .filter(|a| {
                    a.get("status_code")
                        .and_then(Value::as_u64)
                        .is_some_and(|s| s >= 500)
                })
                .count();
            ratio(failed, attempts.len())
        }
        "fallback_rate" => {
            let fell = w
                .iter()
                .filter(|r| {
                    r.get("attempts").and_then(Value::as_array).is_some_and(|a| {
                        a.iter()
                            .any(|a| a.get("reason").and_then(Value::as_str) == Some("fallback"))
                    })
                })
                .count();
            ratio(fell, w.len())
        }
        "cost_per_request_usd" =>
        {
            #[allow(clippy::cast_precision_loss)]
            (!w.is_empty()).then(|| w.iter().map(|r| cost(r)).sum::<f64>() / w.len() as f64)
        }
        "spend_usd" => Some(w.iter().map(|r| cost(r)).sum()),
        "guardrail_events" => {
            let n = w
                .iter()
                .filter_map(|r| r.get("guardrails").and_then(Value::as_array))
                .flatten()
                .filter(|g| {
                    !matches!(
                        g.get("verdict").and_then(Value::as_str),
                        Some("pass" | "skipped") | None
                    )
                })
                .count();
            #[allow(clippy::cast_precision_loss)]
            Some(n as f64)
        }
        "cache_hit_rate" => {
            let looked: Vec<&str> = w
                .iter()
                .filter_map(|r| r.pointer("/cache/status").and_then(Value::as_str))
                .filter(|s| matches!(*s, "hit" | "miss" | "refresh"))
                .collect();
            ratio(looked.iter().filter(|s| **s == "hit").count(), looked.len())
        }
        "rpm_utilization" => {
            #[allow(clippy::cast_precision_loss)]
            let rate = w.len() as f64 / minutes;
            x.limits.rpm.filter(|l| *l > 0.0).map(|l| rate / l)
        }
        "tpm_utilization" => {
            let tokens: f64 = w
                .iter()
                .filter_map(|r| r.pointer("/usage/total").and_then(Value::as_f64))
                .sum();
            x.limits.tpm.filter(|l| *l > 0.0).map(|l| tokens / minutes / l)
        }
        "budget_projection_ratio" => x
            .budgets
            .iter()
            .filter(|(_, amount, ..)| *amount > 0.0)
            .map(|(spent, amount, elapsed, remaining)| {
                let trailing = (*elapsed).clamp(1, WEEK_US);
                #[allow(clippy::cast_precision_loss)]
                let rate = spent / trailing as f64;
                #[allow(clippy::cast_precision_loss)]
                let projected = spent + rate * (*remaining).max(0) as f64;
                projected / amount
            })
            .reduce(f64::max),
        "latency_baseline_ratio" => {
            let now = mean(&latencies(w, "/timing/total_ms"))?;
            let base = mean(&latencies(x.baseline, "/timing/total_ms"))?;
            (base > 0.0).then(|| now / base)
        }
        "cost_baseline_ratio" => {
            let per = |rs: &[&Value]| mean(&rs.iter().map(|r| cost(r)).collect::<Vec<_>>());
            let now = per(w)?;
            let base = per(x.baseline)?;
            (base > 0.0).then(|| now / base)
        }
        _ => None,
    }
}

/// The baseline window of an evaluation at `now`: the same hour of the week, one week earlier.
#[must_use]
pub fn baseline_hour(now_us: i64) -> (i64, i64) {
    let start = now_us - now_us.rem_euclid(HOUR_US) - WEEK_US;
    (start, start + HOUR_US)
}

/// Per-minute rate of a declared limit (`limit` per `window`), when it counts requests or tokens.
#[must_use]
pub fn per_minute(limit: &Value) -> Option<(String, f64)> {
    let unit = limit.get("unit")?.as_str()?;
    let amount = limit.get("limit")?.as_f64()?;
    let window = limit
        .get("per")
        .and_then(Value::as_str)
        .and_then(antirion_config::scalar::parse_duration)?;
    let minutes = window.as_secs_f64() / 60.0;
    let kind = match unit {
        "requests" => "rpm",
        "tokens" => "tpm",
        _ => return None,
    };
    (minutes > 0.0).then(|| (kind.to_owned(), amount / minutes))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn impact_matches_what_records_carry() {
        let scope: BTreeMap<String, String> = [("key", "k"), ("anomaly", "a1")]
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .into();
        assert_eq!(
            record_scope(&scope),
            Some([("key".to_owned(), "k".to_owned())].into())
        );
        let about: BTreeMap<String, String> = [("store".to_owned(), "vault".to_owned())].into();
        assert_eq!(record_scope(&about), None);
        assert_eq!(record_scope(&BTreeMap::new()), Some(BTreeMap::new()));
    }

    fn rec(ms: f64, status: u64, cost: &str) -> Value {
        json!({"started_at": "2026-10-01T00:00:00Z", "status_code": status, "provider": "p",
               "timing": {"total_ms": ms, "ttft_ms": null}, "cost": {"total": cost},
               "attempts": [{"reason": "primary", "status_code": status}],
               "cache": {"status": "miss"}, "guardrails": [], "usage": {"total": 10}})
    }

    fn inputs<'a>(w: &'a [&'a Value]) -> Inputs<'a> {
        Inputs {
            window: w,
            window_us: 5 * MINUTE_US,
            baseline: &[],
            limits: Limits {
                rpm: Some(1.0),
                tpm: None,
            },
            budgets: &[],
        }
    }

    #[test]
    fn latency_errors_and_spend() {
        let rs: Vec<Value> = (1..=20)
            .map(|i| rec(f64::from(i) * 100.0, if i == 20 { 502 } else { 200 }, "0.01"))
            .collect();
        let w: Vec<&Value> = rs.iter().collect();
        let x = inputs(&w);
        assert_eq!(compute("p95_latency_ms", &x), Some(1900.0));
        assert_eq!(compute("p99_latency_ms", &x), Some(2000.0));
        assert_eq!(compute("error_rate", &x), Some(0.05));
        assert_eq!(compute("upstream_5xx_rate", &x), Some(0.05));
        assert_eq!(compute("requests", &x), Some(20.0));
        assert!((compute("spend_usd", &x).unwrap() - 0.2).abs() < 1e-9);
        assert_eq!(compute("rpm_utilization", &x), Some(4.0));
        assert_eq!(compute("tpm_utilization", &x), None, "no token limit applies");
        assert_eq!(compute("cache_hit_rate", &x), Some(0.0));
        assert_eq!(compute("ttft_p95_ms", &x), None, "no stream in the window");
        assert_eq!(compute("latency_baseline_ratio", &x), None, "no baseline");
        assert_eq!(compute("unknown", &x), None);
        let i = impact(rs.iter());
        assert_eq!((i.requests, i.errors, i.throttled), (20, 1, 0));
    }

    #[test]
    fn projections_and_baselines() {
        let w: Vec<&Value> = Vec::new();
        let budgets = [(50.0, 100.0, 10 * 86_400_000_000, 20 * 86_400_000_000)];
        let x = Inputs {
            budgets: &budgets,
            ..inputs(&w)
        };
        let r = compute("budget_projection_ratio", &x).unwrap();
        assert!((r - (50.0 + 50.0 / 7.0 * 20.0) / 100.0).abs() < 1e-9, "{r}");
        let a = rec(200.0, 200, "0.02");
        let b = rec(100.0, 200, "0.01");
        let (now, base) = ([&a], [&b]);
        let x = Inputs {
            baseline: &base,
            ..inputs(&now)
        };
        assert_eq!(compute("latency_baseline_ratio", &x), Some(2.0));
        assert_eq!(compute("cost_baseline_ratio", &x), Some(2.0));
        let (s, e) = baseline_hour(WEEK_US + 90 * MINUTE_US);
        assert_eq!((s, e), (HOUR_US, 2 * HOUR_US));
    }

    #[test]
    fn scopes_and_limits() {
        let r = rec(1.0, 200, "0");
        let mut scope = BTreeMap::new();
        assert!(in_scope(&r, &scope));
        scope.insert("provider".into(), "p".into());
        assert!(in_scope(&r, &scope));
        scope.insert("model".into(), "m".into());
        assert!(!in_scope(&r, &scope));
        assert_eq!(
            per_minute(&json!({"unit": "requests", "limit": 120, "per": "2m"})),
            Some(("rpm".into(), 60.0))
        );
        assert_eq!(
            per_minute(&json!({"unit": "tokens", "limit": 1000, "per": "1m"})),
            Some(("tpm".into(), 1000.0))
        );
        assert_eq!(per_minute(&json!({"unit": "concurrency", "limit": 5})), None);
    }
}
