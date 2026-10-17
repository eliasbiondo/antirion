//! The schedule of this module. Every node writes its queued events and request counts every
//! quarter second, and every second reloads the running grants and the spend of budgets that
//! downgrade (so the request path reads them from memory). Forecasts and the expiry of overage
//! requests run every second on one node: always in standalone mode, under the `budget_watch`
//! lease in a cluster.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use antirion_core::UtcMicros;
use antirion_limits::applicable::NamedBudget;
use serde_json::json;
use tokio::time::Instant;

use super::events::{EventBudget, once_key};
use super::{FORECAST_RULE, budget_path, counters};
use crate::{Runtime, Snapshot};

const TICK: Duration = Duration::from_millis(250);
const EVERY_SECOND: u32 = 4;
const LEASE: &str = "budget_watch";
const LEASE_TTL: Duration = Duration::from_secs(15);
const RENEW_EVERY: Duration = Duration::from_secs(5);

async fn leader(rt: &Runtime, renewed: &mut Option<Instant>) -> bool {
    let (Some(pg), Some(_)) = (rt.store.postgres(), rt.cluster.as_ref()) else {
        return true;
    };
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

pub(super) async fn run(rt: Arc<Runtime>) {
    let mut ticks: u32 = 0;
    let mut renewed = None;
    loop {
        tokio::select! {
            () = rt.shutdown.cancelled() => {
                rt.write_queued_budget_events().await;
                rt.flush_budget_tallies().await;
                return;
            }
            () = tokio::time::sleep(TICK) => {}
        }
        rt.write_queued_budget_events().await;
        rt.flush_budget_tallies().await;
        ticks = ticks.wrapping_add(1);
        if !ticks.is_multiple_of(EVERY_SECOND) {
            continue;
        }
        rt.reload_grants().await;
        let snap = rt.gens.load();
        if snap.limits.has_downgrades() {
            refresh_downgrades(&rt, &snap).await;
        }
        if leader(&rt, &mut renewed).await {
            let now = rt.now();
            if let Err(e) = rt.store.expire_overages(now.0).await {
                tracing::debug!(error = %e, "overage requests could not be expired");
            }
            forecasts(&rt, &snap, now).await;
        }
    }
}

/// Reads the spend of every budget that downgrades into the node's memory.
async fn refresh_downgrades(rt: &Runtime, snap: &Snapshot) {
    let now = rt.now();
    for b in counters(rt, snap).iter().filter(|b| b.budget.downgrade.is_some()) {
        let _ = rt.accounts.budget_state(b, now).await;
    }
}

/// Raises the forecast event and alert of each budget whose projection reached its amount, once per
/// period, and resolves the alerts of budgets whose projection fell back or whose period ended.
async fn forecasts(rt: &Runtime, snap: &Snapshot, now: UtcMicros) {
    for b in counters(rt, snap) {
        let Ok((state, f, _)) = rt.budget_forecast(&b, now).await else {
            continue;
        };
        let path = budget_path(b.decl.level, &b.decl.owner, &b.decl.name);
        let exceeded = f
            .as_ref()
            .and_then(|f| f.projected_ratio)
            .is_some_and(|r| r >= 1.0);
        let raised = rt.state.budgets.forecast_alerts.lock().get(&path).copied();
        if let Some(start) = raised
            && (!exceeded || start != state.period.start.0)
        {
            rt.state.budgets.forecast_alerts.lock().remove(&path);
            crate::alerting::resolve_builtin(rt, FORECAST_RULE, &scope(&b), "system").await;
        }
        let Some(f) = f.filter(|_| exceeded) else {
            continue;
        };
        let event = EventBudget {
            level: b.decl.level,
            owner: &b.decl.owner,
            name: &b.decl.name,
            identity: Some(b.identity),
        };
        let shown = |v: u128| super::amount_text(b.budget, v);
        let runs_out = f.runs_out_at.map(UtcMicros::to_rfc3339_seconds);
        let detail = json!({
            "projected_spend": shown(f.projected_spend),
            "projected_ratio": f.projected_ratio,
            "runs_out_at": runs_out,
            "amount": shown(b.quota),
            "spent": shown(state.view.spent),
            "period_start": state.period.start.to_rfc3339_seconds(),
        });
        let once = once_key("forecast", b.identity, state.period.start);
        if !rt
            .record_budget_event("forecast_exceeded", event, None, &detail, Some(once))
            .await
        {
            continue;
        }
        let data = json!({
            "budget": {"level": b.decl.level.as_str(), "owner": b.decl.owner, "name": b.decl.name},
            "forecast": detail,
        });
        rt.send_alert(snap, None, "budget.forecast_exceeded", data);
        let label = crate::wording::budget_label(&snap.config.doc, b.decl.level, &b.decl.owner, &b.decl.name);
        let worded = |v: u128| crate::wording::budget_amount(&b.budget.unit, v);
        // The figures are those of the moment it fires, which the summary keeps saying.
        let summary = crate::wording::forecast_summary(
            &label,
            &worded(f.projected_spend),
            &worded(b.quota),
            &rt.now().to_rfc3339_seconds(),
        );
        crate::alerting::raise_builtin(rt, FORECAST_RULE, "budget", "warning", scope(&b), summary.clone())
            .await;
        rt.state
            .budgets
            .forecast_alerts
            .lock()
            .insert(path.clone(), state.period.start.0);
        let body = match &runs_out {
            Some(at) => format!("{summary}; at this rate it runs out at {at}.\n\nBudget: {path}\n"),
            None => format!("{summary}.\n\nBudget: {path}\n"),
        };
        rt.mail_owner(
            snap,
            b.budget,
            &format!("{label} is projected over its amount"),
            body,
        );
    }
}

fn scope(b: &NamedBudget<'_>) -> BTreeMap<String, String> {
    BTreeMap::from([(
        "budget".to_owned(),
        budget_path(b.decl.level, &b.decl.owner, &b.decl.name),
    )])
}
