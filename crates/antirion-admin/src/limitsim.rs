//! `POST /runtime/limits:simulate`: every limit and budget admission would reserve for a request
//! of a key to a Model, each with its current state and what `count` such requests would take,
//! and whether they would be admitted now. Counters are read, never changed.

use std::sync::Arc;
use std::time::Duration;

use antirion_core::UtcMicros;
use antirion_limits::applicable::{RequestAttrs, Scope as LimitScope, applicable};
use antirion_limits::counter::{HoldMode, Rule};
use antirion_limits::estimate::{Estimate, OutSource};
use antirion_limits::money::Nusd;
use antirion_limits::plan::{AdmitInput, PlannedHold, Unit, plan_holds, reading};
use antirion_limits::price::Price;
use antirion_limits::store::{CounterView, ReservationId};
use antirion_limits::weights::Incurred;
use antirion_runtime::identity::KeyRt;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;

const MAX_COUNT: u64 = 1_000_000;

struct Simulation {
    key: String,
    model: String,
    input_tokens: u64,
    output_tokens: u64,
    count: u64,
}

fn number(
    body: &serde_json::Map<String, Value>,
    field: &str,
    default: u64,
    min: u64,
) -> Result<u64, AdminError> {
    match body.get(field) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v
            .as_u64()
            .filter(|n| *n >= min && *n <= MAX_COUNT.saturating_mul(1_000))
            .ok_or_else(|| {
                AdminError::invalid_parameter(format!("{field} must be an integer of at least {min}"))
            }),
    }
}

fn simulation(call: &Call) -> Result<Simulation, AdminError> {
    let body = call.json_body()?;
    let text = |field: &str| {
        body.get(field)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| AdminError::invalid_parameter(format!("{field} is required")))
    };
    let count = number(&body, "count", 1, 1)?;
    if count > MAX_COUNT {
        return Err(AdminError::invalid_parameter("count may not exceed 1000000"));
    }
    Ok(Simulation {
        key: text("key")?,
        model: text("model")?,
        input_tokens: number(&body, "input_tokens", 0, 0)?,
        output_tokens: number(&body, "output_tokens", 0, 0)?,
        count,
    })
}

fn key_named(call: &Call, name: &str) -> Option<Arc<KeyRt>> {
    let id = &call.snap.identity;
    id.keys_by_kid
        .values()
        .chain(id.imported.values())
        .map(|s| &s.key)
        .find(|k| k.name == name)
        .or_else(|| id.jwt_keys.get(name))
        .cloned()
}

/// `count` requests of `ρ` each against a view: whether they fit, and what they take.
fn fits(h: &PlannedHold, v: &CounterView, count: u64) -> (u128, bool) {
    let take = h.hold.amount.saturating_mul(u128::from(count));
    let fits = match &h.hold.rule {
        Rule::Budget { amount, .. } => v.spent.saturating_add(v.reserved).saturating_add(take) <= *amount,
        Rule::Rolling(w) => v.spent.saturating_add(take) <= u128::from(w.limit),
        _ => take <= v.remaining,
    };
    (take, fits || h.hold.mode != HoldMode::Enforce)
}

fn usd(n: u128) -> String {
    Nusd::from_units(n).to_usd_string()
}

fn limit_json(h: &PlannedHold, v: &CounterView, count: u64, now: UtcMicros) -> Value {
    let (quota, per) = match &h.hold.rule {
        Rule::Gcra(g) => (u128::from(g.limit), Some(g.per_us.div_ceil(1_000_000))),
        Rule::Window(w) => (u128::from(w.limit), Some(w.per_us.div_ceil(1_000_000))),
        Rule::Concurrency { limit } => (u128::from(*limit), None),
        Rule::Budget { amount, .. } => (*amount, None),
        Rule::Rolling(w) => (u128::from(w.limit), None),
    };
    let remaining = v.remaining.min(quota);
    let (take, admitted) = fits(h, v, count);
    let reset_at = (v.full_in_us > 0 && per.is_some()).then(|| {
        now.saturating_add(Duration::from_micros(v.full_in_us))
            .to_rfc3339_seconds()
    });
    json!({
        "level": h.info.level.as_str(),
        "owner": h.info.owner,
        "name": h.info.name,
        "unit": h.info.unit.name(),
        "limit": quota,
        "per_s": per,
        "used": quota.saturating_sub(remaining),
        "remaining": remaining,
        "reset_at": reset_at,
        "enforced": h.hold.mode == HoldMode::Enforce,
        "requested": take,
        "admitted": admitted,
    })
}

fn budget_json(h: &PlannedHold, v: &CounterView, count: u64, now: UtcMicros) -> Value {
    let (amount, period) = match &h.hold.rule {
        Rule::Budget { amount, period } => (amount, *period),
        Rule::Rolling(w) => match h.hold.rule.budget_period(now) {
            Some(p) => (&u128::from(w.limit), p),
            None => return Value::Null,
        },
        _ => return Value::Null,
    };
    let in_usd = h.info.unit == Unit::Usd;
    let text = |n: u128| if in_usd { usd(n) } else { n.to_string() };
    let (take, admitted) = fits(h, v, count);
    json!({
        "level": h.info.level.as_str(),
        "owner": h.info.owner,
        "name": h.info.name,
        "unit": if in_usd { "usd" } else { "tokens" },
        "mode": if h.hold.mode == HoldMode::Enforce { "hard" } else { "soft" },
        "amount": text(*amount),
        "spent": text(v.spent),
        "reserved": text(v.reserved),
        "remaining": text(amount.saturating_sub(v.spent.saturating_add(v.reserved))),
        "period_start": period.start.to_rfc3339_seconds(),
        "resets_at": period.end.map(UtcMicros::to_rfc3339_seconds),
        "requested": text(take),
        "admitted": admitted,
    })
}

pub(crate) async fn simulate(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&[])?;
    let sim = simulation(call)?;
    let snap = Arc::clone(&call.snap);
    let key = key_named(call, &sim.key).ok_or_else(AdminError::not_found)?;
    let scopes = snap.identity.project_scopes(&key.project);
    if !call.principal.sees(&scopes) {
        return Err(AdminError::not_found());
    }
    call.require("policies:read", &scopes)?;
    let resolved = snap
        .routing
        .resolve(&sim.model)
        .ok_or_else(AdminError::not_found)?;
    let model = &resolved.model;
    let deployments: Vec<_> = snap
        .routing
        .route_of(model)
        .map(|r| antirion_router::deployments_of(&r.tree))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|d| snap.providers.deployments.get(d))
        .collect();
    let prices: Vec<_> = deployments
        .iter()
        .map(|d| {
            snap.pricing
                .price(d.provider.kind.as_str(), &d.base_model, &d.name)
        })
        .collect();
    let price_refs: Vec<Option<&Price>> = prices.iter().map(|p| p.as_deref().map(|p| &**p)).collect();
    let tags: Vec<String> = snap
        .identity
        .chain(&key)
        .iter()
        .flatten()
        .flat_map(|t| t.tags.iter().cloned())
        .chain(key.tags.iter().cloned())
        .collect();
    let now = call.rt.now();
    let input = AdmitInput {
        uid: ReservationId(0),
        attrs: RequestAttrs {
            organization: &key.organization,
            team: &key.team,
            project: &key.project,
            key: &key.name,
            user: key.owner.as_deref(),
            model: Some(&model.name),
            route: model.named_route.then_some(model.route.as_str()),
            endpoint: "chat",
            tags: &tags,
            ..RequestAttrs::default()
        },
        estimate: Estimate {
            input: sim.input_tokens,
            output: sim.output_tokens,
            out_source: OutSource::Request,
            clamp_to: None,
        },
        hedge: 1,
        incurs: Incurred::default(),
        candidates: &price_refs,
        fixed: antirion_limits::cost::FixedUnits::default(),
        multiplier: snap
            .pricing
            .multiplier(&key.organization, &key.team, &key.project, &key.name),
        now,
    };
    let missing = |attribute: &str| {
        AdminError::invalid_parameter(format!(
            "a limit partitions on {attribute}, which a simulated request does not carry"
        ))
    };
    let set =
        applicable(&snap.limits, &input.attrs, LimitScope::Request).map_err(|e| missing(&e.attribute))?;
    let mut readings: Vec<PlannedHold> = set.iter().map(|a| reading(a, now)).collect();
    readings.sort_by_key(|h| h.hold.counter);
    readings.dedup_by(|a, b| a.hold.counter == b.hold.counter);
    let views = call
        .rt
        .accounts
        .read(&readings)
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    let (planned, _) = plan_holds(&snap.limits, &input, LimitScope::Request)
        .map_err(|_| AdminError::invalid_parameter("the request cannot be planned"))?;
    let holds: Vec<PlannedHold> = readings
        .into_iter()
        .map(|mut h| {
            h.hold.amount = planned
                .iter()
                .find(|p| p.hold.counter == h.hold.counter)
                .map_or(0, |p| p.hold.amount);
            h
        })
        .collect();
    let mut limits = Vec::new();
    let mut budgets = Vec::new();
    let mut admitted = true;
    for (h, v) in holds.iter().zip(&views) {
        admitted &= fits(h, v, sim.count).1;
        if h.info.unit.is_budget() {
            budgets.push(budget_json(h, v, sim.count, now));
        } else {
            limits.push(limit_json(h, v, sim.count, now));
        }
    }
    Ok(call.json(
        StatusCode::OK,
        &json!({"admitted": admitted, "count": sim.count, "limits": limits, "budgets": budgets}),
    ))
}
