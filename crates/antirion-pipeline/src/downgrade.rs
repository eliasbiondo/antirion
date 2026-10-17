//! Budget downgrades (stage `resolve`): while a budget that covers the request has spent `at` or
//! more of its declared amount, the request is served by the budget's `to_model`, or by the first
//! Model of its `to_tier` (in name order) the key may use, instead of the requested Model. The
//! spend is the one this node last saw (settlements, and a reload every second); nothing here waits
//! on a store. A target the key may not use, or that no longer serves, leaves the request as it was.

use antirion_core::UtcMicros;
use antirion_limits::applicable::{RequestAttrs, Scope, applicable};
use antirion_limits::decl::DowngradeTo;
use antirion_limits::money::DEC_ONE;
use antirion_limits::overlay::Touched;
use antirion_limits::policy::DeclKind;
use antirion_runtime::identity::KeyRt;
use antirion_runtime::{Runtime, Snapshot};

use crate::authorize::{self, Target};

/// A downgrade decided for a request.
pub(crate) struct Downgrade {
    /// `<level>/<owner>/<name>` of the budget.
    pub(crate) budget: String,
    /// The Model the request is served by instead.
    pub(crate) to: String,
    pub(crate) target: Target,
    pub(crate) touched: Touched,
}

/// The downgrade of a request for `attrs.model`, if a budget asks for one.
pub(crate) fn decide(
    rt: &Runtime,
    snap: &Snapshot,
    key: &KeyRt,
    attrs: &RequestAttrs<'_>,
    now: UtcMicros,
) -> Option<Downgrade> {
    if !snap.limits.has_downgrades() {
        return None;
    }
    let current = attrs.model?;
    let covering = applicable(&snap.limits, attrs, Scope::Request).ok()?;
    for a in covering {
        let DeclKind::Budget(b) = &a.decl.kind else {
            continue;
        };
        let Some(dg) = &b.downgrade else {
            continue;
        };
        let period = b.bounds(now);
        let spent = rt.accounts.overlay().spent(a.identity, period.start).unwrap_or(0);
        if spent.saturating_mul(DEC_ONE) < dg.at.0.saturating_mul(a.quota) {
            continue;
        }
        let names: Vec<String> = match &dg.to {
            DowngradeTo::Model(m) => vec![m.clone()],
            DowngradeTo::Tier(t) => snap
                .routing
                .models
                .values()
                .filter(|m| m.enabled && m.tier.as_deref() == Some(t.as_str()))
                .map(|m| m.name.clone())
                .collect(),
        };
        for name in names.into_iter().filter(|n| n != current) {
            let Ok(target) = authorize::resolve(snap, key, &name) else {
                continue;
            };
            if authorize::lifecycle(&target, key, &name, now).is_err() {
                continue;
            }
            return Some(Downgrade {
                budget: antirion_runtime::budgets::budget_path(a.decl.level, &a.decl.owner, &a.decl.name),
                to: name,
                target,
                touched: Touched {
                    identity: a.identity,
                    level: a.decl.level,
                    owner: a.decl.owner.clone(),
                    name: a.decl.name.clone(),
                    partition: a.partition.clone(),
                    period,
                },
            });
        }
    }
    None
}
