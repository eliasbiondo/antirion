//! Settlement of an auxiliary call of a request: a shadow (mirrored) call, flagged `shadow` and
//! charged to the gateway- and project-level budgets, or a routing call (`router`, tagged
//! `antirion.routing`) charged to the gateway-level budgets (and the project's with
//! `budgets.charge_auxiliary`). Each gets its own ledger line, priced
//! with the deployment that answered, after the fact: nothing was reserved, so nothing refuses it.

use std::sync::Arc;

use antirion_core::UtcMicros;
use antirion_limits::cost::{self, CostStatus};
use antirion_limits::estimate::OutSource;
use antirion_limits::ledger::{LedgerLine, LineStatus, Purpose, Reserved, UsageSource};
use antirion_limits::money::Nusd;
use antirion_limits::usage::{Units, Usage};
use antirion_providers::DeploymentRt;
use antirion_runtime::{Runtime, Snapshot};
use serde_json::Value;

use super::{LineFacts, Slot, price_of};
use crate::reqlog::Usage as Reported;

/// What a shadow call's line inherits from the request it copies.
#[derive(Clone, Debug)]
pub(crate) struct ShadowFacts {
    request_id: String,
    facts: LineFacts,
    admitted_at: UtcMicros,
}

impl Slot {
    /// The facts a shadow call of this request is settled with, while the request is admitted.
    pub(crate) fn shadow_facts(&self) -> Option<ShadowFacts> {
        let s = self.lock();
        let p = s.pending.as_ref()?;
        Some(ShadowFacts {
            request_id: p.request_id.clone(),
            facts: p.facts.clone(),
            admitted_at: p.reservation.as_ref().map_or(p.rt.now(), |r| r.admitted_at),
        })
    }
}

/// How a shadow call ended.
pub(crate) struct ShadowCall<'a> {
    pub(crate) deployment: &'a DeploymentRt,
    /// The line number, beyond the request's own attempts.
    pub(crate) line: u32,
    /// The upstream answered with a 2xx status.
    pub(crate) answered: bool,
    pub(crate) usage: Option<Reported>,
    pub(crate) raw: Option<Value>,
    /// `E_in`, which completes a report without input tokens.
    pub(crate) e_in: u64,
    /// `shadow`, or `router` for a classifier or scorer call.
    pub(crate) purpose: Purpose,
}

/// The reserved tag of routing calls.
const ROUTING_TAG: &str = "antirion.routing";

/// The usage, its source and the line status of a shadow call.
fn outcome(answered: bool, reported: Option<&Reported>, e_in: u64) -> (Usage, UsageSource, LineStatus) {
    let (usage, source) = match (reported, answered) {
        (Some(u), true) => {
            let (n, estimated) = super::settle::completed(u, e_in);
            (
                n,
                if estimated {
                    UsageSource::Estimated
                } else {
                    UsageSource::Upstream
                },
            )
        }
        (Some(u), false) if !u.is_empty() => (u.normalised(), UsageSource::Upstream),
        (None, true) => (Usage::estimated(e_in, 0), UsageSource::Estimated),
        _ => (Usage::default(), UsageSource::Upstream),
    };
    let status = match (answered, usage.is_zero()) {
        (true, _) => LineStatus::Success,
        (false, false) => LineStatus::FailedWithUsage,
        (false, true) => LineStatus::Failed,
    };
    (usage, source, status)
}

fn line_of(snap: &Snapshot, f: &ShadowFacts, call: &ShadowCall<'_>, now: UtcMicros) -> LedgerLine {
    let d = call.deployment;
    let (usage, source, status) = outcome(call.answered, call.usage.as_ref(), call.e_in);
    let units = Units {
        requests: u64::from(status == LineStatus::Success && !usage.is_zero()),
        ..Units::default()
    };
    let priced = price_of(snap, d).map(|p| {
        (
            cost::cost(&p, &usage, &units, None, f.facts.multiplier),
            p.version(),
        )
    });
    let (cost, billed, cost_status, version) = match priced {
        Some((Ok(c), v)) => {
            let s = if source == UsageSource::Estimated {
                CostStatus::Estimated
            } else {
                c.status
            };
            (Some(c.total), Some(c.billed), s, Some(v))
        }
        Some((Err(_), v)) => (None, None, CostStatus::Estimated, Some(v)),
        None => (Some(Nusd::ZERO), Some(Nusd::ZERO), CostStatus::Unpriced, None),
    };
    let facts = &f.facts;
    let mut tags = facts.tags.clone();
    if call.purpose == Purpose::Router {
        tags.push(ROUTING_TAG.to_owned());
    }
    LedgerLine {
        request_id: f.request_id.clone(),
        line: call.line,
        purpose: call.purpose,
        shadow: call.purpose == Purpose::Shadow,
        status,
        admitted_at: f.admitted_at,
        settled_at: now,
        organization: facts.organization.clone(),
        team: facts.team.clone(),
        project: facts.project.clone(),
        key: facts.key.clone(),
        user: facts.user.clone(),
        end_user: facts.end_user.clone(),
        tags,
        model: Some(facts.model.clone()),
        route: facts.route.clone(),
        provider: Some(d.provider.name.clone()),
        deployment: Some(d.name.clone()),
        upstream_model: Some(d.model.clone()),
        call: antirion_limits::ledger::CallTarget::default(),
        endpoint: facts.endpoint.to_owned(),
        stream: facts.stream,
        usage,
        usage_raw: call.raw.clone(),
        usage_source: source,
        cost,
        billed,
        cost_status,
        price_version: version,
        service_tier: None,
        avoided: None,
        reserved: Reserved {
            input: 0,
            output: 0,
            usd: None,
            out_source: OutSource::Request,
        },
        budgets: Vec::new(),
    }
}

/// Settles one shadow call: the budget charges first, then the line that attributes them.
/// Settles a copy and returns its cost.
pub(crate) async fn settle_shadow(
    rt: &Arc<Runtime>,
    snap: &Snapshot,
    f: &ShadowFacts,
    call: &ShadowCall<'_>,
) -> Option<antirion_limits::money::Nusd> {
    let mut line = line_of(snap, f, call, rt.now());
    let cost = line.cost;
    let billed = line.billed.unwrap_or(Nusd::ZERO);
    if let Ok(mut holds) =
        antirion_limits::shadow::charges(&snap.limits, &f.facts.attrs(), f.admitted_at, &line.usage, billed)
    {
        if call.purpose != Purpose::Shadow && !snap.limits.budgets.charge_auxiliary {
            holds.retain(|h| h.info.level == antirion_limits::level::Level::Gateway);
        }
        line.budgets = rt.accounts.charge_shadow(&holds).await;
    }
    rt.record_spend(vec![line]);
    cost
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes_of_a_copy() {
        let reported = Reported {
            input: Some(3),
            output: Some(4),
            ..Reported::default()
        };
        let (u, source, status) = outcome(true, Some(&reported), 10);
        assert_eq!((u.input, u.output), (3, 4));
        assert_eq!((source, status), (UsageSource::Upstream, LineStatus::Success));
        let (u, source, status) = outcome(true, None, 10);
        assert_eq!((u.input, u.output), (10, 0));
        assert_eq!((source, status), (UsageSource::Estimated, LineStatus::Success));
        let (u, _, status) = outcome(false, None, 10);
        assert!(u.is_zero());
        assert_eq!(status, LineStatus::Failed);
        let (_, _, status) = outcome(false, Some(&reported), 10);
        assert_eq!(status, LineStatus::FailedWithUsage);
    }
}
