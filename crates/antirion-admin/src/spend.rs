//! The spend ledger and its reports: `GET /reports/spend/requests/{request_id}`,
//! `GET /reports/spend/ledger` and `GET /reports/spend/summary`, and the effective price of a
//! deployment, `GET /pricing/deployments/{name}`. Lines are visible to principals with
//! `analytics:read` in their tenancy scope.

use std::sync::Arc;

use antirion_core::UtcMicros;
use antirion_identity::rbac::Scope;
use antirion_limits::book::{GROUP_ATTRS, LedgerFilter, summary as summarise};
use antirion_limits::ledger::LedgerLine;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;
use crate::pagination;

const FILTERS: &[&str] = &[
    "organization",
    "team",
    "project",
    "key",
    "user",
    "end_user",
    "model",
    "deployment",
    "tag",
    "status",
];

fn chain(l: &LedgerLine) -> Vec<Scope> {
    let mut c = vec![Scope::Gateway];
    for (s, v) in [
        (Scope::Organization as fn(String) -> Scope, &l.organization),
        (Scope::Team, &l.team),
        (Scope::Project, &l.project),
    ] {
        if !v.is_empty() {
            c.push(s(v.clone()));
        }
    }
    c
}

fn readable(call: &Call, l: &LedgerLine) -> bool {
    let c = chain(l);
    call.principal.sees(&c) && call.require("analytics:read", &c).is_ok()
}

fn require_any(call: &Call) -> Result<(), AdminError> {
    if call
        .principal
        .bindings
        .iter()
        .any(|b| b.role.allows("analytics:read"))
    {
        return Ok(());
    }
    call.require("analytics:read", &[Scope::Gateway])
}

fn time_param(call: &Call, name: &str) -> Result<Option<UtcMicros>, AdminError> {
    call.param(name)
        .map(|v| {
            UtcMicros::parse_rfc3339(v)
                .map_err(|_| AdminError::invalid_parameter(format!("{name} must be an RFC 3339 time")))
        })
        .transpose()
}

/// One page of lines, by position in the filtered list.
fn page(call: &Call, scope: &str, lines: &[Arc<LedgerLine>]) -> Result<Response, AdminError> {
    let (limit, after) = pagination::page_params(call, scope)?;
    let start = after.and_then(|a| a.parse::<usize>().ok()).unwrap_or(0);
    let items: Vec<Value> = lines
        .iter()
        .skip(start)
        .take(limit)
        .map(|l| l.to_json())
        .collect();
    let next = (start + items.len() < lines.len())
        .then(|| pagination::encode(&call.rt.cursor_key, scope, &(start + items.len()).to_string()));
    Ok(call.json(StatusCode::OK, &json!({"items": items, "next_cursor": next})))
}

/// The ledger as a feed: the newest request first, a request's lines in their order.
fn newest_first(lines: &mut [Arc<LedgerLine>]) {
    lines.sort_by(|a, b| {
        b.admitted_at
            .0
            .cmp(&a.admitted_at.0)
            .then_with(|| b.request_id.cmp(&a.request_id))
            .then_with(|| a.line.cmp(&b.line))
    });
}

/// `GET /reports/spend/requests/{request_id}`.
pub(crate) async fn request(call: &Call, id: &str) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&["limit", "cursor"])?;
    require_any(call)?;
    let lines: Vec<Arc<LedgerLine>> = call
        .rt
        .spend_of_request(id)
        .await
        .into_iter()
        .filter(|l| readable(call, l))
        .collect();
    if lines.is_empty() {
        return Err(AdminError::not_found());
    }
    page(call, "spend_request", &lines)
}

fn filter(call: &Call) -> Result<LedgerFilter, AdminError> {
    let p = |k: &str| call.param(k).map(str::to_owned);
    Ok(LedgerFilter {
        from: time_param(call, "from")?,
        to: time_param(call, "to")?,
        organization: p("organization"),
        team: p("team"),
        project: p("project"),
        key: p("key"),
        user: p("user"),
        end_user: p("end_user"),
        model: p("model"),
        deployment: p("deployment"),
        tag: p("tag"),
        status: p("status"),
    })
}

/// `GET /reports/spend/ledger`.
pub(crate) async fn ledger(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    let mut allowed = vec!["limit", "cursor", "from", "to"];
    allowed.extend(FILTERS);
    call.allow_params(&allowed)?;
    require_any(call)?;
    let f = filter(call)?;
    let mut lines: Vec<Arc<LedgerLine>> = call
        .rt
        .spend_lines(&f)
        .await
        .into_iter()
        .filter(|l| readable(call, l))
        .collect();
    newest_first(&mut lines);
    page(call, "spend_ledger", &lines)
}

/// `GET /reports/spend/summary?group_by=<attrs>`.
pub(crate) async fn summary(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&["limit", "cursor", "from", "to", "group_by"])?;
    require_any(call)?;
    let group_by: Vec<String> = call
        .param("group_by")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(str::to_owned)
        .collect();
    if let Some(bad) = group_by.iter().find(|a| !GROUP_ATTRS.contains(&a.as_str())) {
        return Err(AdminError::invalid_parameter(format!(
            "group_by: unknown attribute {bad:?}"
        )));
    }
    let f = LedgerFilter {
        from: time_param(call, "from")?,
        to: time_param(call, "to")?,
        ..LedgerFilter::default()
    };
    let lines: Vec<Arc<LedgerLine>> = call
        .rt
        .spend_lines(&f)
        .await
        .into_iter()
        .filter(|l| readable(call, l))
        .collect();
    let rows = summarise(&lines, &group_by);
    let (limit, after) = pagination::page_params(call, "spend_summary")?;
    let start = after.and_then(|a| a.parse::<usize>().ok()).unwrap_or(0);
    let items: Vec<Value> = rows.iter().skip(start).take(limit).cloned().collect();
    let next = (start + items.len() < rows.len()).then(|| {
        pagination::encode(
            &call.rt.cursor_key,
            "spend_summary",
            &(start + items.len()).to_string(),
        )
    });
    Ok(call.json(StatusCode::OK, &json!({"items": items, "next_cursor": next})))
}

/// `GET /pricing/deployments/{name}`: the merged price and its version.
pub(crate) fn price(call: &Call, name: &str) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&[])?;
    call.require("pricing:read", &[Scope::Gateway])?;
    let d = call
        .snap
        .providers
        .deployments
        .get(name)
        .ok_or_else(AdminError::not_found)?;
    Ok(call.json(StatusCode::OK, &crate::pricing::effective(call, d)))
}
