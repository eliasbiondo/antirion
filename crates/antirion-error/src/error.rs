//! The one data-plane error type.

use std::time::Duration;

use antirion_core::Stage;
use http::{HeaderMap, HeaderName, HeaderValue};

use crate::{Code, Reason};

/// A client-facing message. Gateway messages are fixed sentences or built from safe parts (field
/// paths, limit names); they never carry a secret or the text of an internal error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    Fixed(&'static str),
    Built(Box<str>),
}

impl Message {
    /// A message composed at run time.
    #[must_use]
    pub fn built(text: impl Into<String>) -> Message {
        Message::Built(text.into().into_boxed_str())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Message::Fixed(s) => s,
            Message::Built(s) => s,
        }
    }
}

/// What is known about the upstream when an attempt was involved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpstreamFacts {
    pub provider: String,
    /// The status of the last upstream response, when there was one.
    pub status: Option<u16>,
    /// The retry-table class of the failure.
    pub failure_class: &'static str,
    /// The client status when it is the upstream's (`upstream_rejected`).
    pub client_status: Option<u16>,
    /// What the upstream's error body said, when it was in a known shape.
    pub error: Option<Box<UpstreamError>>,
}

/// The structured members of an upstream error body.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpstreamError {
    /// The upstream's own type string: OpenAI `error.type`, Anthropic `error.type`, Gemini
    /// `error.status`, or the Bedrock exception name.
    pub kind: Option<String>,
    /// OpenAI `error.code`, as a string.
    pub code: Option<String>,
    /// OpenAI `error.param`.
    pub param: Option<String>,
    /// The client speaks the upstream's dialect, so the upstream's type, code and param are kept.
    pub passthrough: bool,
}

/// The guardrail that blocked a request.
#[derive(Clone, Debug, PartialEq)]
pub struct GuardrailFacts {
    /// What `antirion.guardrail` shows.
    pub shown: serde_json::Value,
    /// The status a routing block chose instead of `guardrail_blocked`'s own.
    pub status: Option<u16>,
}

/// Every failure on the data plane, from a 404 at `accept` to an upstream error. Rare members are
/// boxed so the error stays small on every `Result`.
#[derive(Clone, Debug)]
pub struct GatewayError {
    pub code: Code,
    pub reason: Option<Reason>,
    pub stage: Stage,
    pub message: Message,
    /// OpenAI `error.param`: the field or header the error is about.
    pub param: Option<Box<str>>,
    /// OpenAI `error.code` when the table allows a finer value than the code's default.
    pub openai_code: Option<&'static str>,
    pub attempts: u16,
    pub upstream: Option<Box<UpstreamFacts>>,
    /// The refusing limit or budget, rendered as `antirion.limit`.
    pub limit: Option<Box<serde_json::Value>>,
    /// The blocking guardrail.
    pub guardrail: Option<Box<GuardrailFacts>>,
    pub retry_after: Option<Duration>,
    pub extra_headers: Option<Box<HeaderMap>>,
    /// The connection must be closed after this response (framing errors, unread bodies).
    pub close_connection: bool,
    /// The failure is final although its code is retryable (a destination the egress rules deny).
    pub not_retryable: bool,
    /// The operator hides guardrail details: `antirion.guardrail` is left out, not `null`.
    pub hide_guardrail: bool,
}

impl GatewayError {
    #[must_use]
    pub fn new(code: Code, stage: Stage, message: Message) -> GatewayError {
        GatewayError {
            code,
            reason: None,
            stage,
            message,
            param: None,
            openai_code: None,
            attempts: 0,
            upstream: None,
            limit: None,
            guardrail: None,
            retry_after: None,
            extra_headers: None,
            close_connection: false,
            not_retryable: false,
            hide_guardrail: false,
        }
    }

    /// An error whose code is the reason's own.
    #[must_use]
    pub fn with_reason(reason: Reason, stage: Stage, message: Message) -> GatewayError {
        let mut e = GatewayError::new(reason.code(), stage, message);
        e.reason = Some(reason);
        e
    }

    #[must_use]
    pub fn param(mut self, param: impl Into<String>) -> GatewayError {
        self.param = Some(param.into().into_boxed_str());
        self
    }

    #[must_use]
    pub fn openai_code(mut self, code: &'static str) -> GatewayError {
        self.openai_code = Some(code);
        self
    }

    #[must_use]
    pub fn header(mut self, name: HeaderName, value: HeaderValue) -> GatewayError {
        self.extra_headers
            .get_or_insert_with(Box::default)
            .append(name, value);
        self
    }

    /// The same error, closing the client connection after the response.
    #[must_use]
    pub fn closing(mut self) -> GatewayError {
        self.close_connection = true;
        self
    }

    #[must_use]
    pub fn status(&self) -> u16 {
        self.guardrail
            .as_ref()
            .and_then(|g| g.status)
            .or_else(|| self.upstream.as_ref().and_then(|u| u.client_status))
            .unwrap_or_else(|| self.code.status())
    }

    #[must_use]
    pub fn retryable(&self) -> bool {
        self.code.retryable() && !self.not_retryable
    }

    /// The value of `x-antirion-response-path` for this error.
    #[must_use]
    pub fn response_path(&self) -> &'static str {
        match self.code {
            Code::RateLimited | Code::RequestExceedsLimit => "rate_limited",
            Code::BudgetExceeded => "budget",
            Code::GuardrailBlocked => "blocked",
            _ => "error",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_error_stays_small() {
        assert!(
            std::mem::size_of::<GatewayError>() <= 120,
            "{}",
            std::mem::size_of::<GatewayError>()
        );
    }
}
