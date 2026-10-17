//! `GET /runtime/budgets/events`: the budget event feed, newest first, each event visible to who may
//! read its budget's policies. An `exhausted` event reports the requests its budget has refused so
//! far in that period.

use antirion_core::UtcMicros;
use antirion_limits::level::Level;
use antirion_store::{BudgetEventFilter, BudgetEventRow};
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::budgets::chain;
use crate::call::Call;
use crate::error::AdminError;
use crate::pagination;

const SCOPE: &str = "budget_events";
const KINDS: &[&str] = &[
    "threshold_crossed",
    "exhausted",
    "forecast_exceeded",
    "overage_granted",
    "overage_requested",
    "reset",
    "adjusted",
    "config_changed",
];
const BATCH: u64 = 500;

fn time_param(call: &Call, name: &str) -> Result<Option<i64>, AdminError> {
    call.param(name)
        .map(|v| {
            UtcMicros::parse_rfc3339(v)
                .map(|t| t.0)
                .map_err(|_| AdminError::invalid_parameter(format!("{name} must be an RFC 3339 time")))
        })
        .transpose()
}

async fn event_json(call: &Call, e: BudgetEventRow) -> Value {
    let mut detail: Value = serde_json::from_str(&e.detail).unwrap_or_else(|_| json!({}));
    if e.kind == "exhausted"
        && let (Some(identity), Some(start)) = (
            e.identity.as_deref(),
            detail
                .get("period_start")
                .and_then(Value::as_str)
                .and_then(|s| UtcMicros::parse_rfc3339(s).ok()),
        )
        && let Ok((refused, _)) = call.rt.store.tally(identity, start.0).await
        && let Some(m) = detail.as_object_mut()
    {
        m.insert("refused_requests".into(), Value::from(refused));
    }
    json!({
        "id": e.id,
        "time": UtcMicros(e.time_us).to_rfc3339_seconds(),
        "kind": e.kind,
        "level": e.level,
        "owner": e.owner,
        "name": e.name,
        "actor": e.actor,
        "detail": detail,
    })
}

pub(crate) async fn list(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&["level", "owner", "name", "from", "to", "kind", "limit", "cursor"])?;
    let (limit, after) = pagination::page_params(call, SCOPE)?;
    if call.param("level").is_some_and(|l| Level::parse(l).is_none()) {
        return Err(AdminError::invalid_parameter("unknown level"));
    }
    let kinds: Vec<String> = call
        .query
        .iter()
        .filter(|(k, _)| k == "kind")
        .flat_map(|(_, v)| v.split(','))
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .collect();
    if let Some(k) = kinds.iter().find(|k| !KINDS.contains(&k.as_str())) {
        return Err(AdminError::invalid_parameter(format!("unknown event kind {k:?}")));
    }
    let mut before = match after {
        Some(a) => {
            let (t, id) = a
                .split_once('|')
                .ok_or_else(|| AdminError::invalid_parameter("the cursor is not valid"))?;
            Some((
                t.parse::<i64>()
                    .map_err(|_| AdminError::invalid_parameter("the cursor is not valid"))?,
                id.to_owned(),
            ))
        }
        None => None,
    };
    call.rt.write_queued_budget_events().await;
    call.rt.flush_budget_tallies().await;
    let mut visible: Vec<BudgetEventRow> = Vec::new();
    let mut more = false;
    loop {
        let filter = BudgetEventFilter {
            level: call.param("level").map(str::to_owned),
            owner: call.param("owner").map(str::to_owned),
            name: call.param("name").map(str::to_owned),
            kinds: kinds.clone(),
            from_us: time_param(call, "from")?,
            to_us: time_param(call, "to")?,
            before: before.clone(),
            limit: BATCH,
        };
        let rows = call
            .rt
            .store
            .budget_events(&filter)
            .await
            .map_err(|_| AdminError::store_unavailable())?;
        let fetched = rows.len();
        before = rows.last().map(|r| (r.time_us, r.id.clone()));
        for r in rows {
            let scopes = Level::parse(&r.level).map_or_else(
                || vec![antirion_identity::rbac::Scope::Gateway],
                |l| chain(call, l, &r.owner),
            );
            if !call.principal.sees(&scopes) || call.require("policies:read", &scopes).is_err() {
                continue;
            }
            if visible.len() == limit {
                more = true;
                break;
            }
            visible.push(r);
        }
        if more || u64::try_from(fetched).unwrap_or(0) < BATCH {
            break;
        }
    }
    let next = more
        .then(|| visible.last().map(|r| format!("{}|{}", r.time_us, r.id)))
        .flatten()
        .map(|p| pagination::encode(&call.rt.cursor_key, SCOPE, &p));
    let mut items = Vec::with_capacity(visible.len());
    for e in visible {
        items.push(event_json(call, e).await);
    }
    Ok(call.json(StatusCode::OK, &json!({"items": items, "next_cursor": next})))
}
