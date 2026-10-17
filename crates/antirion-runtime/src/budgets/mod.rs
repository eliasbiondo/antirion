//! What budgets do beyond admission, off the request path: forecasts and the alerts they raise,
//! overage grants and requests, the budget event feed, the per-period counts of refused and
//! downgraded requests, and the notifications of a budget's owner. The request path only reads
//! node memory ([`antirion_limits::overlay::Overlay`]); this module fills it.

use std::collections::HashMap;
use std::sync::Arc;

use antirion_limits::applicable::{NamedBudget, named_budget};
use antirion_limits::decl::BudgetDecl;
use antirion_limits::level::Level;
use antirion_limits::policy::{DeclKind, Declared, Policies};
use antirion_store::BudgetEventRow;
use parking_lot::Mutex;
use serde_json::Value;

use crate::{Runtime, Snapshot};

pub mod config;
pub mod events;
mod grants;
mod overage;
mod view;
mod watch;

pub use grants::NewGrant;
pub use overage::{DecideError, Decision};
pub use view::BudgetView;

/// The built-in alert rule forecasts raise instances of.
pub const FORECAST_RULE: &str = "budget_forecast";

/// Node memory of this module.
#[derive(Debug, Default)]
pub struct BudgetsState {
    /// Events noticed on paths that must not wait for the store, written by the watch.
    queued: Mutex<Vec<(BudgetEventRow, Option<String>)>>,
    /// Forecast alerts this node raised and has not resolved: budget path → period start.
    forecast_alerts: Mutex<HashMap<String, i64>>,
}

impl BudgetsState {
    fn queue(&self, e: BudgetEventRow, once: Option<String>) {
        let mut q = self.queued.lock();
        if q.len() < 10_000 {
            q.push((e, once));
        }
    }

    fn take(&self) -> Vec<(BudgetEventRow, Option<String>)> {
        std::mem::take(&mut *self.queued.lock())
    }

    pub(crate) fn clear(&self) {
        self.queued.lock().clear();
        self.forecast_alerts.lock().clear();
    }
}

/// Starts the scheduled work of this module.
pub fn spawn(rt: &Arc<Runtime>) {
    rt.tasks.spawn(watch::run(Arc::clone(rt)));
}

/// `<level>/<owner>/<name>`: how events, requests and headers name a budget.
#[must_use]
pub fn budget_path(level: Level, owner: &str, name: &str) -> String {
    format!("{}/{owner}/{name}", level.as_str())
}

/// The declaration of a budget.
#[must_use]
pub fn declaration<'p>(
    p: &'p Policies,
    level: Level,
    owner: &str,
    name: &str,
) -> Option<(&'p Declared, &'p BudgetDecl)> {
    p.owned(level, owner)
        .find(|d| d.name == name)
        .and_then(|d| match &d.kind {
            DeclKind::Budget(b) => Some((d, b)),
            DeclKind::Limit(_) => None,
        })
}

/// The project end-user partitions of a budget owned at `level` by `owner` are scoped to.
#[must_use]
pub fn project_of(snap: &Snapshot, level: Level, owner: &str) -> Option<String> {
    match level {
        Level::Project => Some(owner.to_owned()),
        Level::Key => snap
            .config
            .doc
            .pointer(&format!("/keys/{owner}/project"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        _ => None,
    }
}

/// Every counter of every budget this node knows of: the unpartitioned ones, and the partitions
/// it has seen.
pub(crate) fn counters<'p>(rt: &Runtime, snap: &'p Snapshot) -> Vec<NamedBudget<'p>> {
    let mut out = Vec::new();
    for d in snap.limits.all() {
        let DeclKind::Budget(b) = &d.kind else {
            continue;
        };
        let partitions = if b.partition.attrs.is_empty() {
            vec![Vec::new()]
        } else {
            rt.accounts.partitions_of(d.level, &d.owner, &d.name)
        };
        let project = project_of(snap, d.level, &d.owner);
        for p in partitions {
            if let Some(n) = named_budget(&snap.limits, d.level, &d.owner, &d.name, &p, project.as_deref()) {
                out.push(n);
            }
        }
    }
    out
}

/// An amount in a budget's unit as the Admin API shows it.
#[must_use]
pub fn amount_text(b: &BudgetDecl, v: u128) -> String {
    match b.unit {
        antirion_limits::decl::BudgetUnit::Usd => antirion_limits::money::Nusd::from_units(v).to_usd_string(),
        antirion_limits::decl::BudgetUnit::Tokens(_) => v.to_string(),
    }
}

impl Runtime {
    /// Mails a budget's owner when the budget has one with an address and mail is on.
    fn mail_owner(&self, snap: &Snapshot, b: &BudgetDecl, subject: &str, body: String) -> bool {
        let Some(owner) = &b.owner else {
            return false;
        };
        let Some(email) = snap.identity.users.get(owner).and_then(|u| u.email.clone()) else {
            return false;
        };
        self.send_mail(&email, subject, body)
    }
}
