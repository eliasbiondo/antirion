//! Delivery and status operations of alerting and audit: the delivery log and redelivery, the test
//! actions of webhooks, channels and audit sinks, webhook secret rotation, the runtime status of
//! webhooks, channels and audit sinks, service-level objectives, the availability report and
//! operator notifications.

use antirion_config::{ConfigError, DetailCode, DocPath, model};
use antirion_core::UtcMicros;
use antirion_core::audit::Outcome;
use antirion_identity::rbac::Scope;
use antirion_runtime::alerting::{self, TestOutcome};
use antirion_runtime::changes::Target;
use antirion_store::DeliveryQuery;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::Handled;
use crate::call::Call;
use crate::commit::{Proposal, api_write};
use crate::error::AdminError;
use crate::pagination;

const WEEK_US: i64 = 7 * 86_400_000_000;
/// The longest overlap a rotated webhook secret keeps.
const MAX_OVERLAP: std::time::Duration = std::time::Duration::from_secs(3600);
/// Buckets an availability report may hold.
const MAX_BUCKETS: i64 = 1440;

fn store_err(e: &antirion_store::StoreError) -> AdminError {
    tracing::warn!(error = %e, "delivery state could not be read or written");
    AdminError::store_unavailable()
}

fn only(call: &Call, method: Method, allow: &'static str, params: &[&str]) -> Result<(), AdminError> {
    if call.method != method {
        return Err(AdminError::method_not_allowed(allow));
    }
    call.allow_params(params)
}

fn body_error(path: &str, code: DetailCode, message: &str) -> AdminError {
    AdminError::validation(vec![ConfigError::new(&DocPath::root().key(path), code, message)])
}

/// A page of `items` (already ordered) as `{items, next_cursor}`.
fn page(call: &Call, scope: &str, items: &[Value]) -> Result<Response, AdminError> {
    let (limit, after) = pagination::page_params(call, scope)?;
    let start = after.and_then(|a| a.parse::<usize>().ok()).unwrap_or(0);
    let shown: Vec<&Value> = items.iter().skip(start).take(limit).collect();
    let next = (start + shown.len() < items.len())
        .then(|| pagination::encode(&call.rt.cursor_key, scope, &(start + shown.len()).to_string()));
    Ok(call.json(StatusCode::OK, &json!({"items": shown, "next_cursor": next})))
}

/// `GET /alerts/deliveries`: the last seven days of webhook and channel attempts, newest first.
pub(crate) async fn deliveries(call: &Call) -> Result<Response, AdminError> {
    only(
        call,
        Method::GET,
        "GET",
        &["limit", "cursor", "webhook", "event_type", "result"],
    )?;
    call.require("config:read", &[Scope::Gateway])?;
    let (limit, after) = pagination::page_params(call, "deliveries")?;
    let before = after
        .map(|a| {
            a.parse::<i64>()
                .map_err(|_| AdminError::invalid_parameter("the cursor is not valid"))
        })
        .transpose()?;
    let (rows, next) = call
        .rt
        .store
        .deliveries(DeliveryQuery {
            alerts_only: true,
            target: call.param("webhook").map(str::to_owned),
            event_type: call.param("event_type").map(str::to_owned),
            result: call.param("result").map(str::to_owned),
            since_us: call.rt.now().0 - WEEK_US,
            before,
            limit,
            ..DeliveryQuery::default()
        })
        .await
        .map_err(|e| store_err(&e))?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(_, d)| {
            json!({
                "event_id": d.event_id,
                "webhook": d.target,
                "target_kind": d.target_kind,
                "event_type": d.event_type,
                "attempt": d.attempt,
                "time": UtcMicros(d.time_us).to_rfc3339(),
                "status_code": d.status_code,
                "result": d.result,
            })
        })
        .collect();
    let next = next.map(|n| pagination::encode(&call.rt.cursor_key, "deliveries", &n.to_string()));
    Ok(call.json(StatusCode::OK, &json!({"items": items, "next_cursor": next})))
}

/// `POST /alerts/deliveries/{event_id}:redeliver`.
pub(crate) async fn redeliver(call: &Call, event_id: &str) -> Result<Response, AdminError> {
    only(call, Method::POST, "POST", &[])?;
    call.require("config:apply", &[Scope::Gateway])?;
    let found = alerting::redeliver(&call.rt, event_id)
        .await
        .map_err(|e| store_err(&e))?;
    if !found {
        return Err(AdminError::not_found());
    }
    call.audit(
        "alerts.redeliver",
        &format!("alerts/deliveries/{event_id}"),
        Outcome::Success,
        StatusCode::ACCEPTED,
    )
    .await;
    Ok(call.json(StatusCode::ACCEPTED, &json!({"event_id": event_id})))
}

fn tested(call: &Call, outcome: Option<TestOutcome>) -> Result<Response, AdminError> {
    let outcome = outcome.ok_or_else(AdminError::not_found)?;
    Ok(call.json(StatusCode::OK, &outcome.to_json()))
}

/// `POST /alerts/channels/{name}:test`.
pub(crate) async fn test_channel(call: &Call, name: &str) -> Result<Response, AdminError> {
    only(call, Method::POST, "POST", &[])?;
    call.require("alerts:write", &[Scope::Gateway])?;
    let outcome = alerting::test_channel(&call.rt, name).await;
    tested(call, outcome)
}

/// `POST /alerts/webhooks/{name}:test` and `:rotate_secret`.
pub(crate) async fn webhook_action(call: &Call, segment: &str) -> Result<Response, AdminError> {
    let Some((name, action)) = segment.split_once(':') else {
        return Err(AdminError::not_found());
    };
    only(call, Method::POST, "POST", &[])?;
    call.require("alerts:write", &[Scope::Gateway])?;
    match action {
        "test" => {
            let outcome = alerting::test_webhook(&call.rt, name).await;
            tested(call, outcome)
        }
        "rotate_secret" => rotate(call, name).await,
        _ => Err(AdminError::not_found()),
    }
}

async fn rotate(call: &Call, name: &str) -> Result<Response, AdminError> {
    let overlap = if call.body.is_empty() {
        std::time::Duration::ZERO
    } else {
        match call.json_body()?.get("overlap") {
            None | Some(Value::Null) => std::time::Duration::ZERO,
            Some(Value::String(s)) => antirion_config::scalar::parse_duration(s).ok_or_else(|| {
                body_error("overlap", DetailCode::InvalidFormat, "overlap must be a duration")
            })?,
            Some(_) => {
                return Err(body_error(
                    "overlap",
                    DetailCode::TypeMismatch,
                    "overlap must be a duration",
                ));
            }
        }
    };
    if overlap > MAX_OVERLAP {
        return Err(body_error(
            "overlap",
            DetailCode::OutOfRange,
            "overlap is at most 1h",
        ));
    }
    let Some(hook) = call.snap.alerts.iter().find(|h| h.name == name).cloned() else {
        return Err(AdminError::not_found());
    };
    let secret = alerting::generate_secret().map_err(|_| AdminError::internal())?;
    let resource = format!("settings/observability/alerts/webhooks/{name}");
    let written = secret.clone();
    let webhook = name.to_owned();
    let committed = api_write(
        call,
        "alerts.webhooks.rotate_secret",
        &resource,
        StatusCode::OK,
        |state| {
            let mut doc = state.doc.clone();
            let entry = doc
                .pointer_mut(&format!("/observability/alerts/webhooks/{webhook}"))
                .and_then(Value::as_object_mut)
                .ok_or_else(AdminError::not_found)?;
            entry.insert("signing_secret".into(), Value::String(written.clone()));
            Ok(Proposal {
                candidate: doc,
                written: vec![Target::Setting(
                    model::singleton("observability").map_or("observability", |s| s.name),
                )],
                credentials: Vec::new(),
                meta_edits: Vec::new(),
            })
        },
    )
    .await?;
    let until = call.rt.now().0 + i64::try_from(overlap.as_micros()).unwrap_or(i64::MAX);
    let previous = hook
        .signing_secret
        .filter(|_| !overlap.is_zero())
        .map(|s| (s, until));
    let valid_until = previous.as_ref().map(|(_, u)| UtcMicros(*u).to_rfc3339());
    alerting::keep_previous_secret(&call.rt, name, previous)
        .await
        .map_err(|e| store_err(&e))?;
    call.audit_read("alerts.webhooks.secret_revealed", &resource)
        .await;
    let resp = call.json(
        StatusCode::OK,
        &json!({"secret": secret, "previous_valid_until": valid_until}),
    );
    Ok(call.mutated(resp, committed.generation).await)
}

/// `GET /runtime/alerts/webhooks` and `/runtime/alerts/channels`.
pub(crate) async fn target_status(call: &Call, kind: &str) -> Result<Response, AdminError> {
    only(call, Method::GET, "GET", &["limit", "cursor"])?;
    call.require("alerts:read", &[Scope::Gateway])?;
    let items = alerting::status::targets(&call.rt, kind)
        .await
        .map_err(|e| store_err(&e))?;
    page(call, kind, &items)
}

/// `GET /runtime/audit/sinks` and `POST /runtime/audit/sinks/{name}:test`.
pub(crate) async fn audit_sinks(call: &Call, segment: Option<&str>) -> Result<Response, AdminError> {
    match segment {
        None => {
            only(call, Method::GET, "GET", &["limit", "cursor"])?;
            call.require("audit:read", &[Scope::Gateway])?;
            let items = alerting::sinks::status(&call.rt)
                .await
                .map_err(|e| store_err(&e))?;
            page(call, "audit_sinks", &items)
        }
        Some(segment) => {
            let Some((name, "test")) = segment.split_once(':') else {
                return Err(AdminError::not_found());
            };
            only(call, Method::POST, "POST", &[])?;
            call.require("security:write", &[Scope::Gateway])?;
            let outcome = alerting::sinks::test(&call.rt, name).await;
            tested(call, outcome)
        }
    }
}

/// `GET /runtime/slos`.
pub(crate) async fn slos(call: &Call) -> Result<Response, AdminError> {
    only(call, Method::GET, "GET", &["limit", "cursor"])?;
    call.require("analytics:read", &[Scope::Gateway])?;
    let items = alerting::slo::status(&call.rt).await;
    page(call, "slos", &items)
}

pub(crate) fn interval_us(v: &str) -> Option<i64> {
    let minutes = match v {
        "1m" => 1,
        "5m" => 5,
        "15m" => 15,
        "1h" | "hour" => 60,
        "6h" => 360,
        "1d" | "day" => 1440,
        _ => return None,
    };
    Some(minutes * 60_000_000)
}

/// `GET /reports/availability`.
pub(crate) async fn availability(call: &Call) -> Result<Response, AdminError> {
    only(
        call,
        Method::GET,
        "GET",
        &["from", "to", "interval", "group_by", "limit", "cursor"],
    )?;
    call.require("analytics:read", &[Scope::Gateway])?;
    let time = |name: &str| {
        call.param(name)
            .map(|v| {
                UtcMicros::parse_rfc3339(v)
                    .map(|t| t.0)
                    .map_err(|_| AdminError::invalid_parameter(format!("{name} must be an RFC 3339 time")))
            })
            .transpose()
    };
    let now = call.rt.now().0;
    let to = time("to")?.unwrap_or(now + 1);
    let from = time("from")?.unwrap_or(to - 86_400_000_000);
    if from >= to {
        return Err(AdminError::invalid_parameter("from must be before to"));
    }
    let group_by = call.param("group_by").unwrap_or("gateway");
    if !matches!(group_by, "gateway" | "provider" | "deployment" | "model") {
        return Err(AdminError::invalid_parameter(
            "group_by is gateway, provider, deployment or model",
        ));
    }
    let interval = call
        .param("interval")
        .map(|v| interval_us(v).ok_or_else(|| AdminError::invalid_parameter("unknown interval")))
        .transpose()?;
    if interval.is_some_and(|i| (to - from) / i > MAX_BUCKETS) {
        return Err(AdminError::invalid_parameter(
            "the range holds more than 1440 buckets of this interval",
        ));
    }
    let items = alerting::slo::availability(&call.rt, from, to, group_by, interval).await;
    page(call, "availability", &items)
}

/// The members of a team: users whose primary team it is or who hold a binding on it or one of its
/// projects.
fn team_members(call: &Call, team: &str) -> Vec<String> {
    let id = &call.snap.identity;
    let mut out: Vec<String> = id
        .users
        .iter()
        .filter(|(_, u)| u.enabled && u.human)
        .filter(|(_, u)| {
            u.primary_team.as_deref() == Some(team)
                || u.bindings.iter().any(|b| match &b.scope {
                    Scope::Team(t) => t == team,
                    Scope::Project(p) => id.projects.get(p).and_then(|p| p.parent.as_deref()) == Some(team),
                    _ => false,
                })
        })
        .map(|(n, _)| n.clone())
        .collect();
    out.sort();
    out
}

/// The scopes of a user's own bindings, for the recipient check.
fn user_chain(call: &Call, user: &str) -> Vec<Vec<Scope>> {
    let id = &call.snap.identity;
    id.users
        .get(user)
        .map(|u| {
            u.bindings
                .iter()
                .map(|b| match &b.scope {
                    Scope::Gateway => vec![Scope::Gateway],
                    Scope::Organization(o) => vec![Scope::Gateway, Scope::Organization(o.clone())],
                    Scope::Team(t) => id.team_scopes(t),
                    Scope::Project(p) => id.project_scopes(p),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `POST /notifications:send`: one mail per recipient, each within the caller's scope.
pub(crate) async fn send_notification(call: &Call) -> Result<Response, AdminError> {
    only(call, Method::POST, "POST", &[])?;
    let body = call.json_body()?;
    let subject = body
        .get("subject")
        .and_then(Value::as_str)
        .ok_or_else(|| body_error("subject", DetailCode::Required, "subject is required"))?;
    let message = body
        .get("message")
        .and_then(Value::as_str)
        .ok_or_else(|| body_error("message", DetailCode::Required, "message is required"))?;
    if subject.chars().count() > 200 || subject.is_empty() {
        return Err(body_error(
            "subject",
            DetailCode::OutOfRange,
            "subject has 1 to 200 characters",
        ));
    }
    if message.chars().count() > 5000 {
        return Err(body_error(
            "message",
            DetailCode::OutOfRange,
            "message has at most 5000 characters",
        ));
    }
    let to = body
        .get("to")
        .and_then(Value::as_object)
        .ok_or_else(|| body_error("to", DetailCode::Required, "to names users or a team"))?;
    let mut recipients: Vec<String> = Vec::new();
    if let Some(users) = to.get("users") {
        let Some(users) = users.as_array() else {
            return Err(body_error(
                "to",
                DetailCode::TypeMismatch,
                "to.users is a list of user names",
            ));
        };
        for u in users {
            let name = u.as_str().ok_or_else(|| {
                body_error("to", DetailCode::TypeMismatch, "to.users is a list of user names")
            })?;
            recipients.push(name.to_owned());
        }
    }
    let team = to.get("team").and_then(Value::as_str);
    let team_scopes = match team {
        Some(t) => {
            if !call.snap.identity.teams.contains_key(t) && t != model::DEFAULT_TENANT {
                return Err(AdminError::not_found());
            }
            let scopes = call.snap.identity.team_scopes(t);
            call.require("notifications:send", &scopes)?;
            recipients.extend(team_members(call, t));
            Some(scopes)
        }
        None => None,
    };
    if recipients.is_empty() && team.is_none() {
        return Err(body_error("to", DetailCode::Required, "to names users or a team"));
    }
    recipients.sort();
    recipients.dedup();
    for user in &recipients {
        let Some(u) = call.snap.identity.users.get(user).filter(|u| u.enabled) else {
            return Err(AdminError::not_found());
        };
        let in_scope = team_scopes
            .as_ref()
            .is_some_and(|s| call.principal.allows("notifications:send", s))
            || user_chain(call, user)
                .iter()
                .any(|chain| call.principal.allows("notifications:send", chain))
            || call.principal.allows("notifications:send", &[Scope::Gateway]);
        if !in_scope || u.email.is_none() {
            return Err(AdminError::forbidden("notifications:send"));
        }
    }
    if call.snap.identity.console.mail.is_none() {
        return Err(AdminError::invalid_parameter(
            "outgoing mail (smtp) is not enabled",
        ));
    }
    let link = body.get("link").and_then(Value::as_str);
    let mut text = message.to_owned();
    if let Some(l) = link {
        text.push_str(&format!("\n\n{l}\n"));
    }
    let mut sent = 0usize;
    for user in &recipients {
        if let Some(email) = call
            .snap
            .identity
            .users
            .get(user)
            .and_then(|u| u.email.as_deref())
            && call.rt.send_mail(email, subject, text.clone())
        {
            sent += 1;
        }
    }
    call.audit(
        "notifications.send",
        &team.map_or_else(|| "notifications".to_owned(), |t| format!("teams/{t}")),
        Outcome::Success,
        StatusCode::ACCEPTED,
    )
    .await;
    Ok(call.json(StatusCode::ACCEPTED, &json!({"recipients": sent})))
}

/// Whether a path is one of alerting's and audit delivery's operations.
pub(crate) fn routes(segs: &[&str]) -> bool {
    match segs {
        ["alerts"] | ["alerts", "deliveries" | "webhooks", ..] | ["notifications:send"] => true,
        ["alerts", "channels", action] => action.contains(':'),
        ["alerts", segment] => !matches!(*segment, "rules" | "channels"),
        ["reports", "alerts", _, _] | ["reports", "availability"] | ["runtime", "slos"] => true,
        ["runtime", "alerts", "webhooks" | "channels"] | ["runtime", "audit", "sinks", ..] => true,
        _ => false,
    }
}

/// The future of the operation `segs` names among those [`routes`] claims.
pub(crate) fn dispatch<'a>(call: &'a Call, segs: &'a [&'a str]) -> Handled<'a> {
    match segs {
        ["alerts"] => Box::pin(crate::alerts::list(call)),
        ["alerts", "deliveries"] => Box::pin(deliveries(call)),
        ["alerts", "deliveries", action] if action.ends_with(":redeliver") => {
            Box::pin(redeliver(call, action.trim_end_matches(":redeliver")))
        }
        ["alerts", "channels", action] if action.contains(':') => match action.split_once(':') {
            Some((name, "test")) => Box::pin(test_channel(call, name)),
            _ => crate::routes::done(Err(AdminError::not_found())),
        },
        ["alerts", "webhooks", action] => Box::pin(webhook_action(call, action)),
        ["alerts", segment] if !matches!(*segment, "rules" | "channels") => {
            Box::pin(crate::alerts::one(call, segment))
        }
        ["reports", "alerts", id, which] => Box::pin(crate::alerts::report(call, id, which)),
        ["reports", "availability"] => Box::pin(availability(call)),
        ["runtime", "alerts", "webhooks"] => Box::pin(target_status(call, "webhook")),
        ["runtime", "alerts", "channels"] => Box::pin(target_status(call, "channel")),
        ["runtime", "audit", "sinks"] => Box::pin(audit_sinks(call, None)),
        ["runtime", "audit", "sinks", segment] => Box::pin(audit_sinks(call, Some(segment))),
        ["runtime", "slos"] => Box::pin(slos(call)),
        ["notifications:send"] => Box::pin(send_notification(call)),
        _ => crate::routes::done(Err(AdminError::not_found())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervals_and_their_synonyms() {
        assert_eq!(interval_us("1h"), interval_us("hour"));
        assert_eq!(interval_us("1d"), Some(86_400_000_000));
        assert_eq!(interval_us("2h"), None);
    }
}
