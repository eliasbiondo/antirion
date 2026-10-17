//! `POST /reports/spend:recost`: the ledger lines admitted in a range priced again with the current
//! prices. Each line whose cost changed gets an `adjustment` line with the difference, under its
//! request id and admission time; the USD budgets the original charged take the difference too,
//! and so do their counters of the period in progress. Originals are never rewritten.

use std::collections::HashMap;
use std::sync::Arc;

use antirion_config::DetailCode;
use antirion_core::UtcMicros;
use antirion_core::audit::Outcome;
use antirion_identity::rbac::Scope;
use antirion_limits::applicable::named_budget;
use antirion_limits::cost::CostStatus;
use antirion_limits::decl::BudgetUnit;
use antirion_limits::ledger::{BudgetCharge, LedgerLine, LineStatus, Purpose, Reserved};
use antirion_limits::level::Level;
use antirion_limits::money::Nusd;
use antirion_limits::policy::DeclKind;
use antirion_limits::recost::{original_of, recost_line, reprice, repriceable};
use antirion_limits::usage::Usage;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::json;

use crate::call::Call;
use crate::danger_routes::{body, body_error, text};
use crate::error::AdminError;

/// Whether `segs` is this operation.
pub(crate) fn routes(segs: &[&str]) -> bool {
    matches!(segs, ["reports", "spend:recost"])
}

pub(crate) async fn dispatch(call: &Call, _segs: &[&str]) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&[])?;
    call.require("budgets:adjust", &[Scope::Gateway])?;
    let m = body(call, &["from", "to"])?;
    let time = |member: &str| -> Result<UtcMicros, AdminError> {
        let s = text(&m, member)?.ok_or_else(|| body_error(member, DetailCode::Required, "is required"))?;
        UtcMicros::parse_rfc3339(s)
            .map_err(|_| body_error(member, DetailCode::InvalidFormat, "must be an RFC 3339 time"))
    };
    let (from, to) = (time("from")?, time("to")?);
    if to <= from {
        return Err(body_error("to", DetailCode::OutOfRange, "must be after from"));
    }
    let lines = call
        .rt
        .lines_admitted_between(from, to)
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    let appended = recost(call, &lines).await;
    let n = appended.len();
    call.rt.record_spend(appended);
    call.audit_with(
        "spend.recost",
        "reports/spend",
        Outcome::Success,
        StatusCode::OK,
        json!({"from": from.to_rfc3339(), "to": to.to_rfc3339(), "lines_appended": n}),
    )
    .await;
    Ok(call.json(StatusCode::OK, &json!({"lines_appended": n})))
}

/// What earlier re-costings of one original line added, and how many there were.
#[derive(Default)]
struct Earlier {
    cost: i128,
    billed: i128,
    count: u32,
}

async fn recost(call: &Call, lines: &[Arc<LedgerLine>]) -> Vec<LedgerLine> {
    let mut earlier: HashMap<(&str, u32), Earlier> = HashMap::new();
    for l in lines {
        if let Some(original) = original_of(l.line) {
            let e = earlier.entry((l.request_id.as_str(), original)).or_default();
            e.cost = e.cost.saturating_add(l.cost.map_or(0, |c| c.0));
            e.billed = e.billed.saturating_add(l.billed.map_or(0, |c| c.0));
            e.count += 1;
        }
    }
    let snap = &call.snap;
    let now = call.rt.now();
    let mut out = Vec::new();
    for l in lines.iter().filter(|l| repriceable(l)) {
        let Some(d) = l
            .deployment
            .as_deref()
            .and_then(|name| snap.providers.deployments.get(name))
        else {
            continue;
        };
        let Some(price) = snap
            .pricing
            .price(d.provider.kind.as_str(), &d.base_model, &d.name)
        else {
            continue;
        };
        let multiplier = snap
            .pricing
            .multiplier(&l.organization, &l.team, &l.project, &l.key);
        let Ok(new) = reprice(l, &price, multiplier) else {
            continue;
        };
        let before = earlier.get(&(l.request_id.as_str(), l.line));
        let old_cost = l.cost.map_or(0, |c| c.0) + before.map_or(0, |e| e.cost);
        let old_billed = l.billed.map_or(0, |c| c.0) + before.map_or(0, |e| e.billed);
        let (cost, billed) = (new.cost.0 - old_cost, new.billed.0 - old_billed);
        if cost == 0 && billed == 0 {
            continue;
        }
        let Some(number) = recost_line(l.line, before.map_or(0, |e| e.count)) else {
            continue;
        };
        let budgets = charges(call, l, billed, now).await;
        out.push(LedgerLine {
            line: number,
            purpose: Purpose::Adjustment,
            status: LineStatus::Adjustment,
            settled_at: now,
            usage: Usage::default(),
            usage_raw: None,
            cost: Some(Nusd(cost)),
            billed: Some(Nusd(billed)),
            cost_status: CostStatus::Exact,
            price_version: Some(price.version()),
            avoided: None,
            reserved: Reserved {
                input: 0,
                output: 0,
                usd: None,
                out_source: l.reserved.out_source,
            },
            budgets,
            ..LedgerLine::clone(l)
        });
    }
    out
}

/// The USD budgets `l` charged, each charged `billed` more; the counters of the period in progress
/// (when it contains the line's admission) take the difference at once.
async fn charges(call: &Call, l: &LedgerLine, billed: i128, now: UtcMicros) -> Vec<BudgetCharge> {
    let policies = &call.snap.limits;
    let mut out = Vec::new();
    for c in &l.budgets {
        let Some(level) = Level::parse(&c.level) else {
            continue;
        };
        let usd = policies
            .owned(level, &c.owner)
            .find(|d| d.name == c.name)
            .is_some_and(|d| matches!(&d.kind, DeclKind::Budget(b) if matches!(b.unit, BudgetUnit::Usd)));
        if !usd || billed == 0 {
            continue;
        }
        let project = crate::budgets::project_of(call, level, &c.owner);
        let mut partitions = call.rt.accounts.partitions_of(level, &c.owner, &c.name);
        partitions.push(Vec::new());
        let named = partitions.iter().find_map(|p| {
            named_budget(policies, level, &c.owner, &c.name, p, project.as_deref())
                .filter(|b| b.identity == c.identity)
        });
        if let Some(b) = named
            && b.budget.bounds(now).start <= l.admitted_at
        {
            let _ = call.rt.accounts.adjust(&b, now, billed).await;
        }
        out.push(BudgetCharge {
            amount: billed,
            ..c.clone()
        });
    }
    out
}
