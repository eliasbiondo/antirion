//! The failure classes and the decision table: which actions each class allows, how it counts for
//! the breaker, and the code the client gets when it ends the request.

use antirion_error::Code;

/// The timer that ended an attempt, or `Upstream` for a provider that reported a timeout itself.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum TimerKind {
    ResponseHeaders,
    Ttft,
    StreamIdle,
    Attempt,
    Deadline,
    Upstream,
}

impl TimerKind {
    /// The configuration key of the timer, as error messages and the attempt trail name it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            TimerKind::ResponseHeaders => "response_headers",
            TimerKind::Ttft => "ttft",
            TimerKind::StreamIdle => "stream_idle",
            TimerKind::Attempt => "attempt",
            TimerKind::Deadline => "deadline",
            TimerKind::Upstream => "upstream",
        }
    }
}

/// The class of one attempt's outcome.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Class {
    DnsError,
    ConnectError,
    ConnectTimeout,
    TlsError,
    RefusedStream,
    UpstreamReset,
    Timeout,
    RateLimited,
    QuotaExhausted,
    CredentialRejected,
    Overloaded,
    ServerError,
    NotImplemented,
    ModelUnavailable,
    MalformedResponse,
    Conflict,
    ContextLength,
    ContentPolicy,
    /// The upstream rejected encrypted reasoning or thinking signatures the request replayed.
    ReasoningReplayRejected,
    CallerError,
    GuardrailBlocked,
    ClientCancelled,
    /// The egress rules refused the destination: a configuration fault, never retried.
    EgressDenied,
    /// A hedged attempt cancelled because another attempt won: it reaches no verdict.
    HedgeCancelled,
    Success,
}

/// Whether the loop may re-send to the same deployment, and how.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RetrySame {
    No,
    /// After the backoff and any hint.
    Backoff,
    /// At once, on a new connection.
    Immediate,
    /// Once, at once, to the same pair, with the rejected reasoning content removed.
    Repair,
    /// After the backoff, and only with a key not yet tried in this request.
    UntriedKey,
}

/// The actions a class allows (the RK, ND and RS columns).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Actions {
    pub rotate_key: bool,
    pub next_deployment: bool,
    pub retry_same: RetrySame,
}

impl Actions {
    pub(crate) const FAIL: Actions = Actions {
        rotate_key: false,
        next_deployment: false,
        retry_same: RetrySame::No,
    };

    /// Whether the class allows nothing but ending the request.
    #[must_use]
    pub fn only_fail(self) -> bool {
        !self.rotate_key && !self.next_deployment && self.retry_same == RetrySame::No
    }
}

/// How a verdict counts for the deployment's breaker.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BreakerEffect {
    Fail,
    Ok,
    Neutral,
}

/// What kind of request is being retried (the restricted rules for some kinds).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum RequestKind {
    #[default]
    Generation,
    /// References a provider-scoped resource; only its owner may serve it.
    ResourceBound,
    /// Creates or changes provider state; re-sent only after a failure before sending.
    SideEffecting,
}

impl Class {
    /// The stable class name used in the trail, logs and config.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Class::DnsError => "dns_error",
            Class::ConnectError => "connect_error",
            Class::ConnectTimeout => "connect_timeout",
            Class::TlsError => "tls_error",
            Class::RefusedStream => "refused_stream",
            Class::UpstreamReset => "upstream_reset",
            Class::Timeout => "timeout",
            Class::RateLimited => "rate_limited",
            Class::QuotaExhausted => "quota_exhausted",
            Class::CredentialRejected => "credential_rejected",
            Class::Overloaded => "overloaded",
            Class::ServerError => "server_error",
            Class::NotImplemented => "not_implemented",
            Class::ModelUnavailable => "model_unavailable",
            Class::MalformedResponse => "malformed_response",
            Class::Conflict => "conflict",
            Class::ContextLength => "context_length",
            Class::ContentPolicy => "content_policy",
            Class::ReasoningReplayRejected => "reasoning_replay_rejected",
            Class::CallerError => "caller_error",
            Class::GuardrailBlocked => "guardrail_blocked",
            Class::ClientCancelled => "client_cancelled",
            Class::EgressDenied => "egress_denied",
            Class::HedgeCancelled => "hedge_cancelled",
            Class::Success => "success",
        }
    }

    /// The class with a given name.
    #[must_use]
    pub fn parse(name: &str) -> Option<Class> {
        const ALL: [Class; 25] = [
            Class::DnsError,
            Class::ConnectError,
            Class::ConnectTimeout,
            Class::TlsError,
            Class::RefusedStream,
            Class::UpstreamReset,
            Class::Timeout,
            Class::RateLimited,
            Class::QuotaExhausted,
            Class::CredentialRejected,
            Class::Overloaded,
            Class::ServerError,
            Class::NotImplemented,
            Class::ModelUnavailable,
            Class::MalformedResponse,
            Class::Conflict,
            Class::ContextLength,
            Class::ContentPolicy,
            Class::ReasoningReplayRejected,
            Class::CallerError,
            Class::GuardrailBlocked,
            Class::ClientCancelled,
            Class::EgressDenied,
            Class::HedgeCancelled,
            Class::Success,
        ];
        ALL.into_iter().find(|c| c.as_str() == name)
    }

    /// A class failed before anything was sent upstream.
    #[must_use]
    pub fn pre_send(self) -> bool {
        matches!(
            self,
            Class::DnsError
                | Class::ConnectError
                | Class::ConnectTimeout
                | Class::TlsError
                | Class::RefusedStream
        )
    }

    /// The decision table's actions for this class.
    #[must_use]
    pub fn actions(self) -> Actions {
        let a = |rotate_key, next_deployment, retry_same| Actions {
            rotate_key,
            next_deployment,
            retry_same,
        };
        match self {
            Class::DnsError | Class::ConnectError | Class::ConnectTimeout | Class::UpstreamReset => {
                a(false, true, RetrySame::Backoff)
            }
            Class::TlsError | Class::NotImplemented | Class::ModelUnavailable | Class::MalformedResponse => {
                a(false, true, RetrySame::No)
            }
            Class::RefusedStream => a(false, true, RetrySame::Immediate),
            Class::Timeout => a(false, true, RetrySame::UntriedKey),
            Class::RateLimited => a(true, true, RetrySame::Backoff),
            Class::QuotaExhausted | Class::CredentialRejected => a(true, true, RetrySame::No),
            Class::Overloaded | Class::ServerError => a(false, true, RetrySame::Backoff),
            Class::Conflict => a(false, true, RetrySame::Backoff),
            Class::ReasoningReplayRejected => a(false, false, RetrySame::Repair),
            Class::ContextLength
            | Class::ContentPolicy
            | Class::CallerError
            | Class::GuardrailBlocked
            | Class::ClientCancelled
            | Class::EgressDenied
            | Class::HedgeCancelled
            | Class::Success => Actions::FAIL,
        }
    }

    /// The actions for a request of `kind`: a side-effecting request moves on only after a failure
    /// before sending.
    #[must_use]
    pub fn actions_for(self, kind: RequestKind) -> Actions {
        let all = self.actions();
        match kind {
            RequestKind::SideEffecting if !self.pre_send() => Actions::FAIL,
            RequestKind::ResourceBound => Actions {
                next_deployment: false,
                ..all
            },
            _ => all,
        }
    }

    /// The Breaker column.
    #[must_use]
    pub fn breaker(self) -> BreakerEffect {
        match self {
            Class::DnsError
            | Class::ConnectError
            | Class::ConnectTimeout
            | Class::TlsError
            | Class::UpstreamReset
            | Class::Timeout
            | Class::Overloaded
            | Class::ServerError
            | Class::ModelUnavailable
            | Class::MalformedResponse => BreakerEffect::Fail,
            Class::Success => BreakerEffect::Ok,
            _ => BreakerEffect::Neutral,
        }
    }

    /// The client code when this class ends the request (the Final column's default).
    #[must_use]
    pub fn final_code(self) -> Code {
        match self {
            Class::Timeout => Code::UpstreamTimeout,
            Class::RateLimited => Code::UpstreamRateLimited,
            Class::QuotaExhausted => Code::UpstreamQuotaExhausted,
            Class::CredentialRejected => Code::UpstreamAuthFailed,
            Class::MalformedResponse => Code::UpstreamBadResponse,
            Class::Conflict
            | Class::ContextLength
            | Class::ContentPolicy
            | Class::ReasoningReplayRejected
            | Class::CallerError => Code::UpstreamRejected,
            Class::GuardrailBlocked => Code::GuardrailBlocked,
            _ => Code::UpstreamUnavailable,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_rows() {
        assert_eq!(Class::RateLimited.actions().retry_same, RetrySame::Backoff);
        assert!(Class::RateLimited.actions().rotate_key);
        assert!(Class::CallerError.actions().only_fail());
        assert_eq!(Class::TlsError.actions().retry_same, RetrySame::No);
        assert_eq!(Class::Timeout.breaker(), BreakerEffect::Fail);
        assert_eq!(Class::RefusedStream.breaker(), BreakerEffect::Neutral);
        assert_eq!(Class::Conflict.final_code(), Code::UpstreamRejected);
        assert_eq!(Class::parse("rate_limited"), Some(Class::RateLimited));
        assert_eq!(Class::parse("nope"), None);
        let repair = Class::ReasoningReplayRejected.actions();
        assert_eq!(repair.retry_same, RetrySame::Repair);
        assert!(!repair.rotate_key && !repair.next_deployment);
        assert_eq!(Class::ReasoningReplayRejected.breaker(), BreakerEffect::Neutral);
        assert_eq!(
            Class::parse("reasoning_replay_rejected"),
            Some(Class::ReasoningReplayRejected)
        );
    }

    #[test]
    fn side_effecting_requests_move_only_after_pre_send_failures() {
        assert!(
            Class::UpstreamReset
                .actions_for(RequestKind::SideEffecting)
                .only_fail()
        );
        let connect = Class::ConnectError.actions_for(RequestKind::SideEffecting);
        assert_eq!(connect.retry_same, RetrySame::Backoff);
        assert!(
            !Class::ServerError
                .actions_for(RequestKind::ResourceBound)
                .next_deployment
        );
    }
}
