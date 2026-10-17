//! Resilience: the classification of every attempt outcome, the decision table, the attempt loop
//! with its retries, key rotation, backoff and timers, upstream hints, and the health state of
//! deployments and keys (circuit breakers, cooldowns, dead keys, manual disables).

#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unreachable
)]

pub mod budget;
pub mod class;
pub mod classify;
pub mod engine;
pub mod health;
pub mod hedging;
pub mod hint;
pub mod lease;
pub mod policy;
pub mod predicate;
pub mod settings;

use std::time::Duration;

/// Reasons registered for the resilience rules.
pub mod reasons {
    antirion_error::reasons! {
        RESPONSE_HEADERS_TIMEOUT = "response_headers_timeout" => UpstreamTimeout,
        TTFT_TIMEOUT = "ttft_timeout" => UpstreamTimeout,
        STREAM_IDLE_TIMEOUT = "stream_idle_timeout" => UpstreamTimeout,
        ATTEMPT_TIMEOUT = "attempt_timeout" => UpstreamTimeout,
        DEADLINE_EXCEEDED = "deadline_exceeded" => UpstreamTimeout,
        UPSTREAM_REPORTED_TIMEOUT = "upstream_reported_timeout" => UpstreamTimeout,
        UPSTREAM_COOLING_DOWN = "upstream_cooling_down" => UpstreamRateLimited,
        RESPONSE_PREDICATE_MATCHED = "response_predicate_matched" => UpstreamBadResponse,
    }
}

/// The reason a timer's expiry carries.
#[must_use]
pub fn timeout_reason(kind: TimerKind) -> antirion_error::Reason {
    match kind {
        TimerKind::ResponseHeaders => reasons::RESPONSE_HEADERS_TIMEOUT,
        TimerKind::Ttft => reasons::TTFT_TIMEOUT,
        TimerKind::StreamIdle => reasons::STREAM_IDLE_TIMEOUT,
        TimerKind::Attempt => reasons::ATTEMPT_TIMEOUT,
        TimerKind::Deadline => reasons::DEADLINE_EXCEEDED,
        TimerKind::Upstream => reasons::UPSTREAM_REPORTED_TIMEOUT,
    }
}

/// A duration as the configuration writes it: `300ms`, `2s`, `1.5s`.
#[must_use]
pub fn format_duration(d: Duration) -> String {
    let ms = d.as_millis();
    if ms == 0 {
        format!("{}us", d.as_micros())
    } else if !ms.is_multiple_of(1000) {
        if ms < 1000 {
            format!("{ms}ms")
        } else {
            format!("{}s", d.as_secs_f64())
        }
    } else {
        format!("{}s", ms / 1000)
    }
}

pub use budget::{BudgetSettings, RetryBudget};
pub use class::{Actions, BreakerEffect, Class, RequestKind, RetrySame, TimerKind};
pub use engine::{
    Attempt, AttemptTimers, Failure, KeyPick, LoopCx, Outcome, Planner, Selected, Sender, Sent, run,
};
pub use health::{
    Committed, ConfigChange, DeploymentView, HealthRegistry, KeyInfo, KeyMode, KeyView, SharedRecord, Ticket,
    Unhealthy, Verdict,
};
pub use hedging::Hedging;
pub use hint::hint;
pub use lease::{LeaseAnswer, ProbeLeases};
pub use policy::{Backoff, Policy, Retry, Timeouts};
pub use predicate::{AnswerFacts, ResponsePredicate};
pub use settings::{BreakerSettings, CheckRequest, CheckSettings, HealthConfig, HealthSettings};
