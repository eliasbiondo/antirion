//! Safety events: `GET /guardrails/events` (newest first, filtered, scoped), `GET
//! /guardrails/events/{id}` and `:review`. An event is visible to principals with `logs:read` in its
//! scope; its spans and score need `logs:read_content` there; a label needs `safety:review`.

use antirion_identity::rbac::Scope;
use antirion_runtime::safety::event::event_json;
use antirion_store::{SafetyColumn, SafetyEventRow, SafetyFilter};
use axum::response::Response;
use http::{HeaderName, HeaderValue, Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;
use crate::{logquery, pagination};

const SCOPE: &str = "guardrails/events";
const REDACTED_HEADER: &str = "x-antirion-fields-redacted";
/// The most events a total counts exactly.
const TOTAL_CAP: u64 = 100_000;
const MAX_NOTE: usize = 2000;

pub(crate) const CATEGORIES: [&str; 19] = [
    "deny_pattern",
    "allow_pattern",
    "keyword",
    "pii",
    "secret",
    "hate",
    "harassment",
    "violence",
    "sexual",
    "self_harm",
    "illicit",
    "prompt_injection",
    "off_topic",
    "invalid_json",
    "schema_violation",
    "tool_not_allowed",
    "length",
    "uninspectable",
    "custom",
];
pub(crate) const SEVERITIES: [&str; 4] = ["critical", "high", "medium", "low"];
pub(crate) const ACTIONS: [&str; 5] = ["block", "redact", "flag", "divert", "error"];
pub(crate) const REVIEW_STATES: [&str; 4] = ["open", "reviewed", "false_positive", "escalated"];

/// The repeatable filters, their column and the values they accept (`None`: any).
const FILTERS: [(&str, SafetyColumn, Option<&[&str]>); 12] = [
    ("category", SafetyColumn::Category, Some(&CATEGORIES)),
    ("severity", SafetyColumn::Severity, Some(&SEVERITIES)),
    ("action", SafetyColumn::Action, Some(&ACTIONS)),
    ("guardrail", SafetyColumn::Guardrail, None),
    ("review_state", SafetyColumn::ReviewState, Some(&REVIEW_STATES)),
    ("fingerprint", SafetyColumn::Fingerprint, None),
    ("organization", SafetyColumn::Organization, None),
    ("team", SafetyColumn::Team, None),
    ("project", SafetyColumn::Project, None),
    ("key", SafetyColumn::Key, None),
    ("model", SafetyColumn::Model, None),
    ("provider", SafetyColumn::Provider, None),
];

/// Every selection parameter.
pub(crate) const SELECTION: [&str; 15] = [
    "from",
    "to",
    "category",
    "severity",
    "action",
    "guardrail",
    "review_state",
    "fingerprint",
    "organization",
    "team",
    "project",
    "key",
    "model",
    "provider",
    "q",
];

/// The scopes containing an event, broadest first.
pub(crate) fn chain(e: &SafetyEventRow) -> Vec<Scope> {
    let mut out = vec![Scope::Gateway];
    out.extend(e.organization.clone().map(Scope::Organization));
    out.extend(e.team.clone().map(Scope::Team));
    out.extend(e.project.clone().map(Scope::Project));
    out
}

fn readable(call: &Call, e: &SafetyEventRow) -> bool {
    let c = chain(e);
    call.principal.sees(&c) && call.require("logs:read", &c).is_ok()
}

fn reads_content(call: &Call, e: &SafetyEventRow) -> bool {
    call.require("logs:read_content", &chain(e)).is_ok()
}

/// The selection of `call`: time range, repeatable filters (OR within one, AND across), `q`
/// and the caller's scope.
pub(crate) fn selection(call: &Call) -> Result<SafetyFilter, AdminError> {
    let mut f = SafetyFilter {
        from_us: logquery::time(call, "from")?,
        to_us: logquery::time(call, "to")?,
        q: call.param("q").map(str::to_owned).filter(|q| !q.is_empty()),
        scopes: logquery::scopes(&call.principal, "logs:read"),
        ..SafetyFilter::default()
    };
    if let (Some(from), Some(to)) = (f.from_us, f.to_us)
        && from > to
    {
        return Err(AdminError::invalid_parameter("from must not be after to"));
    }
    for (param, column, accepted) in FILTERS {
        let values: Vec<String> = call
            .query
            .iter()
            .filter(|(k, _)| k == param)
            .map(|(_, v)| v.clone())
            .collect();
        if values.is_empty() {
            continue;
        }
        if let Some(ok) = accepted
            && let Some(bad) = values.iter().find(|v| !ok.contains(&v.as_str()))
        {
            return Err(AdminError::invalid_parameter(format!(
                "{param} {bad:?} is not one of {}",
                ok.join(", ")
            )));
        }
        f.any_of.push((column, values));
    }
    Ok(f)
}

fn shown(call: &Call, e: &SafetyEventRow, redacted: &mut bool) -> Value {
    let content = reads_content(call, e);
    *redacted |= !content;
    event_json(e, content)
}

fn with_redactions(call: &Call, body: &Value, redacted: bool) -> Response {
    let mut resp = call.json(StatusCode::OK, body);
    if redacted {
        resp.headers_mut().insert(
            HeaderName::from_static(REDACTED_HEADER),
            HeaderValue::from_static("spans,score"),
        );
    }
    resp
}

fn decode_after(s: &str) -> Option<(i64, String)> {
    let (t, id) = s.split_once('\u{1f}')?;
    Some((t.parse().ok()?, id.to_owned()))
}

async fn list(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    logquery::allow(call, &SELECTION, &["limit", "cursor", "include_total"])?;
    crate::logs::require_some(call, "logs:read")?;
    let (limit, after) = pagination::page_params(call, SCOPE)?;
    let after = after
        .map(|a| decode_after(&a).ok_or_else(|| AdminError::invalid_parameter("the cursor is not valid")))
        .transpose()?;
    let include_total = call.bool_param("include_total", false)?;
    let f = selection(call)?;
    call.rt.read_safety().await;
    let mut position = after;
    let mut items: Vec<SafetyEventRow> = Vec::new();
    let mut more = false;
    loop {
        let page = call
            .rt
            .store
            .list_safety_events(&f, position.clone(), limit + 1)
            .await
            .map_err(|_| AdminError::store_unavailable())?;
        let exhausted = page.len() <= limit;
        position = page.last().map(|e| (e.time_us, e.id.clone()));
        for e in page {
            if !readable(call, &e) {
                continue;
            }
            if items.len() == limit {
                more = true;
                break;
            }
            items.push(e);
        }
        if more || exhausted || position.is_none() {
            break;
        }
    }
    let next_cursor = more.then(|| items.last()).flatten().map(|e| {
        pagination::encode(
            &call.rt.cursor_key,
            SCOPE,
            &format!("{}\u{1f}{}", e.time_us, e.id),
        )
    });
    let mut redacted = false;
    let shown_items: Vec<Value> = items.iter().map(|e| shown(call, e, &mut redacted)).collect();
    let mut body = json!({"items": shown_items, "next_cursor": next_cursor});
    if include_total {
        let n = call
            .rt
            .store
            .count_safety_events(&f, TOTAL_CAP)
            .await
            .map_err(|_| AdminError::store_unavailable())?;
        body["total"] = json!(n.min(TOTAL_CAP));
    }
    Ok(with_redactions(call, &body, redacted))
}

/// An event the caller may read, else not found.
async fn visible(call: &Call, id: &str) -> Result<SafetyEventRow, AdminError> {
    call.rt.read_safety().await;
    let e = call
        .rt
        .store
        .safety_event(id)
        .await
        .map_err(|_| AdminError::store_unavailable())?
        .ok_or_else(AdminError::not_found)?;
    let c = chain(&e);
    if !call.principal.sees(&c) {
        return Err(AdminError::not_found());
    }
    call.require("logs:read", &c)
        .map_err(|e| e.with_resource(format!("{SCOPE}/{id}")))?;
    Ok(e)
}

async fn one(call: &Call, id: &str) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&[])?;
    let e = visible(call, id).await?;
    let mut redacted = false;
    let body = shown(call, &e, &mut redacted);
    Ok(with_redactions(call, &body, redacted))
}

/// The label of a review request: `{state, note?}`.
fn label(call: &Call) -> Result<(String, Option<String>), AdminError> {
    use antirion_config::{ConfigError, DetailCode, DocPath};
    let invalid = |field: &str, code: DetailCode, message: &str| {
        AdminError::validation(vec![ConfigError::new(&DocPath::root().key(field), code, message)])
    };
    let body = call.json_body()?;
    if let Some(extra) = body.keys().find(|k| !matches!(k.as_str(), "state" | "note")) {
        return Err(invalid(
            extra,
            DetailCode::UnknownField,
            "not a field of a review",
        ));
    }
    let state = match body.get("state") {
        None => return Err(invalid("state", DetailCode::Required, "the state is required")),
        Some(Value::String(s)) if REVIEW_STATES.contains(&s.as_str()) => s.clone(),
        Some(Value::String(_)) => {
            return Err(invalid(
                "state",
                DetailCode::InvalidEnum,
                "must be open, reviewed, false_positive or escalated",
            ));
        }
        Some(_) => return Err(invalid("state", DetailCode::TypeMismatch, "expected a string")),
    };
    let note = match body.get("note") {
        None | Some(Value::Null) => None,
        Some(Value::String(n)) if n.chars().count() <= MAX_NOTE => Some(n.clone()),
        Some(Value::String(_)) => {
            return Err(invalid("note", DetailCode::OutOfRange, "at most 2000 characters"));
        }
        Some(_) => return Err(invalid("note", DetailCode::TypeMismatch, "expected a string")),
    };
    Ok((state, note))
}

async fn review(call: &Call, id: &str) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&[])?;
    let e = visible(call, id).await?;
    let resource = format!("{SCOPE}/{id}");
    call.require("safety:review", &chain(&e))
        .map_err(|err| err.with_resource(resource.clone()))?;
    let (state, note) = label(call)?;
    let reviewer = call.principal.user.as_deref().unwrap_or(&call.principal.id);
    let done = call
        .rt
        .store
        .review_safety_event(id, &state, reviewer, call.rt.now().0, note.as_deref())
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    if !done {
        return Err(AdminError::not_found());
    }
    call.audit_with(
        "guardrails.event_reviewed",
        &resource,
        antirion_core::audit::Outcome::Success,
        StatusCode::OK,
        json!({"state": state, "previous_state": e.review_state, "note": note}),
    )
    .await;
    let after = visible(call, id).await?;
    let mut redacted = false;
    let body = shown(call, &after, &mut redacted);
    Ok(with_redactions(call, &body, redacted))
}

/// Routes `/guardrails/events` and `/guardrails/events/{id}[:review]`.
pub(crate) async fn handle(call: &Call, segment: Option<&str>) -> Result<Response, AdminError> {
    match segment {
        None => list(call).await,
        Some(s) => match s.strip_suffix(":review") {
            Some(id) => review(call, id).await,
            None => one(call, s).await,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_decode() {
        assert_eq!(decode_after("12\u{1f}abc"), Some((12, "abc".to_owned())));
        assert_eq!(decode_after("x\u{1f}abc"), None);
        assert_eq!(decode_after("12"), None);
    }

    #[test]
    fn the_category_filter_is_the_closed_list() {
        assert!(CATEGORIES.contains(&"prompt_injection") && !CATEGORIES.contains(&"error"));
        assert_eq!(FILTERS.len() + 3, SELECTION.len());
    }
}
