//! Usage settled after its request: a batch's, read once the batch completes, and each response
//! of a Realtime session. Nothing was reserved for it, so nothing refuses it: each call gets its
//! own ledger line under the request's id, priced with the deployment that served it, and is
//! charged to every budget that applies to the request.

use std::sync::Arc;

use antirion_core::UtcMicros;
use antirion_limits::cost::{self, CostStatus};
use antirion_limits::estimate::OutSource;
use antirion_limits::ledger::{LedgerLine, LineStatus, Purpose, Reserved, UsageSource};
use antirion_limits::level::Level;
use antirion_limits::money::{Dec, Nusd};
use antirion_limits::usage::{Units, Usage};
use antirion_providers::DeploymentRt;
use antirion_runtime::{Runtime, Snapshot};
use serde_json::{Value, json};

use super::{LineFacts, Slot, price_of};

/// What a deferred line inherits from the request it belongs to, kept with a batch until it
/// completes.
#[derive(Clone, Debug)]
pub(crate) struct Deferred {
    pub(crate) request_id: String,
    pub(crate) admitted_at: UtcMicros,
    pub(crate) facts: LineFacts,
}

const ALL_LEVELS: &[Level] = &[
    Level::Gateway,
    Level::Organization,
    Level::Team,
    Level::Project,
    Level::Key,
    Level::Route,
    Level::Model,
];

impl Slot {
    /// The facts deferred lines of this request are written with, while it is admitted.
    pub(crate) fn deferred(&self) -> Option<Deferred> {
        let s = self.lock();
        let p = s.pending.as_ref()?;
        Some(Deferred {
            request_id: p.request_id.clone(),
            admitted_at: p.reservation.as_ref().map_or(p.rt.now(), |r| r.admitted_at),
            facts: p.facts.clone(),
        })
    }
}

impl Deferred {
    /// The facts as JSON, to keep with the object they will be settled for.
    pub(crate) fn to_json(&self) -> Value {
        let f = &self.facts;
        json!({
            "request_id": self.request_id,
            "admitted_at": self.admitted_at.0,
            "organization": f.organization,
            "team": f.team,
            "project": f.project,
            "key": f.key,
            "user": f.user,
            "end_user": f.end_user,
            "tags": f.tags,
            "model": f.model,
            "route": f.route,
            "multiplier": f.multiplier.to_string(),
        })
    }

    /// The facts kept by [`Deferred::to_json`], for an endpoint.
    pub(crate) fn from_json(v: &Value, endpoint: &'static str) -> Option<Deferred> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        Some(Deferred {
            request_id: s("request_id")?,
            admitted_at: UtcMicros(v.get("admitted_at")?.as_i64()?),
            facts: LineFacts {
                organization: s("organization")?,
                team: s("team")?,
                project: s("project")?,
                key: s("key")?,
                user: s("user"),
                end_user: s("end_user"),
                tags: v
                    .get("tags")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                    .unwrap_or_default(),
                model: s("model").unwrap_or_default(),
                route: s("route"),
                endpoint,
                stream: false,
                ip: None,
                multiplier: s("multiplier")
                    .and_then(|m| Dec::parse(&m).ok())
                    .unwrap_or(Dec::ONE),
                priority_class: None,
            },
        })
    }
}

/// One deferred call: its deployment, its line number and what it used.
pub(crate) struct DeferredCall<'a> {
    pub(crate) deployment: &'a DeploymentRt,
    pub(crate) line: u32,
    pub(crate) usage: Usage,
    pub(crate) raw: Option<Value>,
    /// The usage is a Batches API job's: the price's batch multiplier applies.
    pub(crate) batch: bool,
    pub(crate) purpose: Purpose,
    pub(crate) status: LineStatus,
}

/// Writes one deferred line and charges it to the request's budgets; its billed cost.
pub(crate) async fn settle_deferred(
    rt: &Arc<Runtime>,
    snap: &Snapshot,
    d: &Deferred,
    call: DeferredCall<'_>,
) -> Option<Nusd> {
    let dep = call.deployment;
    let units = Units {
        requests: u64::from(!call.usage.is_zero()),
        ..Units::default()
    };
    let priced = price_of(snap, dep).map(|p| {
        (
            cost::cost(
                &p.for_call(antirion_limits::price::CallTerms {
                    service_tier: None,
                    batch: call.batch,
                }),
                &call.usage,
                &units,
                None,
                d.facts.multiplier,
            ),
            p.version(),
        )
    });
    let (cost, billed, cost_status, version) = match priced {
        Some((Ok(c), v)) => (Some(c.total), Some(c.billed), c.status, Some(v)),
        Some((Err(_), v)) => (None, None, CostStatus::Estimated, Some(v)),
        None => (Some(Nusd::ZERO), Some(Nusd::ZERO), CostStatus::Unpriced, None),
    };
    let f = &d.facts;
    let mut line = LedgerLine {
        request_id: d.request_id.clone(),
        line: call.line,
        purpose: call.purpose,
        shadow: false,
        status: call.status,
        admitted_at: d.admitted_at,
        settled_at: rt.now(),
        organization: f.organization.clone(),
        team: f.team.clone(),
        project: f.project.clone(),
        key: f.key.clone(),
        user: f.user.clone(),
        end_user: f.end_user.clone(),
        tags: f.tags.clone(),
        model: (!f.model.is_empty()).then(|| f.model.clone()),
        route: f.route.clone(),
        provider: Some(dep.provider.name.clone()),
        deployment: Some(dep.name.clone()),
        upstream_model: Some(dep.model.clone()),
        call: antirion_limits::ledger::CallTarget::default(),
        endpoint: f.endpoint.to_owned(),
        stream: f.stream,
        usage: call.usage,
        usage_raw: call.raw,
        usage_source: UsageSource::Upstream,
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
    };
    let amount = line.billed.unwrap_or(Nusd::ZERO);
    if let Ok(holds) = antirion_limits::shadow::charges_at(
        &snap.limits,
        &f.attrs(),
        d.admitted_at,
        &line.usage,
        amount,
        ALL_LEVELS,
    ) {
        line.budgets = rt.accounts.charge_shadow(&holds).await;
    }
    rt.record_spend(vec![line]);
    billed
}
