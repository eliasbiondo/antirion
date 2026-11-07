//! Availability and service-level objectives from the request log, and the personal digests of
//! alerts, spend and safety events.
//!
//! A request counts against availability only when the gateway or the upstream caused its failure
//! (a 5xx); caller errors, limit and budget refusals and guardrail blocks are 4xx and count as
//! requests but not as failures.

use std::collections::BTreeMap;

use antirion_core::UtcMicros;
use serde_json::{Value, json};

use super::config::Slo;
use super::metrics::{in_scope, started_us};
use crate::Runtime;
use crate::operators::ConsoleLocale;

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

/// Calls `visit` with each request-log record started between `from` and `to` (half-open), oldest
/// first, reading the store a page at a time, so only one page is held at once (at most
/// `MAX_RECORDS` records); returns how many it visited.
pub async fn for_each_record(
    rt: &Runtime,
    from: i64,
    to: i64,
    mut visit: impl FnMut(&Value) + Send,
) -> usize {
    use antirion_store::logs::{LogFilter, LogOrder, SortKey};
    let mut seen = 0;
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
            content: false,
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
        for l in &page {
            if let Ok(r) = serde_json::from_str::<Value>(&l.log.record) {
                visit(&r);
                seen += 1;
            }
        }
        if !full || seen >= MAX_RECORDS {
            break;
        }
    }
    seen
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
    for_each_record(rt, from, to, |r| {
        let Some(t) = started_us(r) else { return };
        let bucket = interval_us.map_or(from, |i| from + (t - from).div_euclid(i.max(1)) * i.max(1));
        let group = (group_by != "gateway")
            .then(|| r.get(group_by).and_then(Value::as_str).map(str::to_owned))
            .flatten();
        rows.entry((bucket, group)).or_default().add(r);
    })
    .await;
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

/// The records of `[now − span, now]` in an objective's scope, and how many of them were bad.
#[derive(Clone, Copy, Debug, Default)]
struct Tally {
    total: u64,
    bad: u64,
}

impl Tally {
    fn add(&mut self, slo: &Slo, r: &Value, now: i64, span: i64) {
        if started_us(r).is_some_and(|t| t >= now - span) && in_scope(r, &slo.scope) {
            self.total += 1;
            if !good(slo, r) {
                self.bad += 1;
            }
        }
    }

    /// The bad fraction, `None` without records.
    fn ratio(self) -> Option<f64> {
        #[allow(clippy::cast_precision_loss)]
        (self.total > 0).then(|| self.bad as f64 / self.total as f64)
    }
}

/// The spans each objective is read over: its window, then the burn-rate spans.
fn spans(slo: &Slo) -> [i64; 3] {
    [slo.window_us, HOUR_US, 6 * HOUR_US]
}

/// `GET /runtime/slos`: each objective's attainment, remaining error budget and burn rates.
#[must_use]
pub async fn status(rt: &Runtime) -> Vec<Value> {
    let now = rt.now().0;
    let cfg = rt.alerting();
    let longest = cfg.slos.iter().map(|s| s.window_us).max().unwrap_or(0);
    let mut tallies = vec![[Tally::default(); 3]; cfg.slos.len()];
    if !cfg.slos.is_empty() {
        for_each_record(rt, now - longest, now + 1, |r| {
            for (slo, t) in cfg.slos.iter().zip(tallies.iter_mut()) {
                for (span, t) in spans(slo).into_iter().zip(t.iter_mut()) {
                    t.add(slo, r, now, span);
                }
            }
        })
        .await;
    }
    let mut out: Vec<Value> = cfg
        .slos
        .iter()
        .zip(&tallies)
        .map(|(slo, [window, hour, six])| {
            let allowed = 1.0 - slo.objective;
            let bad = window.ratio();
            let burn = |t: &Tally| t.ratio().and_then(|b| (allowed > 0.0).then(|| b / allowed));
            json!({
                "name": slo.name,
                "kind": slo.kind,
                "objective": slo.objective,
                "attained": bad.map(|b| 1.0 - b),
                "error_budget_remaining": bad.and_then(|b| (allowed > 0.0).then(|| 1.0 - b / allowed)),
                "burn_rate_1h": burn(hour),
                "burn_rate_6h": burn(six),
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
        let locale = ConsoleLocale::from_preferences(&prefs);
        let body = digest_text(rt, u, since, now, locale).await;
        let subject = if period == DAY_US {
            locale.pick("Antirion daily digest", "خلاصهٔ روزانهٔ Antirion")
        } else {
            locale.pick("Antirion weekly digest", "خلاصهٔ هفتگی Antirion")
        };
        if let Some(to) = &u.email {
            rt.send_mail(to, subject, body);
        }
    }
}

async fn digest_text(
    rt: &Runtime,
    u: &crate::identity::UserRt,
    since: i64,
    now: i64,
    locale: ConsoleLocale,
) -> String {
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
                locale.severity(&s.instance.severity),
                s.instance.summary,
                s.instance.status,
                s.instance.fired_at
            ));
        }
    }
    let mut spend: f64 = std::iter::empty::<f64>().sum();
    let mut safety = 0usize;
    for_each_record(rt, since, now + 1, |r| {
        if let Some(cost) = r
            .pointer("/cost/total")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<f64>().ok())
        {
            spend += cost;
        }
        safety += r
            .get("guardrails")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|g| {
                !matches!(
                    g.get("verdict").and_then(Value::as_str),
                    Some("pass" | "skipped") | None
                )
            })
            .count();
    })
    .await;
    let mut text = format!(
        "{}: {} ({})\n",
        locale.pick("Alerts fired", "هشدارهای فعال‌شده"),
        instances.len(),
        by_severity
            .iter()
            .map(|(k, v)| format!("{}: {v}", locale.severity(k)))
            .collect::<Vec<_>>()
            .join(", ")
    );
    for l in lines {
        text.push_str(&l);
        text.push('\n');
    }
    text.push_str(&format!(
        "\n{}: {}\n{}: {safety}\n",
        locale.pick("Spend in the period", "هزینه در این دوره"),
        crate::wording::usd(spend),
        locale.pick("Safety events", "رویدادهای ایمنی"),
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
        let fast = json!({"started_at": "2026-10-01T00:00:00Z", "timing": {"total_ms": 50.0}});
        let slow = json!({"started_at": "2026-10-01T00:00:00Z", "timing": {"total_ms": 500.0}});
        let old = json!({"started_at": "2026-09-30T00:00:00Z", "timing": {"total_ms": 500.0}});
        let now = UtcMicros::parse_rfc3339("2026-10-01T00:00:01Z").unwrap().0;
        let mut t = Tally::default();
        for r in [&fast, &slow, &old] {
            t.add(&slo, r, now, HOUR_US);
        }
        assert_eq!(Tally::default().ratio(), None);
        let r = t.ratio().unwrap();
        assert!((r - 0.5).abs() < 1e-12);
    }
}
