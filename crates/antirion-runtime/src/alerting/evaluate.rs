//! The scheduled evaluation of metric rules. It wakes often but evaluates only when the gateway's
//! clock has passed the next due time, so the test clock drives it like the wall clock does; in a
//! cluster only the holder of the `alert_evaluator` lease evaluates, so a rule fires once.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use antirion_limits::applicable::named_budget;
use antirion_limits::decl::BudgetUnit;
use antirion_limits::level::Level;
use antirion_limits::policy::DeclKind;
use serde_json::Value;
use tokio::time::Instant;

use super::config::{AlertingConfig, Rule, Trigger};
use super::engine;
use super::instance::{Impact, Stored};
use super::metrics::{self, Inputs, Limits};
use crate::Runtime;

const TICK: Duration = Duration::from_millis(250);
const LEASE: &str = "alert_evaluator";
const LEASE_TTL: Duration = Duration::from_secs(15);
const RENEW_EVERY: Duration = Duration::from_secs(5);
/// How often old instances, events and deliveries are removed.
const PURGE_EVERY_US: i64 = 3_600_000_000;
/// Delivery attempts and kept events are listed for seven days.
pub(crate) const DELIVERY_RETENTION_US: i64 = 7 * 86_400_000_000;

/// Whether this node evaluates: always standalone, under the lease in a cluster.
async fn leader(rt: &Runtime, renewed: &mut Option<Instant>) -> bool {
    let Some(pg) = rt.store.postgres() else {
        return true;
    };
    if rt.cluster.is_none() {
        return true;
    }
    if renewed.is_some_and(|t| t.elapsed() < RENEW_EVERY) {
        return true;
    }
    match pg.lease(LEASE, &rt.node.id, LEASE_TTL).await {
        Ok(true) => {
            *renewed = Some(Instant::now());
            true
        }
        _ => {
            *renewed = None;
            false
        }
    }
}

pub(crate) async fn run(rt: Arc<Runtime>) {
    let mut last: Option<i64> = None;
    let mut renewed = None;
    let mut purged: Option<i64> = None;
    loop {
        tokio::select! {
            () = rt.shutdown.cancelled() => return,
            () = tokio::time::sleep(TICK) => {}
        }
        let now = rt.now().0;
        let cfg = rt.alerting();
        let due = last.is_none_or(|l| now < l || now - l >= cfg.evaluation_interval_us);
        if !due {
            continue;
        }
        if !leader(&rt, &mut renewed).await {
            last = None;
            continue;
        }
        last = Some(now);
        if let Err(e) = evaluate(&rt, &cfg, now).await {
            tracing::warn!(error = %e, "alert rules could not be evaluated");
        }
        if purged.is_none_or(|p| now < p || now - p >= PURGE_EVERY_US) {
            purged = Some(now);
            let before = now.saturating_sub(cfg.instance_retention_us);
            if let Err(e) = rt
                .store
                .purge_alerting(before, now.saturating_sub(DELIVERY_RETENTION_US))
                .await
            {
                tracing::debug!(error = %e, "old alerting records could not be removed");
            }
        }
        super::slo::digests(&rt, now).await;
    }
}

/// Request and token limits per minute declared for the levels a scope names (the gateway's own
/// when it names none); the tightest of each.
fn limits(doc: &Value, scope: &BTreeMap<String, String>) -> Limits {
    let mut blocks = vec![doc.pointer("/policies/limits")];
    for (dim, collection) in [
        ("organization", "organizations"),
        ("team", "teams"),
        ("project", "projects"),
        ("key", "keys"),
        ("route", "routes"),
        ("model", "models"),
        ("deployment", "deployments"),
    ] {
        if let Some(name) = scope.get(dim) {
            blocks.push(
                doc.get(collection)
                    .and_then(|c| c.get(name))
                    .and_then(|r| r.pointer("/policies/limits")),
            );
        }
    }
    let mut out = Limits::default();
    for (kind, per_minute) in blocks
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .flat_map(|m| m.values())
        .filter_map(metrics::per_minute)
    {
        let slot = if kind == "rpm" { &mut out.rpm } else { &mut out.tpm };
        *slot = Some(slot.map_or(per_minute, |s: f64| s.min(per_minute)));
    }
    out
}

/// `(spent, amount, elapsed, remaining)` of the unpartitioned USD budgets a scope names.
async fn budgets(rt: &Runtime, scope: &BTreeMap<String, String>, now: i64) -> Vec<(f64, f64, i64, i64)> {
    let snap = rt.gens.load();
    let mut out = Vec::new();
    for d in snap.limits.all() {
        let DeclKind::Budget(b) = &d.kind else { continue };
        if !matches!(b.unit, BudgetUnit::Usd) || !b.partition.attrs.is_empty() {
            continue;
        }
        let dim = match d.level {
            Level::Organization => "organization",
            Level::Team => "team",
            Level::Project => "project",
            Level::Key => "key",
            Level::Gateway => "",
            _ => continue,
        };
        if !dim.is_empty() && scope.get(dim) != Some(&d.owner) {
            continue;
        }
        let project = match d.level {
            Level::Project => Some(d.owner.clone()),
            Level::Key => snap
                .config
                .doc
                .pointer(&format!("/keys/{}/project", d.owner))
                .and_then(Value::as_str)
                .map(str::to_owned),
            _ => None,
        };
        let Some(named) = named_budget(&snap.limits, d.level, &d.owner, &d.name, &[], project.as_deref())
        else {
            continue;
        };
        let Ok(state) = rt
            .accounts
            .budget_state(&named, antirion_core::UtcMicros(now))
            .await
        else {
            continue;
        };
        #[allow(clippy::cast_precision_loss)]
        out.push((
            state.view.spent as f64,
            named.quota as f64,
            now - state.period.start.0,
            state.period.end.map_or(0, |e| e.0 - now),
        ));
    }
    out
}

/// One evaluation of every enabled metric rule at `now`, and the resolution of instances whose
/// condition has cleared.
pub(crate) async fn evaluate(
    rt: &Runtime,
    cfg: &AlertingConfig,
    now: i64,
) -> Result<(), antirion_store::StoreError> {
    let open: HashMap<String, Stored> = rt
        .store
        .open_instances()
        .await?
        .iter()
        .filter_map(|r| serde_json::from_str::<Stored>(&r.body).ok())
        .map(|s| (s.instance.fingerprint.clone(), s))
        .collect();
    let metric_rules: Vec<&Rule> = cfg
        .rules
        .iter()
        .filter(|r| r.enabled && matches!(r.trigger, Trigger::Metric { .. }))
        .collect();
    let earliest = metric_rules
        .iter()
        .map(|r| now - r.window_us)
        .chain(std::iter::once(metrics::baseline_hour(now).0))
        .min()
        .unwrap_or(now);
    let records = if metric_rules.is_empty() {
        Vec::new()
    } else {
        super::slo::records_between(rt, earliest, now + 1).await
    };
    let doc = rt.gens.load().config.doc.clone();
    let mut evaluated = std::collections::HashSet::new();
    for rule in metric_rules {
        let Trigger::Metric {
            metric,
            operator,
            threshold,
        } = &rule.trigger
        else {
            continue;
        };
        let in_window = |r: &&Arc<Value>| {
            metrics::started_us(r).is_some_and(|t| t >= now - rule.window_us && t <= now)
                && metrics::in_scope(r, &rule.scope)
        };
        let window: Vec<&Value> = records.iter().filter(in_window).map(|r| r.as_ref()).collect();
        let (b0, b1) = metrics::baseline_hour(now);
        let baseline: Vec<&Value> = if metric.ends_with("_baseline_ratio") {
            records
                .iter()
                .filter(|r| {
                    metrics::started_us(r).is_some_and(|t| t >= b0 && t < b1)
                        && metrics::in_scope(r, &rule.scope)
                })
                .map(|r| r.as_ref())
                .collect()
        } else {
            Vec::new()
        };
        let budget_states = if metric == "budget_projection_ratio" {
            budgets(rt, &rule.scope, now).await
        } else {
            Vec::new()
        };
        let value = metrics::compute(
            metric,
            &Inputs {
                window: &window,
                window_us: rule.window_us,
                baseline: &baseline,
                limits: limits(&doc, &rule.scope),
                budgets: &budget_states,
            },
        );
        let fp = super::instance::fingerprint(&rule.name, &rule.scope);
        evaluated.insert(fp.clone());
        let holds = value.is_some_and(|v| operator.holds(v, *threshold));
        if holds {
            let since = *rt.state.alerting.pending.lock().entry(fp.clone()).or_insert(now);
            if now - since >= rule.for_us {
                let i = metrics::impact(window.iter().copied());
                engine::raise(
                    rt,
                    rule,
                    rule.scope.clone(),
                    value,
                    super::summaries::metric(&doc, rule, value.unwrap_or_default()),
                    Impact {
                        requests: i.requests,
                        errors: i.errors,
                        throttled: i.throttled,
                    },
                )
                .await?;
            }
        } else {
            rt.state.alerting.pending.lock().remove(&fp);
            if let Some(o) = open.get(&fp) {
                engine::clear(rt, rule, o).await?;
            }
        }
    }
    for (fp, o) in &open {
        if evaluated.contains(fp)
            || o.instance.rule == crate::people::anomalies::ALERT_RULE
            || o.instance.rule == crate::budgets::FORECAST_RULE
        {
            continue;
        }
        match cfg.rule(&o.instance.rule).filter(|r| r.enabled) {
            Some(rule) if matches!(rule.trigger, Trigger::Event { .. }) => {
                if now - o.state.last_fired_us >= rule.resolve_after_us {
                    engine::clear(rt, rule, o).await?;
                }
            }
            Some(_) => {}
            None if o.instance.category == "test" => {}
            None => {
                let gone = Rule {
                    resolve_after_us: 0,
                    ..placeholder(&o.instance.rule)
                };
                engine::clear(rt, &gone, o).await?;
            }
        }
    }
    Ok(())
}

/// A rule that no longer exists, for resolving its instances.
fn placeholder(name: &str) -> Rule {
    Rule {
        name: name.to_owned(),
        enabled: false,
        trigger: Trigger::Event { events: Vec::new() },
        category: String::new(),
        severity: super::config::Severity::Info,
        scope: BTreeMap::new(),
        window_us: 0,
        for_us: 0,
        cooldown_us: 0,
        resolve_after_us: 0,
        channels: Vec::new(),
        escalation: Vec::new(),
        owner_team: None,
        runbook_url: None,
        description: None,
        silence_during_maintenance: false,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_tightest_declared_limit_applies() {
        let doc = json!({
            "policies": {"limits": {"g": {"unit": "requests", "limit": 600, "per": "1m"}}},
            "keys": {"k": {"policies": {"limits": {
                "rpm": {"unit": "requests", "limit": 60, "per": "1m"},
                "tpm": {"unit": "tokens", "limit": 1000, "per": "1m"}}}}}
        });
        let l = limits(&doc, &BTreeMap::from([("key".to_owned(), "k".to_owned())]));
        assert_eq!((l.rpm, l.tpm), (Some(60.0), Some(1000.0)));
        let g = limits(&doc, &BTreeMap::new());
        assert_eq!((g.rpm, g.tpm), (Some(600.0), None));
    }
}
