//! Alert instances: `GET /alerts` and `/alerts/{id}`, the actions (`:acknowledge`, `:resolve`,
//! `:snooze`, `:unsnooze`, `:note`, `:escalate`), and the reports around an instance
//! (`/reports/alerts/{id}/signal` and `/similar`). An instance is visible to principals holding
//! `alerts:read` on its scope; others get 404.

use std::collections::BTreeMap;

use antirion_config::{ConfigError, DetailCode, DocPath};
use antirion_core::UtcMicros;
use antirion_core::audit::Outcome;
use antirion_runtime::alerting::instance::{Instance, Stored};
use antirion_runtime::alerting::{self, Action, ActionError};
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;
use crate::pagination;

const MINUTE_US: i64 = 60_000_000;
/// The signal report covers 20 minutes on each side of the firing.
const SIGNAL_SPAN_US: i64 = 20 * MINUTE_US;
const SIMILAR_US: i64 = 90 * 86_400_000_000;
/// The longest snooze.
const MAX_SNOOZE: std::time::Duration = std::time::Duration::from_secs(30 * 86_400);

fn store_err(e: &antirion_store::StoreError) -> AdminError {
    tracing::warn!(error = %e, "alert instances could not be read or written");
    AdminError::store_unavailable()
}

fn visible(call: &Call, permission: &str, i: &Instance) -> bool {
    call.principal
        .allows(permission, &alerting::chain(&call.snap.identity, &i.scope))
}

fn values<'a>(call: &'a Call, name: &str) -> Vec<&'a str> {
    call.query
        .iter()
        .filter(|(k, _)| k == name)
        .flat_map(|(_, v)| v.split(','))
        .filter(|v| !v.is_empty())
        .collect()
}

fn time(call: &Call, name: &str) -> Result<Option<i64>, AdminError> {
    call.param(name)
        .map(|v| {
            UtcMicros::parse_rfc3339(v)
                .map(|t| t.0)
                .map_err(|_| AdminError::invalid_parameter(format!("{name} must be an RFC 3339 time")))
        })
        .transpose()
}

fn fired_us(i: &Instance) -> i64 {
    UtcMicros::parse_rfc3339(&i.fired_at).map_or(0, |t| t.0)
}

fn matches_q(i: &Instance, q: &str) -> bool {
    let q = q.to_lowercase();
    [&i.id, &i.rule, &i.fingerprint, &i.summary, &i.category]
        .iter()
        .any(|f| f.to_lowercase().contains(&q))
        || i.scope.values().any(|v| v.to_lowercase().contains(&q))
}

/// A page of instances, newest first, as `{items, next_cursor}`.
fn page(call: &Call, scope: &str, items: Vec<Instance>) -> Result<Response, AdminError> {
    let (limit, after) = pagination::page_params(call, scope)?;
    let start = after.and_then(|a| a.parse::<usize>().ok()).unwrap_or(0);
    let shown: Vec<&Instance> = items.iter().skip(start).take(limit).collect();
    let next = (start + shown.len() < items.len())
        .then(|| pagination::encode(&call.rt.cursor_key, scope, &(start + shown.len()).to_string()));
    Ok(call.json(StatusCode::OK, &json!({"items": shown, "next_cursor": next})))
}

/// `GET /alerts`.
pub(crate) async fn list(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&[
        "limit", "cursor", "from", "to", "status", "severity", "category", "rule", "q", "scope",
    ])?;
    if !call
        .principal
        .bindings
        .iter()
        .any(|b| b.role.allows("alerts:read"))
    {
        return Err(AdminError::forbidden("alerts:read"));
    }
    let statuses = values(call, "status");
    if let Some(bad) = statuses
        .iter()
        .find(|s| !matches!(**s, "firing" | "acknowledged" | "resolved"))
    {
        return Err(AdminError::invalid_parameter(format!("unknown status {bad:?}")));
    }
    let severities = values(call, "severity");
    if let Some(bad) = severities.iter().find(|s| alerting::Severity::parse(s).is_none()) {
        return Err(AdminError::invalid_parameter(format!("unknown severity {bad:?}")));
    }
    let (categories, rules) = (values(call, "category"), values(call, "rule"));
    let mut scopes = Vec::new();
    for s in values(call, "scope") {
        let (dim, value) = s
            .split_once(':')
            .ok_or_else(|| AdminError::invalid_parameter("scope is <dimension>:<value>"))?;
        scopes.push((dim, value));
    }
    let (from, to) = (time(call, "from")?, time(call, "to")?);
    let all = alerting::instances(&call.rt, from)
        .await
        .map_err(|e| store_err(&e))?;
    let items: Vec<Instance> = all
        .into_iter()
        .map(|s| s.instance)
        .filter(|i| visible(call, "alerts:read", i))
        .filter(|i| statuses.is_empty() || statuses.contains(&i.status.as_str()))
        .filter(|i| severities.is_empty() || severities.contains(&i.severity.as_str()))
        .filter(|i| categories.is_empty() || categories.contains(&i.category.as_str()))
        .filter(|i| rules.is_empty() || rules.contains(&i.rule.as_str()))
        .filter(|i| {
            scopes
                .iter()
                .all(|(d, v)| i.scope.get(*d).is_some_and(|x| x == v))
        })
        .filter(|i| to.is_none_or(|t| fired_us(i) < t))
        .filter(|i| call.param("q").is_none_or(|q| matches_q(i, q)))
        .collect();
    page(call, "alerts", items)
}

async fn instance(call: &Call, id: &str, permission: &str) -> Result<Stored, AdminError> {
    let stored = alerting::load(&call.rt, id)
        .await
        .map_err(|e| store_err(&e))?
        .filter(|s| visible(call, "alerts:read", &s.instance))
        .ok_or_else(AdminError::not_found)?;
    if !visible(call, permission, &stored.instance) {
        return Err(AdminError::forbidden(permission));
    }
    Ok(stored)
}

fn body_error(path: &str, code: DetailCode, message: &str) -> AdminError {
    AdminError::validation(vec![ConfigError::new(&DocPath::root().key(path), code, message)])
}

fn action_of(call: &Call, name: &str) -> Result<Action, AdminError> {
    Ok(match name {
        "acknowledge" => Action::Acknowledge,
        "resolve" => Action::Resolve,
        "unsnooze" => Action::Unsnooze,
        "escalate" => Action::Escalate,
        "snooze" => {
            let body = call.json_body()?;
            let d = body
                .get("duration")
                .and_then(Value::as_str)
                .ok_or_else(|| body_error("duration", DetailCode::Required, "duration is required"))?;
            let d = antirion_config::scalar::parse_duration(d).ok_or_else(|| {
                body_error(
                    "duration",
                    DetailCode::InvalidFormat,
                    "duration must be a duration",
                )
            })?;
            if d.is_zero() || d > MAX_SNOOZE {
                return Err(body_error(
                    "duration",
                    DetailCode::OutOfRange,
                    "a snooze lasts more than 0s and at most 30d",
                ));
            }
            Action::Snooze(d)
        }
        "note" => {
            let body = call.json_body()?;
            let text = body
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| body_error("text", DetailCode::Required, "text is required"))?;
            let n = text.chars().count();
            if n == 0 || n > 2000 {
                return Err(body_error(
                    "text",
                    DetailCode::OutOfRange,
                    "a note has 1 to 2000 characters",
                ));
            }
            Action::Note(text.to_owned())
        }
        _ => return Err(AdminError::not_found()),
    })
}

/// `GET /alerts/{id}` and `POST /alerts/{id}:<action>`.
pub(crate) async fn one(call: &Call, segment: &str) -> Result<Response, AdminError> {
    let Some((id, action)) = segment.split_once(':') else {
        if call.method != Method::GET {
            return Err(AdminError::method_not_allowed("GET"));
        }
        call.allow_params(&[])?;
        let stored = instance(call, segment, "alerts:read").await?;
        if stored.instance.severity == "critical" {
            call.audit_read("alerts.read", &format!("alerts/{segment}")).await;
        }
        return Ok(call.json(StatusCode::OK, &json!(stored.instance)));
    };
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&[])?;
    let action = action_of(call, action)?;
    let stored = instance(call, id, "alerts:write").await?;
    let resource = format!("alerts/{id}");
    let audit_action = format!("alerts.{}", segment.rsplit(':').next().unwrap_or_default());
    if action == Action::Resolve && stored.instance.status == "resolved" {
        return Ok(call.json(StatusCode::OK, &json!(stored.instance)));
    }
    let actor = call
        .principal
        .user
        .clone()
        .unwrap_or_else(|| call.principal.id.clone());
    match alerting::act(&call.rt, id, action, &actor).await {
        Ok(i) => {
            call.audit(&audit_action, &resource, Outcome::Success, StatusCode::OK)
                .await;
            Ok(call.json(StatusCode::OK, &json!(i)))
        }
        Err(ActionError::NotFound) => Err(AdminError::not_found()),
        Err(ActionError::Conflict(message)) => {
            call.audit(&audit_action, &resource, Outcome::Failed, StatusCode::BAD_REQUEST)
                .await;
            Err(AdminError::invalid_parameter(message))
        }
        Err(ActionError::Store(e)) => Err(store_err(&e)),
    }
}

/// `GET /reports/alerts/{id}/signal` and `/similar`.
pub(crate) async fn report(call: &Call, id: &str, which: &str) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    match which {
        "signal" => {
            call.allow_params(&[])?;
            let stored = instance(call, id, "alerts:read").await?;
            let fired = fired_us(&stored.instance);
            let rule = call.snap.alerting.rule(&stored.instance.rule).cloned();
            let scope = rule
                .map(|r| r.scope)
                .unwrap_or_else(|| stored.instance.scope.clone());
            let scope = record_scope(&scope);
            let items = signal(call, &scope, fired - SIGNAL_SPAN_US, fired + SIGNAL_SPAN_US).await;
            Ok(call.json(StatusCode::OK, &json!({"items": items, "next_cursor": null})))
        }
        "similar" => {
            call.allow_params(&["limit", "cursor"])?;
            let stored = instance(call, id, "alerts:read").await?;
            let since = fired_us(&stored.instance) - SIMILAR_US;
            let all = alerting::similar(&call.rt, &stored.instance.fingerprint, since)
                .await
                .map_err(|e| store_err(&e))?;
            let items: Vec<Instance> = all
                .into_iter()
                .map(|s| s.instance)
                .filter(|i| i.id != stored.instance.id)
                .collect();
            page(call, "alerts_similar", items)
        }
        _ => Err(AdminError::not_found()),
    }
}

/// The part of an instance's scope that request records carry: an anomaly's id and signal name
/// are not record fields, and a budget's scope is its owner (`budget: team/search/monthly` reads as
/// `team: search`).
fn record_scope(scope: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    const FIELDS: [&str; 8] = [
        "provider",
        "deployment",
        "model",
        "route",
        "organization",
        "team",
        "project",
        "key",
    ];
    let mut out: BTreeMap<String, String> = scope
        .iter()
        .filter(|(k, _)| FIELDS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if let Some((level, rest)) = scope.get("budget").and_then(|b| b.split_once('/'))
        && let Some((owner, _)) = rest.split_once('/')
        && FIELDS.contains(&level)
    {
        out.entry(level.to_owned()).or_insert_with(|| owner.to_owned());
    }
    out
}

/// One-minute buckets of the instance's scope between `from` and `to`, in the usage report's shape.
async fn signal(call: &Call, scope: &BTreeMap<String, String>, from: i64, to: i64) -> Vec<Value> {
    use alerting::metrics::{in_scope, percentile, started_us};
    let first = from - from.rem_euclid(MINUTE_US);
    let end = first + (to - from);
    let records = alerting::slo::records_between(&call.rt, first, end).await;
    let mut buckets: BTreeMap<i64, Vec<&Value>> = BTreeMap::new();
    let mut start = first;
    while start < end {
        buckets.insert(start, Vec::new());
        start += MINUTE_US;
    }
    for r in &records {
        if !in_scope(r, scope) {
            continue;
        }
        if let Some(t) = started_us(r)
            && let Some(b) = buckets.get_mut(&(t - t.rem_euclid(MINUTE_US)))
        {
            b.push(r.as_ref());
        }
    }
    buckets
        .into_iter()
        .map(|(start, rs)| {
            let mut lat: Vec<f64> = rs
                .iter()
                .filter_map(|r| r.pointer("/timing/total_ms").and_then(Value::as_f64))
                .collect();
            let mut ttft: Vec<f64> = rs
                .iter()
                .filter_map(|r| r.pointer("/timing/ttft_ms").and_then(Value::as_f64))
                .collect();
            let cost: f64 = rs
                .iter()
                .filter_map(|r| r.pointer("/cost/total").and_then(Value::as_str))
                .filter_map(|s| s.parse::<f64>().ok())
                .sum();
            let errors = rs
                .iter()
                .filter(|r| {
                    r.get("status_code")
                        .and_then(Value::as_u64)
                        .is_some_and(|s| s >= 500)
                })
                .count();
            let tokens = |p: &str| -> u64 {
                rs.iter()
                    .filter_map(|r| r.pointer(p).and_then(Value::as_u64))
                    .sum()
            };
            let pcts = |v: &mut Vec<f64>| {
                json!({"p50": percentile(v, 50.0), "p90": percentile(v, 90.0),
                       "p95": percentile(v, 95.0), "p99": percentile(v, 99.0)})
            };
            json!({
                "start": UtcMicros(start).to_rfc3339(),
                "group": {},
                "requests": rs.len(),
                "errors": errors,
                "input": tokens("/usage/input"),
                "output": tokens("/usage/output"),
                "cost_usd": format!("{cost:.9}").trim_end_matches('0').trim_end_matches('.').to_owned(),
                "latency_ms": pcts(&mut lat),
                "ttft_ms": pcts(&mut ttft),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::record_scope;

    #[test]
    fn a_signal_reads_the_scope_records_carry() {
        let scope = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect()
        };
        assert_eq!(
            record_scope(&scope(&[
                ("key", "copilot-prod"),
                ("anomaly", "01a1"),
                ("signal", "user_agent")
            ])),
            scope(&[("key", "copilot-prod")])
        );
        assert_eq!(
            record_scope(&scope(&[("budget", "project/zendesk-assistant/monthly")])),
            scope(&[("project", "zendesk-assistant")])
        );
        assert!(record_scope(&scope(&[("budget", "gateway/-/monthly")])).is_empty());
    }
}
