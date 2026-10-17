//! The budget event feed: events written as they happen on admin paths, and queued (then written by
//! the watch) where the caller must not wait for the store: settlements and configuration commits.

use std::collections::BTreeMap;

use antirion_core::UtcMicros;
use antirion_limits::counter::CounterId;
use antirion_limits::decl::BudgetDecl;
use antirion_limits::engine::BudgetAlert;
use antirion_limits::level::Level;
use antirion_limits::policy::DeclKind;
use antirion_store::BudgetEventRow;
use serde_json::{Value, json};

use super::amount_text;
use crate::{Runtime, Snapshot};

/// The budget an event is about.
#[derive(Clone, Copy, Debug)]
pub struct EventBudget<'a> {
    pub level: Level,
    pub owner: &'a str,
    pub name: &'a str,
    pub identity: Option<CounterId>,
}

fn row(rt: &Runtime, kind: &str, b: EventBudget<'_>, actor: Option<&str>, detail: &Value) -> BudgetEventRow {
    BudgetEventRow {
        id: antirion_core::RequestId::generate().as_str().to_owned(),
        time_us: rt.now().0,
        kind: kind.to_owned(),
        level: b.level.as_str().to_owned(),
        owner: b.owner.to_owned(),
        name: b.name.to_owned(),
        actor: actor.map(str::to_owned),
        detail: detail.to_string(),
        identity: b.identity.map(|i| i.hex()),
    }
}

fn mode(b: &BudgetDecl) -> &'static str {
    match (b.hard, b.approval) {
        (_, true) => "approval",
        (true, false) => "hard",
        (false, false) => "soft",
    }
}

/// The parts of a declaration a config change event reports.
fn summary(b: &BudgetDecl) -> Value {
    json!({
        "amount": amount_text(b, b.amount),
        "mode": mode(b),
        "period": b.period_key.split('|').next().unwrap_or_default(),
        "owner": b.owner,
    })
}

impl Runtime {
    /// Writes an event; with `once`, only the first event of that key. `false` when it was not
    /// written (a repeat, or the store failed).
    pub async fn record_budget_event(
        &self,
        kind: &str,
        b: EventBudget<'_>,
        actor: Option<&str>,
        detail: &Value,
        once: Option<String>,
    ) -> bool {
        match self
            .store
            .insert_budget_event(row(self, kind, b, actor, detail), once)
            .await
        {
            Ok(written) => written,
            Err(e) => {
                tracing::warn!(error = %e, kind, "a budget event could not be recorded");
                false
            }
        }
    }

    /// Queues an event for the watch to write.
    pub fn queue_budget_event(&self, kind: &str, b: EventBudget<'_>, actor: Option<&str>, detail: &Value) {
        self.state.budgets.queue(row(self, kind, b, actor, detail), None);
    }

    pub async fn write_queued_budget_events(&self) {
        for (e, once) in self.state.budgets.take() {
            if let Err(err) = self.store.insert_budget_event(e, once).await {
                tracing::warn!(error = %err, "a budget event could not be recorded");
            }
        }
    }

    /// A settlement took a budget past one of its alert ratios: the event, and a mail to the
    /// budget's owner. Only queues work.
    pub fn budget_threshold_crossed(&self, snap: &Snapshot, alert: &BudgetAlert) {
        let Some(level) = Level::parse(alert.level) else {
            return;
        };
        let decl = super::declaration(&snap.limits, level, &alert.owner, &alert.name);
        let shown = |v: u128| decl.map_or_else(|| v.to_string(), |(_, b)| amount_text(b, v));
        let threshold = alert.threshold.to_string();
        let mut notified: Vec<String> =
            crate::telemetry::alerts::targets(&snap.alerts, "budget.threshold_crossed", None)
                .iter()
                .map(|h| format!("webhook:{}", h.name))
                .collect();
        if let Some((_, b)) = decl {
            let label = crate::wording::budget_label(&snap.config.doc, level, &alert.owner, &alert.name);
            let worded = |v: u128| crate::wording::budget_amount(&b.unit, v);
            let past = threshold
                .parse::<f64>()
                .map_or_else(|_| threshold.clone(), crate::wording::percent);
            let body = format!(
                "{label} has spent {} of {} this period, past {past} of its amount.\n\nBudget: {}\n",
                worded(alert.spent),
                worded(alert.amount),
                super::budget_path(level, &alert.owner, &alert.name)
            );
            if self.mail_owner(snap, b, &format!("{label} passed {past}"), body) {
                notified.extend(b.owner.as_ref().map(|o| format!("email:{o}")));
            }
        }
        let detail = json!({
            "threshold": threshold.parse::<f64>().ok(),
            "spent": shown(alert.spent),
            "amount": shown(alert.amount),
            "period_start": alert.period.start.to_rfc3339_seconds(),
            "notified": notified,
        });
        let b = EventBudget {
            level,
            owner: &alert.owner,
            name: &alert.name,
            identity: None,
        };
        self.queue_budget_event("threshold_crossed", b, None, &detail);
    }

    /// Queues a `config_changed` event for every budget declaration a commit added, changed or
    /// removed.
    pub(crate) fn note_budget_changes(&self, previous: &Snapshot, next: &Snapshot, actor: Option<&str>) {
        let decls = |s: &Snapshot| -> BTreeMap<(Level, String, String), BudgetDecl> {
            s.limits
                .all()
                .filter_map(|d| match &d.kind {
                    DeclKind::Budget(b) => Some(((d.level, d.owner.clone(), d.name.clone()), b.clone())),
                    DeclKind::Limit(_) => None,
                })
                .collect()
        };
        let (old, new) = (decls(previous), decls(next));
        let keys: std::collections::BTreeSet<&(Level, String, String)> =
            old.keys().chain(new.keys()).collect();
        for key in keys {
            let (before, after) = (old.get(key), new.get(key));
            let change = match (before, after) {
                (None, Some(_)) => "created",
                (Some(_), None) => "deleted",
                (Some(a), Some(b)) if a != b => "changed",
                _ => continue,
            };
            let detail = json!({
                "change": change,
                "old_amount": before.map(|b| amount_text(b, b.amount)),
                "new_amount": after.map(|b| amount_text(b, b.amount)),
                "old": before.map(summary),
                "new": after.map(summary),
                "generation": next.generation,
            });
            let (level, owner, name) = key;
            let b = EventBudget {
                level: *level,
                owner,
                name,
                identity: None,
            };
            self.queue_budget_event("config_changed", b, actor, &detail);
        }
    }
}

/// When a period's once-only events are keyed: the budget and the period start.
pub(crate) fn once_key(kind: &str, identity: CounterId, period_start: UtcMicros) -> String {
    format!("{kind}|{}|{}", identity.hex(), period_start.0)
}
