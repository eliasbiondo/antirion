//! `GET /antirion/v1/limits`: the caller's own limits and budgets with their current state, read
//! without reserving anything. Only the counters the caller's requests use are shown, never another
//! partition's, and never partition values; levels above the key's project need
//! `limits:read_inherited`.

use antirion_core::Stage;
use antirion_error::{GatewayError, Message};
use antirion_identity::KeyPermission;
use antirion_limits::applicable::{RequestAttrs, standing};
use antirion_limits::counter::Rule;
use antirion_limits::level::Level;
use antirion_limits::money::Nusd;
use antirion_limits::plan::{PlannedHold, Unit, reading};
use antirion_limits::store::CounterView;
use antirion_runtime::identity::KeyRt;
use http::header::CONTENT_TYPE;
use http::{HeaderValue, Response};
use serde_json::{Value, json};

use crate::ctx::{Body, Ctx, full};

fn levels(key: &KeyRt) -> Vec<Level> {
    if key.permissions.has(KeyPermission::LimitsReadInherited) {
        vec![
            Level::Gateway,
            Level::Organization,
            Level::Team,
            Level::Project,
            Level::Key,
        ]
    } else {
        vec![Level::Project, Level::Key]
    }
}

fn limit_json(h: &PlannedHold, v: &CounterView, now: antirion_core::UtcMicros) -> Value {
    let (quota, per) = match &h.hold.rule {
        Rule::Gcra(g) => (u128::from(g.limit), Some(g.per_us.div_ceil(1_000_000))),
        Rule::Window(w) => (u128::from(w.limit), Some(w.per_us.div_ceil(1_000_000))),
        Rule::Concurrency { limit } => (u128::from(*limit), None),
        Rule::Budget { amount, .. } => (*amount, None),
        Rule::Rolling(w) => (u128::from(w.limit), None),
    };
    let reset_at = per.filter(|_| quota > 0 && v.full_in_us > 0).map(|_| {
        now.saturating_add(std::time::Duration::from_micros(v.full_in_us))
            .to_rfc3339_seconds()
    });
    json!({
        "level": h.info.level.as_str(),
        "owner": h.info.owner,
        "name": h.info.name,
        "unit": h.info.unit.name(),
        "limit": quota,
        "per_s": per,
        "remaining": v.remaining.min(quota),
        "reset_at": reset_at,
        "enforced": h.hold.mode == antirion_limits::counter::HoldMode::Enforce,
    })
}

fn budget_json(h: &PlannedHold, v: &CounterView, now: antirion_core::UtcMicros) -> Value {
    let (amount, period) = match &h.hold.rule {
        Rule::Budget { amount, period } => (amount, *period),
        Rule::Rolling(w) => match h.hold.rule.budget_period(now) {
            Some(p) => (&u128::from(w.limit), p),
            None => return Value::Null,
        },
        _ => return Value::Null,
    };
    let usd = h.info.unit == Unit::Usd;
    let text = |n: u128| {
        if usd {
            Nusd::from_units(n).to_usd_string()
        } else {
            n.to_string()
        }
    };
    json!({
        "level": h.info.level.as_str(),
        "owner": h.info.owner,
        "name": h.info.name,
        "unit": if usd { "usd" } else { "tokens" },
        "mode": if h.hold.mode == antirion_limits::counter::HoldMode::Enforce { "hard" } else { "soft" },
        "amount": text(*amount),
        "spent": text(v.spent),
        "reserved": text(v.reserved),
        "remaining": text(amount.saturating_sub(v.spent.saturating_add(v.reserved))),
        "period_start": period.start.to_rfc3339_seconds(),
        "resets_at": period.end.map(antirion_core::UtcMicros::to_rfc3339_seconds),
    })
}

/// The caller's limits and budgets: `{"limits": […], "budgets": […]}`.
pub(crate) async fn limits(cx: &Ctx, key: &KeyRt, end_user: Option<&str>) -> Response<Body> {
    let snap = &cx.snap;
    let tags: Vec<String> = snap
        .identity
        .chain(key)
        .iter()
        .flatten()
        .flat_map(|t| t.tags.iter().cloned())
        .chain(key.tags.iter().cloned())
        .collect();
    let attrs = RequestAttrs {
        organization: &key.organization,
        team: &key.team,
        project: &key.project,
        key: &key.name,
        user: key.owner.as_deref(),
        end_user,
        endpoint: "models",
        tags: &tags,
        ip: cx.client_ip,
        ..RequestAttrs::default()
    };
    let now = cx.rt.now();
    let holds: Vec<PlannedHold> = standing(&snap.limits, &attrs, &levels(key))
        .iter()
        .map(|a| reading(a, now))
        .collect();
    let views = match cx.rt.accounts.read(&holds).await {
        Ok(v) => v,
        Err(_) => {
            return cx.error(&GatewayError::with_reason(
                antirion_limits::reasons::LIMITS_UNAVAILABLE,
                Stage::Respond,
                Message::Fixed("Limits cannot be read right now."),
            ));
        }
    };
    let mut limits = Vec::new();
    let mut budgets = Vec::new();
    for (h, v) in holds.iter().zip(&views) {
        if h.info.unit.is_budget() {
            budgets.push(budget_json(h, v, now));
        } else {
            limits.push(limit_json(h, v, now));
        }
    }
    let body = json!({"limits": limits, "budgets": budgets});
    let mut resp = Response::new(full(bytes::Bytes::from(
        serde_json::to_vec(&body).unwrap_or_default(),
    )));
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    cx.finish(&mut resp, false);
    resp
}
