//! An upstream error answer: classified by provider code and status, turned into the client's
//! error and the attempt's failure.

use std::time::Duration;

use antirion_egress::InboundResponse;
use antirion_egress::headers::upstream_request_id;
use antirion_error::reason::hygiene;
use antirion_error::{Code, Message};
use antirion_resilience::classify::{Answer, answer};
use antirion_resilience::{Class, Failure, TimerKind};
use http::HeaderName;
use http::header::CONTENT_TYPE;
use serde_json::Value;

use crate::apis;
use crate::cell::Cell;
use crate::failure;
use crate::upstream_error::extract;

use super::{Attempt, MAX_ERROR_BODY, collect, scrub_for};

fn status_message(status: u16) -> String {
    match status {
        100..=199 | 204 | 205 | 304 => format!("upstream status {status} has no body"),
        300..=399 => format!("upstream status {status} is a redirect, which the gateway does not follow"),
        _ => format!("upstream status {status} rejects the gateway's framing"),
    }
}

/// An upstream error answer: classified by its provider code, then its status; the client's
/// error carries the upstream's message where it is known. `generation` is false for a request on
/// a provider-scoped resource.
pub(crate) async fn error_response(
    a: &Attempt<'_>,
    up: InboundResponse,
    by_status: Class,
    generation: bool,
) -> Failure {
    let status = up.status.as_u16();
    if by_status == Class::MalformedResponse && !(400..=499).contains(&status) {
        let reason = if (300..=399).contains(&status) && status != 304 {
            hygiene::REDIRECT_NOT_FOLLOWED
        } else {
            hygiene::BODYLESS_STATUS
        };
        return a.bad(reason, status, status_message(status));
    }
    let ct = up
        .headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let InboundResponse { headers, body, .. } = up;
    let raw = tokio::time::timeout(Duration::from_secs(10), collect(body, MAX_ERROR_BODY))
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    let parsed: Option<Value> = serde_json::from_slice(&raw).ok();
    if let Some(v) = &parsed
        && let Some(u) = v.get("usage").filter(|u| u.is_object())
    {
        let reported = apis::usage_of(v);
        if !reported.is_empty() {
            a.cx.log
                .update(|r| r.pending_error_usage = Some((reported, u.clone())));
        }
    }
    let scrub = scrub_for(a);
    if a.cx.snap.obs.request_logs.log_upstream_error_bodies {
        let max = a.cx.snap.obs.request_logs.upstream_error_body_max;
        let text = scrub.apply(&String::from_utf8_lossy(&raw));
        let kept = antirion_obs::truncate_utf8(&text, max).to_owned();
        a.cx.log.update(|r| r.pending_error_body = Some(kept));
    }
    let mut ex = extract(status, ct.as_deref(), &raw, &scrub);
    if ex.kind.is_none()
        && let Some(t) = headers.get("x-amzn-errortype").and_then(|v| v.to_str().ok())
    {
        ex.kind = t.split(':').next().map(str::to_owned);
    }
    let mut class = answer(&Answer {
        status,
        headers: &headers,
        body: parsed.as_ref(),
        generation,
    });
    if let Some(c) = a.cx.snap.health.status_overrides.get(&status) {
        class = *c;
    }
    let refused_own_key = a.byok.is_some() && matches!(status, 401 | 403);
    if refused_own_key {
        class = Class::CallerError;
    }
    let quota = [ex.code.as_deref(), ex.kind.as_deref()].contains(&Some("insufficient_quota"));
    if quota && class != Class::CredentialRejected && !refused_own_key {
        class = Class::QuotaExhausted;
    }
    let unrecognised = ex.fallback.ends_with("unrecognised error body");
    let upstream_message = ex.message.clone().unwrap_or_else(|| ex.fallback.clone());
    let (code, reason, message) = match class {
        Class::CredentialRejected => (
            Code::UpstreamAuthFailed,
            Some(hygiene::UPSTREAM_CREDENTIAL_REJECTED),
            match &ex.message {
                Some(m) => Message::built(m.clone()),
                None if !unrecognised => {
                    Message::Fixed("The provider rejected the gateway's credential for this deployment.")
                }
                None => Message::built(upstream_message),
            },
        ),
        Class::Timeout => (
            Code::UpstreamTimeout,
            Some(antirion_resilience::timeout_reason(TimerKind::Upstream)),
            Message::built(format!(
                "upstream timeout: upstream reported {status}: {upstream_message}"
            )),
        ),
        Class::MalformedResponse => (
            Code::UpstreamBadResponse,
            Some(hygiene::BODYLESS_STATUS),
            Message::built(status_message(status)),
        ),
        c => (c.final_code(), None, Message::built(upstream_message)),
    };
    let reason = if refused_own_key {
        Some(crate::reasons::BYOK_CREDENTIAL_REFUSED)
    } else {
        reason
    };
    let reason = reason.or(match class {
        Class::RateLimited => Some(hygiene::RATE_LIMIT_EXCEEDED),
        Class::Overloaded => Some(hygiene::UPSTREAM_OVERLOADED),
        Class::ServerError | Class::NotImplemented | Class::ModelUnavailable => {
            Some(hygiene::UPSTREAM_SERVER_ERROR)
        }
        Class::RefusedStream => Some(hygiene::CONNECTION_FAILED),
        _ if code == Code::UpstreamRejected => crate::upstream_error::normalised_reason(&ex),
        _ => None,
    });
    let mut f = a.fail(class, code, reason, Some(status), message);
    if let Some(u) = f.error.upstream.as_mut()
        && code != Code::UpstreamAuthFailed
        && (ex.kind.is_some() || ex.code.is_some())
    {
        u.error = Some(Box::new(antirion_error::UpstreamError {
            kind: ex.kind,
            code: ex.code,
            param: ex.param,
            passthrough: a.cell == Cell::Identity
                && matches!(
                    code,
                    Code::UpstreamRejected | Code::UpstreamRateLimited | Code::UpstreamUnavailable
                ),
        }));
    }
    if let Some(id) = upstream_request_id(&headers).cloned() {
        f.error.extra_headers.get_or_insert_with(Box::default).insert(
            HeaderName::from_static(antirion_core::headers::UPSTREAM_REQUEST_ID),
            id,
        );
    }
    if class == Class::Timeout {
        f.timer = Some(TimerKind::Upstream);
    }
    f.hint = failure::hint(&headers, parsed.as_ref(), class, a.cx.rt.now());
    f
}
