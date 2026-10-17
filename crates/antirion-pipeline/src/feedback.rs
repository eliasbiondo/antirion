//! `POST /v1/feedback`: a value for a configured metric, attached to one of the caller's project's
//! request-log records. A record that is not visible yet is waited for up to the log visibility
//! bound, so feedback sent right after a response always finds its record; every refusal of a log id
//! comes at the end of that wait, so the answer never tells another project's records apart from
//! records that do not exist.

use std::time::Duration;

use antirion_core::{RequestId, Stage, UtcMicros};
use antirion_error::reason::hygiene;
use antirion_error::{GatewayError, Message};
use antirion_obs::Fam;
use antirion_obs::feedback::Refusal;
use antirion_runtime::identity::KeyRt;
use antirion_runtime::reqlog::FeedbackRow;
use http::header::CONTENT_TYPE;
use http::request::Parts;
use http::{HeaderValue, Response, StatusCode};
use hyper::body::Incoming;
use serde_json::{Value, json};

use crate::ctx::{Body, Ctx, full};

/// Reasons of the feedback API.
pub(crate) mod reasons {
    antirion_error::reasons! {
        UNKNOWN_FEEDBACK_METRIC = "unknown_feedback_metric" => InvalidRequest,
        INVALID_FEEDBACK_VALUE = "invalid_feedback_value" => InvalidRequest,
        LOG_NOT_FOUND = "log_not_found" => NotFound,
    }
}

/// How long a record may take to become visible after its response (the log visibility bound).
const VISIBILITY: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(100);
const MAX_BODY: u64 = 64 << 10;

fn invalid(reason: antirion_error::Reason, param: &'static str, message: &'static str) -> GatewayError {
    GatewayError::with_reason(reason, Stage::Parse, Message::Fixed(message)).param(param)
}

fn string_member<'a>(body: &'a Value, name: &'static str) -> Result<&'a str, GatewayError> {
    body.get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            invalid(
                crate::reasons::MISSING_REQUIRED_PARAMETER,
                name,
                "log_id and metric must be non-empty strings.",
            )
        })
}

/// Whether `record` belongs to `project` and is younger than `max_age`.
fn eligible(record: &Value, project: &str, now: UtcMicros, max_age: Duration) -> bool {
    let started = record
        .get("started_at")
        .and_then(Value::as_str)
        .and_then(|s| UtcMicros::parse_rfc3339(s).ok());
    record.get("project").and_then(Value::as_str) == Some(project)
        && started.is_some_and(|s| now.since(s) <= max_age)
}

/// The caller's record `log_id`, waited for until the visibility bound.
async fn record_of(cx: &Ctx, key: &KeyRt, log_id: &str) -> Result<Value, GatewayError> {
    let deadline = cx.started + VISIBILITY;
    let max_age = cx.snap.obs.feedback.max_age;
    loop {
        if let Ok(Some(r)) = cx.rt.stored_log(log_id).await
            && eligible(&r, &key.project, cx.rt.now(), max_age)
        {
            return Ok(r);
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return Err(GatewayError::with_reason(
                reasons::LOG_NOT_FOUND,
                Stage::Resolve,
                Message::Fixed("No request-log record of this project has that id."),
            )
            .param("log_id"));
        }
        tokio::time::sleep(POLL.min(deadline - now)).await;
    }
}

async fn accept(
    cx: &mut Ctx,
    parts: &Parts,
    incoming: Incoming,
    key: &KeyRt,
) -> Result<Response<Body>, GatewayError> {
    let raw = crate::parse::read(cx, parts, incoming, MAX_BODY.min(cx.snap.security.max_body)).await?;
    let body: Value = serde_json::from_slice(&raw).map_err(|_| {
        GatewayError::with_reason(
            hygiene::INVALID_JSON,
            Stage::Parse,
            Message::Fixed("The request body is not valid JSON."),
        )
    })?;
    let log_id = string_member(&body, "log_id")?.to_owned();
    let metric = string_member(&body, "metric")?.to_owned();
    let value = body.get("value").cloned().unwrap_or(Value::Null);
    let snap = std::sync::Arc::clone(&cx.snap);
    snap.obs.feedback.check(&metric, &value).map_err(|r| match r {
        Refusal::UnknownMetric => invalid(
            reasons::UNKNOWN_FEEDBACK_METRIC,
            "metric",
            "The metric is not configured.",
        ),
        Refusal::InvalidValue => invalid(
            reasons::INVALID_FEEDBACK_VALUE,
            "value",
            "The value does not fit the metric's type or range.",
        ),
    })?;
    let record = record_of(cx, key, &log_id).await?;
    let id = RequestId::generate().as_str().to_owned();
    cx.rt
        .add_feedback(FeedbackRow {
            id: id.clone(),
            log_id: log_id.clone(),
            metric: metric.clone(),
            value: value.to_string(),
            created_us: cx.rt.now().0,
        })
        .await
        .map_err(|_| {
            GatewayError::new(
                antirion_error::Code::DependencyUnavailable,
                Stage::Respond,
                Message::Fixed("The feedback could not be stored."),
            )
        })?;
    cx.rt.metrics.add(Fam::Feedback, &[&metric], 1.0);
    if let (Some(experiment), Some(arm)) = (
        record.pointer("/experiment/name").and_then(Value::as_str),
        record.pointer("/experiment/arm").and_then(Value::as_str),
    ) {
        cx.rt
            .bandit_feedback(&snap, experiment, arm, &metric, &value)
            .await;
    }
    let mut resp = Response::new(full(bytes::Bytes::from(
        serde_json::to_vec(&json!({"id": id, "log_id": log_id})).unwrap_or_default(),
    )));
    *resp.status_mut() = StatusCode::CREATED;
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    cx.finish(&mut resp, false);
    Ok(resp)
}

/// Serves `POST /v1/feedback` for an authenticated key.
pub(crate) async fn serve(cx: &mut Ctx, parts: &Parts, incoming: Incoming, key: &KeyRt) -> Response<Body> {
    match accept(cx, parts, incoming, key).await {
        Ok(r) => r,
        Err(e) => cx.error(&e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_of_other_projects_or_past_max_age_are_not_eligible() {
        let now = UtcMicros::parse_rfc3339("2026-10-13T12:00:00Z").unwrap();
        let rec = json!({"project": "p", "started_at": "2026-10-13T11:00:00Z"});
        assert!(eligible(&rec, "p", now, Duration::from_secs(7200)));
        assert!(!eligible(&rec, "p", now, Duration::from_secs(60)));
        let other = json!({"project": "q", "started_at": "2026-10-13T11:59:00Z"});
        assert!(!eligible(&other, "p", now, Duration::from_secs(7200)));
    }
}
