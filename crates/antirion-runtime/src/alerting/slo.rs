//! Availability and service-level objectives from the request log, and the personal digests of
//! alerts, spend and safety events.
//!
//! A request counts against availability only when the gateway or the upstream caused its failure
//! (a 5xx); caller errors, limit and budget refusals and guardrail blocks are 4xx and count as
//! requests but not as failures.

use std::collections::BTreeMap;
use std::sync::Arc;

use antirion_core::UtcMicros;
use serde_json::{Value, json};

use super::config::Slo;
use super::metrics::{in_scope, started_us};
use crate::Runtime;

const HOUR_US: i64 = 3_600_000_000;
const DAY_US: i64 = 24 * HOUR_US;

/// How a request ended, for availability.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Good,
    GatewayFailure,
    UpstreamFailure,
}

/// Classifies one record.
#[must_use]
pub fn outcome(r: &Value) -> Outcome {
    let status = r.get("status_code").and_then(Value::as_u64).unwrap_or(0);
    if status < 500 {
        return Outcome::Good;
    }
    let code = r.get("code").and_then(Value::as_str).unwrap_or("");
    let upstream = code.starts_with("upstream")
        || code == "no_healthy_deployment"
        || r.get("stage").and_then(Value::as_str) == Some("attempt");
    if upstream {
        Outcome::UpstreamFailure
    } else {
        Outcome::GatewayFailure
    }
}

/// Request counts of one group or bucket.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub requests: u64,
    pub gateway_failures: u64,
    pub upstream_failures: u64,
}

impl Counts {
    pub fn add(&mut self, r: &Value) {
        self.requests += 1;
        match outcome(r) {
            Outcome::Good => {}
            Outcome::GatewayFailure => self.gateway_failures += 1,
            Outcome::UpstreamFailure => self.upstream_failures += 1,
        }
    }

    /// `1 − failures / requests`, `None` without requests.
    #[must_use]
    pub fn availability(&self) -> Option<f64> {
        #[allow(clippy::cast_precision_loss)]
        (self.requests > 0)
            .then(|| 1.0 - (self.gateway_failures + self.upstream_failures) as f64 / self.requests as f64)
    }
}

/// Records read per store call.
const SCAN: usize = 2_000;
/// The most records one evaluation or report reads.
const MAX_RECORDS: usize = 200_000;

/// The request-log records started between `from` and `to` (half-open), oldest first, read from the
/// store (at most `MAX_RECORDS`).
pub async fn records_between(rt: &Runtime, from: i64, to: i64) -> Vec<Arc<Value>> {
    use antirion_store::logs::{LogFilter, LogOrder, SortKey};
    let mut out = Vec::new();
    let mut after = None;
    loop {
        let filter = LogFilter {
            from_us: Some(from),
            to_us: Some(to),
            ..LogFilter::default()
        };
        let order = LogOrder {
            sort: SortKey::StartedAt,
            desc: false,
            after,
            limit: SCAN,
        };
        let page = match rt.list_logs(filter, order).await {
            Ok(p) => p,
            Err(e) => {
                tracing::debug!(error = %e, "request-log records could not be read for alerting");
                break;
            }
        };
        let full = page.len() == SCAN;
        after = page.last().map(|l| l.pos.clone());
        out.extend(
            page.iter()
                .filter_map(|l| serde_json::from_str::<Value>(&l.log.record).ok())
                .map(Arc::new),
        );
        if !full || out.len() >= MAX_RECORDS {
            break;
        }
    }
    out
}

/// The availability report: one row per group and bucket (`interval_us`, or the whole range).
#[must_use]
pub async fn availability(
    rt: &Runtime,
    from: i64,
    to: i64,
    group_by: &str,
    interval_us: Option<i64>,
) -> Vec<Value> {
    let mut rows: BTreeMap<(i64, Option<String>), Counts> = BTreeMap::new();
    for r in records_between(rt, from, to).await {
        let Some(t) = started_us(&r) else { continue };
        let bucket = interval_us.map_or(from, |i| from + (t - from).div_euclid(i.max(1)) * i.max(1));
        let group = (group_by != "gateway")
            .then(|| r.get(group_by).and_then(Value::as_str).map(str::to_owned))
            .flatten();
        rows.entry((bucket, group)).or_default().add(&r);
    }
    let uptime = rt.state.alerting.uptime_ratio(from, to);
    rows.into_iter()
        .map(|((start, group), c)| {
            let mut g = serde_json::Map::new();
            if group_by != "gateway" {
                g.insert(group_by.to_owned(), group.map_or(Value::Null, Value::String));
            }
            json!({
                "group": g,
                "start": UtcMicros(start).to_rfc3339(),
                "requests": c.requests,
                "gateway_failures": c.gateway_failures,
                "upstream_failures": c.upstream_failures,
                "availability": c.availability(),
                "uptime_ratio": if group_by == "gateway" { uptime } else { None },
            })
        })
        .collect()
}

fn good(slo: &Slo, r: &Value) -> bool {
    match slo.latency_threshold_ms {
        Some(limit) if slo.kind == "latency" => r
            .pointer("/timing/total_ms")
            .and_then(Value::as_f64)
            .is_some_and(|ms| ms <= limit),
        _ => outcome(r) == Outcome::Good,
    }
}

/// The bad fraction of the records in `[now − span, now]`, `None` without records.
fn bad_ratio(slo: &Slo, records: &[Arc<Value>], now: i64, span: i64) -> Option<f64> {
    let (mut total, mut bad) = (0u64, 0u64);
    for r in records {
        if started_us(r).is_some_and(|t| t >= now - span) && in_scope(r, &slo.scope) {
            total += 1;
            if !good(slo, r) {
                bad += 1;
            }
        }
    }
    #[allow(clippy::cast_precision_loss)]
    (total > 0).then(|| bad as f64 / total as f64)
}

/// `GET /runtime/slos`: each objective's attainment, remaining error budget and burn rates.
#[must_use]
pub async fn status(rt: &Runtime) -> Vec<Value> {
    let now = rt.now().0;
    let cfg = rt.alerting();
    let longest = cfg.slos.iter().map(|s| s.window_us).max().unwrap_or(0);
    let all = records_between(rt, now - longest, now + 1).await;
    let mut out: Vec<Value> = cfg
        .slos
        .iter()
        .map(|slo| {
            let allowed = 1.0 - slo.objective;
            let bad = bad_ratio(slo, &all, now, slo.window_us);
            let burn =
                |span| bad_ratio(slo, &all, now, span).and_then(|b| (allowed > 0.0).then(|| b / allowed));
            json!({
                "name": slo.name,
                "kind": slo.kind,
                "objective": slo.objective,
                "attained": bad.map(|b| 1.0 - b),
                "error_budget_remaining": bad.and_then(|b| (allowed > 0.0).then(|| 1.0 - b / allowed)),
                "burn_rate_1h": burn(HOUR_US),
                "burn_rate_6h": burn(6 * HOUR_US),
                "window": slo.window,
            })
        })
        .collect();
    out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    out
}

/// Sends the daily and weekly digests that are due at `now`. A user's first digest period starts
/// when the evaluator first sees the preference.
pub(crate) async fn digests(rt: &Runtime, now: i64) {
    let snap = rt.gens.load();
    let mut users: Vec<(&String, &crate::identity::UserRt)> = snap
        .identity
        .users
        .iter()
        .filter(|(_, u)| u.enabled && u.human && u.email.is_some())
        .collect();
    users.sort_by(|a, b| a.0.cmp(b.0));
    for (name, u) in users {
        let Ok(prefs) = rt.preferences(name).await else {
            continue;
        };
        let period = match prefs.pointer("/notifications/digest").and_then(Value::as_str) {
            Some("daily") => DAY_US,
            Some("weekly") => 7 * DAY_US,
            _ => continue,
        };
        let Ok(last) = rt.store.record("notification_digest", name).await else {
            continue;
        };
        let last = last.and_then(|r| r.body.parse::<i64>().ok());
        let due = last.is_some_and(|l| now - l >= period);
        if last.is_none() || due {
            let rec = antirion_store::IdentityRecord {
                kind: "notification_digest".to_owned(),
                id: name.clone(),
                user: name.clone(),
                expires_us: None,
                body: now.to_string(),
            };
            if rt.store.put_record(rec).await.is_err() {
                continue;
            }
        }
        if !due {
            continue;
        }
        let since = now - period;
        let body = digest_text(rt, u, since, now).await;
        let subject = if period == DAY_US {
            "Antirion daily digest"
        } else {
            "Antirion weekly digest"
        };
        if let Some(to) = &u.email {
            rt.send_mail(to, subject, body);
        }
    }
}

async fn digest_text(rt: &Runtime, u: &crate::identity::UserRt, since: i64, now: i64) -> String {
    let principal = antirion_identity::rbac::AdminPrincipal {
        kind: antirion_core::audit::ActorKind::Operator,
        id: String::new(),
        user: None,
        bindings: u.bindings.clone(),
        read_only: false,
    };
    let snap = rt.gens.load();
    let instances = super::engine::all(rt, Some(since), None)
        .await
        .unwrap_or_default();
    let mut by_severity: BTreeMap<String, usize> = BTreeMap::new();
    let mut lines = Vec::new();
    for s in instances.iter().filter(|s| {
        principal.allows(
            "alerts:read",
            &super::engine::chain(&snap.identity, &s.instance.scope),
        )
    }) {
        *by_severity.entry(s.instance.severity.clone()).or_default() += 1;
        if lines.len() < 20 {
            lines.push(format!(
                "- [{}] {} ({}, {})",
                s.instance.severity, s.instance.summary, s.instance.status, s.instance.fired_at
            ));
        }
    }
    let records = records_between(rt, since, now + 1).await;
    let spend: f64 = records
        .iter()
        .filter_map(|r| r.pointer("/cost/total").and_then(Value::as_str))
        .filter_map(|s| s.parse::<f64>().ok())
        .sum();
    let safety = records
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
    let mut text = format!(
        "Alerts fired: {} ({})\n",
        instances.len(),
        by_severity
            .iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    for l in lines {
        text.push_str(&l);
        text.push('\n');
    }
    text.push_str(&format!(
        "\nSpend in the period: {}\nSafety events: {safety}\n",
        crate::wording::usd(spend)
    ));
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caller_errors_do_not_count_against_availability() {
        let mut c = Counts::default();
        for _ in 0..987 {
            c.add(&json!({"status_code": 200}));
        }
        for _ in 0..10 {
            c.add(&json!({"status_code": 400, "code": "invalid_request"}));
        }
        for _ in 0..3 {
            c.add(&json!({"status_code": 502, "code": "upstream_unavailable", "stage": "attempt"}));
        }
        assert_eq!(c.requests, 1000);
        assert_eq!((c.upstream_failures, c.gateway_failures), (3, 0));
        assert!((c.availability().unwrap() - 0.997).abs() < 1e-12);
        assert_eq!(
            outcome(&json!({"status_code": 500, "code": "internal"})),
            Outcome::GatewayFailure
        );
        assert_eq!(Counts::default().availability(), None);
    }

    #[test]
    fn latency_objectives_count_slow_requests_as_bad() {
        let slo = Slo {
            name: "l".into(),
            kind: "latency".into(),
            objective: 0.9,
            latency_threshold_ms: Some(100.0),
            window: "1d".into(),
            window_us: DAY_US,
            scope: BTreeMap::new(),
        };
        let fast = Arc::new(json!({"started_at": "2026-10-01T00:00:00Z", "timing": {"total_ms": 50.0}}));
        let slow = Arc::new(json!({"started_at": "2026-10-01T00:00:00Z", "timing": {"total_ms": 500.0}}));
        let now = UtcMicros::parse_rfc3339("2026-10-01T00:00:01Z").unwrap().0;
        let r = bad_ratio(&slo, &[fast, slow], now, HOUR_US).unwrap();
        assert!((r - 0.5).abs() < 1e-12);
    }
}
