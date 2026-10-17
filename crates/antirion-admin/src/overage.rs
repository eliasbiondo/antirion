//! Overage grants and requests: `:grant_overage` and revoking a grant on a budget counter, and the
//! overage requests `approval` budgets open, listed, approved (a grant) and denied.

use antirion_config::{ConfigError, DetailCode, DocPath};
use antirion_core::UtcMicros;
use antirion_core::audit::Outcome;
use antirion_identity::rbac::Scope;
use antirion_limits::applicable::{NamedBudget, named_budget};
use antirion_limits::decl::BudgetUnit;
use antirion_limits::level::Level;
use antirion_limits::money::Nusd;
use antirion_runtime::budgets::events::EventBudget;
use antirion_runtime::budgets::{DecideError, Decision, NewGrant, amount_text, declaration};
use antirion_store::OverageRow;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Map, Value, json};

use crate::budgets::{allocated, chain, parse_partition, project_of, state};
use crate::call::Call;
use crate::error::AdminError;
use crate::pagination;

const SCOPE: &str = "overage_requests";
const MAX_REASON: usize = 500;

fn invalid(field: &str, code: DetailCode, message: &str) -> AdminError {
    AdminError::validation(vec![ConfigError::new(&DocPath::root().key(field), code, message)])
}

/// `amount` of a grant body, in the budget's units; it must be positive.
fn amount_of(body: &Map<String, Value>, unit: &BudgetUnit) -> Result<u128, AdminError> {
    let v = body
        .get("amount")
        .ok_or_else(|| invalid("amount", DetailCode::Required, "amount is required"))?;
    let n = match unit {
        BudgetUnit::Usd => v
            .as_str()
            .and_then(|s| Nusd::parse_usd(s).ok())
            .map(Nusd::units)
            .ok_or_else(|| {
                invalid(
                    "amount",
                    DetailCode::InvalidFormat,
                    "amount must be a USD decimal string",
                )
            })?,
        BudgetUnit::Tokens(_) => v
            .as_u64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            .map(u128::from)
            .ok_or_else(|| invalid("amount", DetailCode::InvalidFormat, "amount must be an integer"))?,
    };
    if n == 0 {
        return Err(invalid(
            "amount",
            DetailCode::OutOfRange,
            "amount must be more than zero",
        ));
    }
    Ok(n)
}

/// `expires_at` of a grant body: after now and no later than the end of the period.
fn expiry_of(
    body: &Map<String, Value>,
    now: UtcMicros,
    period_end: Option<UtcMicros>,
) -> Result<UtcMicros, AdminError> {
    let at = body
        .get("expires_at")
        .ok_or_else(|| invalid("expires_at", DetailCode::Required, "expires_at is required"))?
        .as_str()
        .and_then(|s| UtcMicros::parse_rfc3339(s).ok())
        .ok_or_else(|| {
            invalid(
                "expires_at",
                DetailCode::InvalidFormat,
                "expires_at must be an RFC 3339 time",
            )
        })?;
    if at <= now {
        return Err(invalid(
            "expires_at",
            DetailCode::OutOfRange,
            "expires_at must be in the future",
        ));
    }
    if period_end.is_some_and(|end| at > end) {
        return Err(invalid(
            "expires_at",
            DetailCode::OutOfRange,
            "expires_at must not be later than the end of the budget's period",
        ));
    }
    Ok(at)
}

fn reason_of(body: &Map<String, Value>, required: bool) -> Result<Option<String>, AdminError> {
    match body.get("reason") {
        None | Some(Value::Null) if required => {
            Err(invalid("reason", DetailCode::Required, "reason is required"))
        }
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.chars().count() <= MAX_REASON => Ok(Some(s.clone())),
        Some(_) => Err(invalid(
            "reason",
            DetailCode::InvalidFormat,
            "reason must be a string of at most 500 characters",
        )),
    }
}

fn counter<'p>(
    call: &'p Call,
    level: Level,
    owner: &str,
    name: &str,
    partition: &[(String, String)],
) -> Result<NamedBudget<'p>, AdminError> {
    named_budget(
        &call.snap.limits,
        level,
        owner,
        name,
        partition,
        project_of(call, level, owner).as_deref(),
    )
    .ok_or_else(AdminError::not_found)
}

/// `POST /runtime/budgets/{level}/{owner}/{name}:grant_overage`.
pub(crate) async fn grant(
    call: &Call,
    level: Level,
    owner: &str,
    name: &str,
    scopes: &[Scope],
) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&[])?;
    call.require("budgets:adjust", scopes)?;
    let body = call.json_body()?;
    let partition = body
        .get("partition")
        .and_then(Value::as_str)
        .map(parse_partition)
        .transpose()?
        .unwrap_or_default();
    let b = counter(call, level, owner, name, &partition)?;
    let now = call.rt.now();
    let period = b.budget.bounds(now);
    let amount = amount_of(&body, &b.budget.unit)?;
    let expires = expiry_of(&body, now, period.end)?;
    let reason = reason_of(&body, true)?.unwrap_or_default();
    let event = EventBudget {
        level,
        owner,
        name,
        identity: Some(b.identity),
    };
    let g = NewGrant {
        id: None,
        amount,
        expires,
        reason,
        granted_by: call.principal.id.clone(),
        request: None,
    };
    let made = call
        .rt
        .grant_overage(event, b.identity, g)
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    call.audit_with(
        "budgets.grant_overage",
        &format!("runtime/budgets/{}/{owner}/{name}", level.as_str()),
        Outcome::Success,
        StatusCode::OK,
        json!({"grant": made.id, "amount": amount_text(b.budget, amount), "expires_at": expires.to_rfc3339_seconds()}),
    )
    .await;
    let body = state(call, &b, &allocated(call)).await?;
    Ok(call.json(StatusCode::OK, &body))
}

/// `DELETE /runtime/budgets/{level}/{owner}/{name}/grants/{id}`.
pub(crate) async fn revoke(
    call: &Call,
    level: &str,
    owner: &str,
    name: &str,
    id: &str,
) -> Result<Response, AdminError> {
    let level = Level::parse(level).ok_or_else(AdminError::not_found)?;
    let scopes = chain(call, level, owner);
    if !call.principal.sees(&scopes) {
        return Err(AdminError::not_found());
    }
    if call.method != Method::DELETE {
        return Err(AdminError::method_not_allowed("DELETE"));
    }
    call.allow_params(&["partition"])?;
    call.require("budgets:adjust", &scopes)?;
    let partition = call
        .param("partition")
        .map(parse_partition)
        .transpose()?
        .unwrap_or_default();
    let b = counter(call, level, owner, name, &partition)?;
    let revoked = call
        .rt
        .revoke_overage_grant(b.identity, id)
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    if !revoked {
        return Err(AdminError::not_found());
    }
    call.audit_with(
        "budgets.revoke_grant",
        &format!("runtime/budgets/{}/{owner}/{name}/grants/{id}", level.as_str()),
        Outcome::Success,
        StatusCode::NO_CONTENT,
        json!({"grant": id}),
    )
    .await;
    Ok(call.no_content())
}

fn request_json(call: &Call, r: &OverageRow) -> Value {
    let level = Level::parse(&r.level);
    let decl = level.and_then(|l| declaration(&call.snap.limits, l, &r.owner, &r.name));
    let shown = |v: &str| match (decl, v.parse::<u128>()) {
        (Some((_, d)), Ok(n)) => amount_text(d, n),
        _ => v.to_owned(),
    };
    let time = |us: i64| UtcMicros(us).to_rfc3339_seconds();
    json!({
        "id": r.id,
        "level": r.level,
        "owner": r.owner,
        "name": r.name,
        "budget_owner": decl.and_then(|(_, d)| d.owner.clone()),
        "opened_at": time(r.opened_us),
        "spent": shown(&r.spent),
        "amount": shown(&r.amount),
        "state": r.state,
        "period_start": time(r.period_start_us),
        "expires_at": r.period_end_us.map(time),
        "decided_by": r.decided_by,
        "decided_at": r.decided_us.map(time),
        "reason": r.reason,
        "grant_id": r.grant,
    })
}

fn scopes_of(call: &Call, r: &OverageRow) -> Vec<Scope> {
    Level::parse(&r.level).map_or_else(|| vec![Scope::Gateway], |l| chain(call, l, &r.owner))
}

/// `GET /budgets/overage_requests`.
pub(crate) async fn list(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&["state", "limit", "cursor"])?;
    let (limit, after) = pagination::page_params(call, SCOPE)?;
    let state = call.param("state");
    if state.is_some_and(|s| !matches!(s, "open" | "approved" | "denied" | "expired")) {
        return Err(AdminError::invalid_parameter(
            "state must be open, approved, denied or expired",
        ));
    }
    call.rt.flush_budget_tallies().await;
    let rows = call
        .rt
        .store
        .expire_overages(call.rt.now().0)
        .await
        .and(call.rt.store.overages(state, 100_000).await)
        .map_err(|_| AdminError::store_unavailable())?;
    let visible: Vec<&OverageRow> = rows
        .iter()
        .filter(|r| {
            let scopes = scopes_of(call, r);
            call.principal.sees(&scopes) && call.require("policies:read", &scopes).is_ok()
        })
        .collect();
    let start = after.and_then(|a| a.parse::<usize>().ok()).unwrap_or(0);
    let page: Vec<Value> = visible
        .iter()
        .skip(start)
        .take(limit)
        .map(|r| request_json(call, r))
        .collect();
    let next = (start + page.len() < visible.len())
        .then(|| pagination::encode(&call.rt.cursor_key, SCOPE, &(start + page.len()).to_string()));
    Ok(call.json(StatusCode::OK, &json!({"items": page, "next_cursor": next})))
}

fn not_open() -> AdminError {
    AdminError::conflict("conflict", "The overage request is no longer open.", Vec::new())
}

/// `POST /budgets/overage_requests/{id}:approve` and `:deny`.
pub(crate) async fn decide(call: &Call, segment: &str) -> Result<Response, AdminError> {
    let (id, approve) = match segment.rsplit_once(':') {
        Some((id, "approve")) => (id, true),
        Some((id, "deny")) => (id, false),
        _ => return Err(AdminError::not_found()),
    };
    let row = call
        .rt
        .store
        .overage(id)
        .await
        .map_err(|_| AdminError::store_unavailable())?
        .ok_or_else(AdminError::not_found)?;
    let scopes = scopes_of(call, &row);
    if !call.principal.sees(&scopes) {
        return Err(AdminError::not_found());
    }
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&[])?;
    call.require("budgets:adjust", &scopes)?;
    let now = call.rt.now();
    if row.state != "open" || row.period_end_us.is_some_and(|e| e <= now.0) {
        return Err(not_open());
    }
    let body = call.json_body()?;
    let decision = if approve {
        let level = Level::parse(&row.level).ok_or_else(AdminError::not_found)?;
        let (_, decl) = declaration(&call.snap.limits, level, &row.owner, &row.name).ok_or_else(|| {
            AdminError::conflict(
                "conflict",
                "The budget of this request no longer exists.",
                Vec::new(),
            )
        })?;
        let end = row.period_end_us.map(UtcMicros);
        Decision::Approve {
            amount: amount_of(&body, &decl.unit)?,
            expires: expiry_of(&body, now, end)?,
            reason: reason_of(&body, false)?,
        }
    } else {
        Decision::Deny {
            reason: reason_of(&body, false)?,
        }
    };
    let decided = match call.rt.decide_overage(row, decision, &call.principal.id).await {
        Ok(r) => r,
        Err(DecideError::NotOpen) => {
            return Err(AdminError::conflict(
                "conflict",
                "The overage request is no longer open.",
                Vec::new(),
            ));
        }
        Err(DecideError::Store(_)) => return Err(AdminError::store_unavailable()),
    };
    let action = if approve {
        "budgets.approve_overage"
    } else {
        "budgets.deny_overage"
    };
    call.audit_with(
        action,
        &format!("budgets/overage_requests/{id}"),
        Outcome::Success,
        StatusCode::OK,
        json!({"grant": decided.grant}),
    )
    .await;
    Ok(call.json(StatusCode::OK, &request_json(call, &decided)))
}
