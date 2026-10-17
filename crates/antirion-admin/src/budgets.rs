//! Budget counters: `GET /runtime/budgets` (filtered by `level`, `owner`, `name`, `partition`),
//! `GET /runtime/budgets/{level}/{owner}/{name}`, and the `:adjust` and `:reset` actions, which go
//! through the ledger as `adjustment` lines so ledger sums keep equalling the counters.

use std::collections::{BTreeMap, HashMap};

use antirion_config::{ConfigError, DetailCode};
use antirion_core::audit::Outcome;
use antirion_core::{RequestId, UtcMicros};
use antirion_identity::rbac::Scope;
use antirion_limits::applicable::{NamedBudget, SHARED_VALUE, named_budget};
use antirion_limits::cost::CostStatus;
use antirion_limits::decl::BudgetUnit;
use antirion_limits::decl::DowngradeTo;
use antirion_limits::estimate::OutSource;
use antirion_limits::forecast::Pace;
use antirion_limits::ledger::{BudgetCharge, LedgerLine, LineStatus, Purpose, Reserved, UsageSource};
use antirion_limits::level::Level;
use antirion_limits::money::Nusd;
use antirion_limits::policy::DeclKind;
use antirion_limits::usage::Usage;
use antirion_runtime::budgets::BudgetView;
use antirion_runtime::budgets::events::EventBudget;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;
use crate::{pagination, perms};

const SCOPE: &str = "budgets";

/// `attr=value[,attr=value]` as the Admin API names a partition.
pub(crate) fn parse_partition(s: &str) -> Result<Vec<(String, String)>, AdminError> {
    s.split(',')
        .filter(|p| !p.is_empty())
        .map(|p| {
            p.split_once('=')
                .map(|(a, v)| (a.to_owned(), v.to_owned()))
                .ok_or_else(|| AdminError::invalid_parameter("partition must be attribute=value"))
        })
        .collect()
}

pub(crate) fn show_partition(p: &[(String, String)]) -> Value {
    if p.is_empty() {
        return Value::Null;
    }
    let text: Vec<String> = p
        .iter()
        .map(|(a, v)| {
            let v = if v == SHARED_VALUE { "" } else { v.as_str() };
            format!("{a}={v}")
        })
        .collect();
    Value::String(text.join(","))
}

/// The scopes containing a budget's owner, broadest first.
pub(crate) fn chain(call: &Call, level: Level, owner: &str) -> Vec<Scope> {
    let collection = level.collection();
    let spec = call.snap.config.doc.get(collection).and_then(|c| c.get(owner));
    match level {
        Level::Organization | Level::Team | Level::Project | Level::Key => {
            perms::chain(&call.snap, collection, owner, spec)
        }
        _ => vec![Scope::Gateway],
    }
}

/// The project end-user partitions of a budget of `(level, owner)` are scoped to, when there is
/// one.
pub(crate) fn project_of(call: &Call, level: Level, owner: &str) -> Option<String> {
    chain(call, level, owner).into_iter().find_map(|s| match s {
        Scope::Project(p) => Some(p),
        _ => None,
    })
}

fn amount_text(unit: &BudgetUnit, v: u128) -> String {
    match unit {
        BudgetUnit::Usd => Nusd::from_units(v).to_usd_string(),
        BudgetUnit::Tokens(_) => v.to_string(),
    }
}

/// What the parent budgets of the configuration have allocated to their children.
pub(crate) type Allocated = HashMap<(Level, String, String), u128>;

pub(crate) fn allocated(call: &Call) -> Allocated {
    antirion_limits::allocation::allocations(&call.snap.config.doc)
        .into_iter()
        .map(|a| (a.parent, a.allocated))
        .collect()
}

fn mode(b: &antirion_limits::decl::BudgetDecl) -> &'static str {
    match (b.hard, b.approval) {
        (_, true) => "approval",
        (true, false) => "hard",
        (false, false) => "soft",
    }
}

fn state_json(b: &NamedBudget<'_>, v: &BudgetView, children: &Allocated) -> Value {
    let unit = &b.budget.unit;
    let st = &v.state;
    let text = |n: u128| amount_text(unit, n);
    let downgrade = b.budget.downgrade.as_ref().map(|d| {
        let ratio = d.at.to_string().parse::<f64>().unwrap_or_default();
        match &d.to {
            DowngradeTo::Model(m) => json!({"at": ratio, "to_model": m}),
            DowngradeTo::Tier(t) => json!({"at": ratio, "to_tier": t}),
        }
    });
    let grants: Vec<Value> = v
        .grants
        .iter()
        .map(|g| {
            json!({
                "id": g.id,
                "amount": g.amount.parse::<u128>().map_or_else(|_| g.amount.clone(), text),
                "expires_at": UtcMicros(g.expires_us).to_rfc3339_seconds(),
                "reason": g.reason,
                "granted_by": g.granted_by,
                "created_at": UtcMicros(g.created_us).to_rfc3339_seconds(),
            })
        })
        .collect();
    let key = (b.decl.level, b.decl.owner.clone(), b.decl.name.clone());
    let mut item = json!({
        "level": b.decl.level.as_str(),
        "owner": b.decl.owner,
        "name": b.decl.name,
        "partition": show_partition(&b.partition),
        "unit": match unit { BudgetUnit::Usd => "usd", BudgetUnit::Tokens(_) => "tokens" },
        "mode": mode(b.budget),
        "amount": text(b.quota),
        "effective_amount": text(st.effective),
        "spent": text(st.view.spent),
        "reserved": text(st.view.reserved),
        "remaining": text(st.effective.saturating_sub(st.view.spent.saturating_add(st.view.reserved))),
        "period_start": st.period.start.to_rfc3339_seconds(),
        "resets_at": st.period.end.map(UtcMicros::to_rfc3339_seconds),
        "budget_owner": b.budget.owner,
        "state": v.pace.as_str(),
        "grants": grants,
        "refused_requests": v.refused,
        "downgraded_requests": v.downgraded,
        "children_allocated": children.get(&key).map(|n| text(*n)),
        "downgrade": downgrade,
    });
    if let Some(f) = &v.forecast {
        item["forecast"] = json!({
            "rate_per_day": text(f.rate_per_day),
            "projected_spend": text(f.projected_spend),
            "projected_ratio": f.projected_ratio,
            "runs_out_at": f.runs_out_at.map(UtcMicros::to_rfc3339_seconds),
            "basis_from": f.basis_from.to_rfc3339_seconds(),
            "basis_to": f.basis_to.to_rfc3339_seconds(),
        });
    }
    item
}

pub(crate) async fn state(
    call: &Call,
    b: &NamedBudget<'_>,
    children: &Allocated,
) -> Result<Value, AdminError> {
    let v = call
        .rt
        .budget_view(b)
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    Ok(state_json(b, &v, children))
}

/// A listed counter with the figures it can be sorted by.
struct Listed {
    item: Value,
    pace: Pace,
    projected: u128,
    spent: u128,
    ratio: f64,
}

/// `GET /runtime/budgets`.
/// Whether the principal may read the counters of budgets on `scopes`: who reads the policies, or
/// the spend analytics of that scope, since a counter is that scope's spend against its limit.
pub(crate) fn reads_counters(call: &Call, scopes: &[Scope]) -> bool {
    call.principal.allows("policies:read", scopes) || call.principal.allows("analytics:read", scopes)
}

pub(crate) async fn list(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&[
        "level",
        "owner",
        "name",
        "partition",
        "state",
        "sort",
        "limit",
        "cursor",
    ])?;
    let (limit, after) = pagination::page_params(call, SCOPE)?;
    let level = match call.param("level") {
        Some(l) => Some(Level::parse(l).ok_or_else(|| AdminError::invalid_parameter("unknown level"))?),
        None => None,
    };
    let wanted_state = match call.param("state") {
        Some(s) => Some(Pace::parse(s).ok_or_else(|| {
            AdminError::invalid_parameter("state must be on_pace, at_risk, over or blocked")
        })?),
        None => None,
    };
    let sort = call.param("sort");
    if sort.is_some_and(|s| !matches!(s, "projected_spend" | "spent" | "ratio")) {
        return Err(AdminError::invalid_parameter(
            "sort must be projected_spend, spent or ratio",
        ));
    }
    let wanted = call.param("partition").map(parse_partition).transpose()?;
    call.rt.flush_budget_tallies().await;
    let children = allocated(call);
    let policies = &call.snap.limits;
    let mut items: Vec<Listed> = Vec::new();
    for d in policies.all() {
        let DeclKind::Budget(budget) = &d.kind else {
            continue;
        };
        if level.is_some_and(|l| l != d.level)
            || call.param("owner").is_some_and(|o| o != d.owner)
            || call.param("name").is_some_and(|n| n != d.name)
        {
            continue;
        }
        let scopes = chain(call, d.level, &d.owner);
        if !call.principal.sees(&scopes) || !reads_counters(call, &scopes) {
            continue;
        }
        let partitions = match &wanted {
            Some(p) => vec![p.clone()],
            None if budget.partition.attrs.is_empty() => vec![Vec::new()],
            None => call.rt.accounts.partitions_of(d.level, &d.owner, &d.name),
        };
        for p in partitions {
            let project = project_of(call, d.level, &d.owner);
            let Some(b) = named_budget(policies, d.level, &d.owner, &d.name, &p, project.as_deref()) else {
                continue;
            };
            let v = call
                .rt
                .budget_view(&b)
                .await
                .map_err(|_| AdminError::store_unavailable())?;
            #[allow(clippy::cast_precision_loss)]
            let ratio = if b.quota == 0 {
                0.0
            } else {
                v.state.view.spent as f64 / b.quota as f64
            };
            items.push(Listed {
                item: state_json(&b, &v, &children),
                pace: v.pace,
                projected: v
                    .forecast
                    .as_ref()
                    .map_or(v.state.view.spent, |f| f.projected_spend),
                spent: v.state.view.spent,
                ratio,
            });
        }
    }
    let mut counts: BTreeMap<&str, usize> = [Pace::OnPace, Pace::AtRisk, Pace::Over, Pace::Blocked]
        .into_iter()
        .map(|p| (p.as_str(), 0))
        .collect();
    for i in &items {
        *counts.entry(i.pace.as_str()).or_default() += 1;
    }
    let total = items.len();
    items.retain(|i| wanted_state.is_none_or(|w| w == i.pace));
    match sort {
        Some("projected_spend") => items.sort_by_key(|a| std::cmp::Reverse(a.projected)),
        Some("spent") => items.sort_by_key(|a| std::cmp::Reverse(a.spent)),
        Some("ratio") => items.sort_by(|a, b| b.ratio.total_cmp(&a.ratio)),
        _ => {}
    }
    let start = after.and_then(|a| a.parse::<usize>().ok()).unwrap_or(0);
    let page: Vec<Value> = items
        .iter()
        .skip(start)
        .take(limit)
        .map(|i| i.item.clone())
        .collect();
    let next = (start + page.len() < items.len())
        .then(|| pagination::encode(&call.rt.cursor_key, SCOPE, &(start + page.len()).to_string()));
    Ok(call.json(
        StatusCode::OK,
        &json!({"items": page, "next_cursor": next, "counts": counts, "total": total}),
    ))
}

/// `/runtime/budgets/{level}/{owner}/{name}` and its `:adjust` and `:reset` actions.
pub(crate) async fn one(call: &Call, level: &str, owner: &str, last: &str) -> Result<Response, AdminError> {
    let (name, action) = match last.split_once(':') {
        Some((n, a)) => (n, Some(a)),
        None => (last, None),
    };
    let level = Level::parse(level).ok_or_else(AdminError::not_found)?;
    let scopes = chain(call, level, owner);
    if !call.principal.sees(&scopes) {
        return Err(AdminError::not_found());
    }
    match action {
        None => {
            if call.method != Method::GET {
                return Err(AdminError::method_not_allowed("GET"));
            }
            call.allow_params(&["partition"])?;
            if !reads_counters(call, &scopes) {
                return Err(AdminError::forbidden("policies:read or analytics:read"));
            }
            let p = call
                .param("partition")
                .map(parse_partition)
                .transpose()?
                .unwrap_or_default();
            let b = named_budget(
                &call.snap.limits,
                level,
                owner,
                name,
                &p,
                project_of(call, level, owner).as_deref(),
            )
            .ok_or_else(AdminError::not_found)?;
            call.rt.flush_budget_tallies().await;
            let body = state(call, &b, &allocated(call)).await?;
            Ok(call.json(StatusCode::OK, &body))
        }
        Some("grant_overage") => Box::pin(crate::overage::grant(call, level, owner, name, &scopes)).await,
        Some(a @ ("adjust" | "reset")) => {
            if call.method != Method::POST {
                return Err(AdminError::method_not_allowed("POST"));
            }
            call.require("budgets:adjust", &scopes)?;
            if a == "reset"
                && let Some(held) =
                    Box::pin(crate::change_control::second_person(call, "budget_reset")).await?
            {
                return Ok(held);
            }
            adjust(call, level, owner, name, a == "reset").await
        }
        Some(_) => Err(AdminError::not_found()),
    }
}

fn amount_error(message: &str) -> AdminError {
    AdminError::validation(vec![ConfigError {
        path: "amount".into(),
        code: DetailCode::OutOfRange,
        message: message.into(),
    }])
}

/// The tenancy of an adjustment line: the owner's chain.
fn tenancy(call: &Call, level: Level, owner: &str) -> [String; 4] {
    let mut out: [String; 4] = Default::default();
    for s in chain(call, level, owner) {
        match s {
            Scope::Organization(o) => out[0] = o,
            Scope::Team(t) => out[1] = t,
            Scope::Project(p) => out[2] = p,
            Scope::Gateway => {}
        }
    }
    if level == Level::Key {
        out[3] = owner.to_owned();
    }
    out
}

async fn adjust(
    call: &Call,
    level: Level,
    owner: &str,
    name: &str,
    reset: bool,
) -> Result<Response, AdminError> {
    let mut reason = call.reason.clone();
    let (partition_text, amount) = if reset {
        call.allow_params(&["partition"])?;
        (call.param("partition").map(str::to_owned), None)
    } else {
        call.allow_params(&[])?;
        let body = call.json_body()?;
        if body.get("amount").is_none() {
            return Err(amount_error("amount is required"));
        }
        let partition = body.get("partition").and_then(Value::as_str).map(str::to_owned);
        if let Some(r) = body.get("reason").and_then(Value::as_str) {
            reason = Some(r.to_owned());
        }
        (partition, body.get("amount").cloned())
    };
    let p = partition_text
        .as_deref()
        .map(parse_partition)
        .transpose()?
        .unwrap_or_default();
    let policies = &call.snap.limits;
    let project = project_of(call, level, owner);
    let b = named_budget(policies, level, owner, name, &p, project.as_deref())
        .ok_or_else(AdminError::not_found)?;
    let now = call.rt.now();
    let delta: i128 = match amount {
        None => {
            let st = call
                .rt
                .accounts
                .budget_state(&b, now)
                .await
                .map_err(|_| AdminError::store_unavailable())?;
            -i128::try_from(st.view.spent).unwrap_or(i128::MAX)
        }
        Some(v) => match &b.budget.unit {
            BudgetUnit::Usd => v
                .as_str()
                .and_then(|s| Nusd::parse_signed_usd(s).ok())
                .map(|n| n.0)
                .ok_or_else(|| amount_error("amount must be a USD decimal string"))?,
            BudgetUnit::Tokens(_) => v
                .as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                .map(i128::from)
                .ok_or_else(|| amount_error("amount must be an integer"))?,
        },
    };
    let after = call
        .rt
        .accounts
        .adjust(&b, now, delta)
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    if after.is_err() {
        return Err(amount_error("the adjustment would make spent negative"));
    }
    let [organization, team, project, key] = tenancy(call, level, owner);
    let money = matches!(b.budget.unit, BudgetUnit::Usd).then_some(Nusd(delta));
    call.rt.record_spend(vec![LedgerLine {
        request_id: RequestId::generate().as_str().to_owned(),
        line: 0,
        purpose: Purpose::Adjustment,
        shadow: false,
        status: LineStatus::Adjustment,
        admitted_at: now,
        settled_at: now,
        organization,
        team,
        project,
        key,
        user: Some(call.principal.id.clone()),
        end_user: None,
        tags: Vec::new(),
        model: None,
        route: None,
        provider: None,
        deployment: None,
        upstream_model: None,
        call: antirion_limits::ledger::CallTarget::default(),
        endpoint: String::new(),
        stream: false,
        usage: Usage::default(),
        usage_raw: None,
        usage_source: UsageSource::Upstream,
        cost: Some(money.unwrap_or(Nusd::ZERO)),
        billed: Some(money.unwrap_or(Nusd::ZERO)),
        cost_status: CostStatus::Exact,
        price_version: None,
        service_tier: None,
        avoided: None,
        reserved: Reserved {
            input: 0,
            output: 0,
            usd: None,
            out_source: OutSource::Request,
        },
        budgets: vec![BudgetCharge {
            level: level.as_str().to_owned(),
            owner: owner.to_owned(),
            name: name.to_owned(),
            identity: b.identity,
            amount: delta,
        }],
    }]);
    let action = if reset { "budgets.reset" } else { "budgets.adjust" };
    call.audit(
        action,
        &format!("runtime/budgets/{}/{owner}/{name}", level.as_str()),
        Outcome::Success,
        StatusCode::OK,
    )
    .await;
    let event = EventBudget {
        level,
        owner,
        name,
        identity: Some(b.identity),
    };
    let shown = if delta < 0 {
        format!("-{}", amount_text(&b.budget.unit, delta.unsigned_abs()))
    } else {
        amount_text(&b.budget.unit, delta.unsigned_abs())
    };
    let detail = json!({"amount": shown, "partitioned": partition_text.is_some(), "reason": reason});
    call.rt
        .record_budget_event(
            if reset { "reset" } else { "adjusted" },
            event,
            Some(&call.principal.id),
            &detail,
            None,
        )
        .await;
    let body = state(call, &b, &allocated(call)).await?;
    Ok(call.json(StatusCode::OK, &body))
}
