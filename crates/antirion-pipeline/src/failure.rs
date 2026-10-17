//! Attempt failures: each class of the decision table becomes the error the client gets if the
//! attempt ends the request.

use antirion_core::{Stage, UtcMicros};
use antirion_egress::TransportError;
use antirion_egress::reasons as egress_reasons;
use antirion_error::reason::hygiene;
use antirion_error::{Code, GatewayError, Message, Reason, UpstreamFacts};
use antirion_providers::DeploymentRt;
use antirion_resilience::{AttemptTimers, Class, Failure, TimerKind, format_duration, timeout_reason};

/// A failure of `class` with the client error `code`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn fail(
    d: &DeploymentRt,
    attempts: u16,
    class: Class,
    code: Code,
    reason: Option<Reason>,
    status: Option<u16>,
    message: Message,
) -> Failure {
    let mut e = GatewayError::new(code, Stage::Attempt, message);
    e.reason = reason;
    e.attempts = attempts;
    e.upstream = Some(Box::new(UpstreamFacts {
        provider: d.provider.name.clone(),
        status,
        failure_class: class.as_str(),
        client_status: if code == Code::UpstreamRejected {
            status
        } else {
            None
        },
        error: None,
    }));
    if class == Class::EgressDenied {
        e.not_retryable = true;
    }
    Failure {
        class,
        status,
        timer: None,
        hint: None,
        error: Box::new(e),
    }
}

/// The default reason of a class's final code.
fn default_reason(class: Class) -> Option<Reason> {
    match class {
        Class::CredentialRejected => Some(hygiene::UPSTREAM_CREDENTIAL_REJECTED),
        Class::RateLimited => Some(hygiene::RATE_LIMIT_EXCEEDED),
        Class::Overloaded => Some(hygiene::UPSTREAM_OVERLOADED),
        Class::ServerError | Class::NotImplemented | Class::ModelUnavailable => {
            Some(hygiene::UPSTREAM_SERVER_ERROR)
        }
        Class::DnsError
        | Class::ConnectError
        | Class::ConnectTimeout
        | Class::RefusedStream
        | Class::UpstreamReset => Some(hygiene::CONNECTION_FAILED),
        Class::TlsError => Some(egress_reasons::TLS_HANDSHAKE_FAILED),
        Class::EgressDenied => Some(egress_reasons::EGRESS_DENIED),
        Class::Timeout => Some(timeout_reason(TimerKind::Upstream)),
        _ => None,
    }
}

/// A failure of `class` with its default code and reason.
pub(crate) fn of_class(
    d: &DeploymentRt,
    attempts: u16,
    class: Class,
    status: Option<u16>,
    message: Message,
) -> Failure {
    fail(
        d,
        attempts,
        class,
        class.final_code(),
        default_reason(class),
        status,
        message,
    )
}

/// A transport failure.
pub(crate) fn transport(
    d: &DeploymentRt,
    attempts: u16,
    e: &TransportError,
    timers: &AttemptTimers,
) -> Failure {
    let class = antirion_resilience::classify::transport(e);
    let message = match class {
        Class::ConnectTimeout => Message::built(format!(
            "connect timeout: connect {} exceeded",
            format_duration(timers.connect)
        )),
        Class::DnsError => Message::Fixed("The upstream host name did not resolve."),
        Class::TlsError => Message::Fixed("The TLS handshake with the upstream failed."),
        Class::MalformedResponse => {
            Message::Fixed("The upstream answered with a content-encoding the gateway cannot decode.")
        }
        Class::RefusedStream => Message::Fixed("The upstream refused the request before processing it."),
        Class::UpstreamReset => Message::Fixed("The upstream closed the connection before answering."),
        Class::EgressDenied => Message::Fixed("The egress rules deny the upstream destination."),
        _ => Message::Fixed("The upstream request failed."),
    };
    let reason = match class {
        Class::MalformedResponse => Some(egress_reasons::UNEXPECTED_CONTENT_ENCODING),
        _ => default_reason(class),
    };
    fail(d, attempts, class, class.final_code(), reason, None, message)
}

/// A gateway timer expired.
pub(crate) fn timeout(d: &DeploymentRt, attempts: u16, kind: TimerKind, timers: &AttemptTimers) -> Failure {
    let value = timers.value(kind).map(format_duration).unwrap_or_default();
    let mut f = fail(
        d,
        attempts,
        Class::Timeout,
        Code::UpstreamTimeout,
        Some(timeout_reason(kind)),
        None,
        Message::built(format!("upstream timeout: {} {value} exceeded", kind.as_str())),
    );
    f.timer = Some(kind);
    f
}

/// The client went away.
pub(crate) fn cancelled(d: &DeploymentRt, attempts: u16) -> Failure {
    fail(
        d,
        attempts,
        Class::ClientCancelled,
        Code::Internal,
        None,
        None,
        Message::Fixed("The client went away."),
    )
}

/// The upstream hint of an answer, on the gateway clock.
pub(crate) fn hint(
    headers: &http::HeaderMap,
    body: Option<&serde_json::Value>,
    class: Class,
    now: UtcMicros,
) -> Option<UtcMicros> {
    antirion_resilience::hint(headers, body, class, now)
}
