//! Classification of an attempt's outcome: a transport failure by its kind, an upstream answer by
//! its structured provider code first and its status second. Messages are only matched by anchored
//! per-provider patterns, and only to recognise context-length outcomes.

use antirion_egress::TransportError;
use http::HeaderMap;
use serde_json::Value;

use crate::class::Class;

/// Classifies a transport failure.
#[must_use]
pub fn transport(e: &TransportError) -> Class {
    match e {
        TransportError::Dns => Class::DnsError,
        TransportError::Connect | TransportError::UnsupportedScheme(_) => Class::ConnectError,
        TransportError::ConnectTimeout => Class::ConnectTimeout,
        TransportError::Tls => Class::TlsError,
        TransportError::RefusedStream => Class::RefusedStream,
        TransportError::EgressDenied(_) => Class::EgressDenied,
        TransportError::UnexpectedEncoding => Class::MalformedResponse,
        TransportError::Timeout => Class::Timeout,
        TransportError::Reset => Class::UpstreamReset,
    }
}

/// Classifies an upstream status alone (the status rows of the decision table). `generation` is
/// false for requests that reference a provider-scoped resource, where a 404 is the caller's.
#[must_use]
pub fn status(status: u16, generation: bool) -> Class {
    match status {
        200..=299 if !matches!(status, 204 | 205) => Class::Success,
        100..=399 => Class::MalformedResponse,
        401 | 403 => Class::CredentialRejected,
        402 => Class::QuotaExhausted,
        404 if generation => Class::ModelUnavailable,
        408 => Class::Timeout,
        409 => Class::Conflict,
        411 | 417 => Class::MalformedResponse,
        421 | 425 => Class::RefusedStream,
        429 => Class::RateLimited,
        400..=499 => Class::CallerError,
        501 => Class::NotImplemented,
        503 | 529 => Class::Overloaded,
        504 | 524 => Class::Timeout,
        _ => Class::ServerError,
    }
}

/// An upstream answer to classify: its status, headers and parsed error body.
#[derive(Copy, Clone, Debug)]
pub struct Answer<'a> {
    pub status: u16,
    pub headers: &'a HeaderMap,
    pub body: Option<&'a Value>,
    pub generation: bool,
}

/// Classifies an upstream error answer: a structured provider code wins over the status, except
/// a provider's catch-all server code (Anthropic `api_error`, Gemini `INTERNAL`/`UNKNOWN`, Bedrock
/// `InternalServerException`), which names no cause: an error status with a cause of its own (402,
/// 401, 503, ...) keeps it, as the one status table of the error model requires.
#[must_use]
pub fn answer(a: &Answer<'_>) -> Class {
    let by_status = status(a.status, a.generation);
    match provider_code(a) {
        Some(Class::ServerError)
            if !matches!(
                by_status,
                Class::Success | Class::MalformedResponse | Class::ServerError
            ) =>
        {
            by_status
        }
        Some(c) => c,
        None => by_status,
    }
}

/// The class a structured provider error code names, if the answer carries one.
#[must_use]
pub fn provider_code(a: &Answer<'_>) -> Option<Class> {
    if let Some(c) = bedrock(a) {
        return Some(c);
    }
    let error = a.body.and_then(|b| b.get("error"))?;
    let text = |name: &str| error.get(name).and_then(Value::as_str);
    let message = text("message").unwrap_or_default();
    if let Some(s) = text("status").filter(|s| s.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')) {
        let c = match s {
            "RESOURCE_EXHAUSTED" => Some(Class::RateLimited),
            "UNAVAILABLE" => Some(Class::Overloaded),
            "INTERNAL" | "UNKNOWN" => Some(Class::ServerError),
            "DEADLINE_EXCEEDED" => Some(Class::Timeout),
            "UNAUTHENTICATED" | "PERMISSION_DENIED" => Some(Class::CredentialRejected),
            "NOT_FOUND" if a.generation => Some(Class::ModelUnavailable),
            "INVALID_ARGUMENT" if token_limit(message) => Some(Class::ContextLength),
            _ => None,
        };
        if c.is_some() {
            return c;
        }
    }
    if let Some(code) = text("code") {
        let c = match code {
            "insufficient_quota" => Some(Class::QuotaExhausted),
            "context_length_exceeded" | "string_above_max_length" => Some(Class::ContextLength),
            "content_policy_violation" | "content_filter" => Some(Class::ContentPolicy),
            "model_not_found" | "deployment_not_found" | "DeploymentNotFound" => {
                Some(Class::ModelUnavailable)
            }
            "invalid_encrypted_content" => Some(Class::ReasoningReplayRejected),
            _ => None,
        };
        if c.is_some() {
            return c;
        }
    }
    if error.pointer("/innererror/code").and_then(Value::as_str) == Some("ResponsibleAIPolicyViolation") {
        return Some(Class::ContentPolicy);
    }
    match text("type")? {
        "overloaded_error" => Some(Class::Overloaded),
        "rate_limit_error"
            if error.pointer("/details/error_code").and_then(Value::as_str)
                == Some("enforced_spend_limit_reached") =>
        {
            Some(Class::QuotaExhausted)
        }
        "rate_limit_error" => Some(Class::RateLimited),
        "authentication_error" | "permission_error" => Some(Class::CredentialRejected),
        "api_error" => Some(Class::ServerError),
        "timeout_error" => Some(Class::Timeout),
        "conflict_error" => Some(Class::Conflict),
        "billing_error" => Some(Class::QuotaExhausted),
        "invalid_request_error" if message.starts_with("prompt is too long") => Some(Class::ContextLength),
        "invalid_request_error" if thinking_signature(message) => Some(Class::ReasoningReplayRejected),
        _ => None,
    }
}

fn bedrock(a: &Answer<'_>) -> Option<Class> {
    let raw = a
        .headers
        .get("x-amzn-errortype")
        .and_then(|v| v.to_str().ok())
        .or_else(|| a.body.and_then(|b| b.get("__type")).and_then(Value::as_str))?;
    let name = raw.split(':').next().unwrap_or(raw);
    let name = name.rsplit('#').next().unwrap_or(name);
    let message = a
        .body
        .and_then(|b| b.get("message").or_else(|| b.get("Message")))
        .and_then(Value::as_str)
        .unwrap_or_default();
    Some(match name {
        "ThrottlingException" | "ServiceQuotaExceededException" | "TooManyRequestsException" => {
            Class::RateLimited
        }
        "ServiceUnavailableException" => Class::Overloaded,
        "InternalServerException"
        | "ModelErrorException"
        | "ModelNotReadyException"
        | "ModelStreamErrorException" => Class::ServerError,
        "ModelTimeoutException" => Class::Timeout,
        "AccessDeniedException" | "UnrecognizedClientException" | "ExpiredTokenException" => {
            Class::CredentialRejected
        }
        "ResourceNotFoundException" => Class::ModelUnavailable,
        "ValidationException" if bedrock_context(message) => Class::ContextLength,
        "ValidationException" => Class::CallerError,
        _ => return None,
    })
}

fn bedrock_context(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.starts_with("input is too long")
        || m.contains("too many input tokens")
        || m.contains("maximum context length")
}

/// Anthropic's rejection of a replayed thinking block: its signature is invalid, or no longer
/// matches the conversation it was produced for.
fn thinking_signature(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    (m.contains("invalid `signature`") || m.contains("invalid signature"))
        && (m.contains("`thinking`") || m.contains("thinking block"))
}

fn token_limit(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("exceeds the maximum number of tokens")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn classify(status: u16, body: Value) -> Class {
        let h = HeaderMap::new();
        answer(&Answer {
            status,
            headers: &h,
            body: Some(&body),
            generation: true,
        })
    }

    #[test]
    fn statuses() {
        assert_eq!(status(500, true), Class::ServerError);
        assert_eq!(status(520, true), Class::ServerError);
        assert_eq!(status(529, true), Class::Overloaded);
        assert_eq!(status(504, true), Class::Timeout);
        assert_eq!(status(421, true), Class::RefusedStream);
        assert_eq!(status(404, true), Class::ModelUnavailable);
        assert_eq!(status(404, false), Class::CallerError);
        assert_eq!(status(499, true), Class::CallerError);
        assert_eq!(status(204, true), Class::MalformedResponse);
        assert_eq!(status(200, true), Class::Success);
    }

    #[test]
    fn codes_win_over_statuses_and_messages_never_do() {
        assert_eq!(
            classify(
                429,
                json!({"error": {"code": "insufficient_quota", "message": "x"}})
            ),
            Class::QuotaExhausted
        );
        assert_eq!(
            classify(400, json!({"error": {"code": "model_not_found"}})),
            Class::ModelUnavailable
        );
        assert_eq!(
            classify(
                400,
                json!({"error": {"message": "rate limit 429 exceeded", "type": "invalid_request_error"}})
            ),
            Class::CallerError
        );
        assert_eq!(
            classify(
                200,
                json!({"type": "error", "error": {"type": "overloaded_error"}})
            ),
            Class::Overloaded
        );
        assert_eq!(
            classify(
                429,
                json!({"error": {"status": "RESOURCE_EXHAUSTED", "code": 429}})
            ),
            Class::RateLimited
        );
        let mut h = HeaderMap::new();
        h.insert(
            "x-amzn-errortype",
            "ThrottlingException:http://x/".parse().unwrap(),
        );
        assert_eq!(
            answer(&Answer {
                status: 400,
                headers: &h,
                body: None,
                generation: true
            }),
            Class::RateLimited
        );
    }

    #[test]
    fn rejected_reasoning_replays() {
        assert_eq!(
            classify(
                400,
                json!({"error": {"code": "invalid_encrypted_content", "message": "x"}})
            ),
            Class::ReasoningReplayRejected
        );
        let anthropic = json!({"type": "error", "error": {"type": "invalid_request_error",
            "message": "messages.1.content.0: Invalid `signature` in `thinking` block"}});
        assert_eq!(classify(400, anthropic), Class::ReasoningReplayRejected);
        let other = json!({"error": {"type": "invalid_request_error", "message": "invalid signature"}});
        assert_eq!(classify(400, other), Class::CallerError);
    }

    #[test]
    fn catch_all_server_codes_keep_a_specific_status() {
        let api_error = json!({"type": "error", "error": {"type": "api_error", "message": "x"}});
        assert_eq!(classify(402, api_error.clone()), Class::QuotaExhausted);
        assert_eq!(classify(503, api_error.clone()), Class::Overloaded);
        assert_eq!(classify(500, api_error.clone()), Class::ServerError);
        assert_eq!(classify(200, api_error), Class::ServerError);
        assert_eq!(
            classify(402, json!({"error": {"code": 402, "status": "UNKNOWN"}})),
            Class::QuotaExhausted
        );
        assert_eq!(
            classify(401, json!({"__type": "InternalServerException", "message": "x"})),
            Class::CredentialRejected
        );
    }

    #[test]
    fn transport_kinds() {
        assert_eq!(transport(&TransportError::Dns), Class::DnsError);
        assert_eq!(transport(&TransportError::ConnectTimeout), Class::ConnectTimeout);
        assert_eq!(transport(&TransportError::Reset), Class::UpstreamReset);
    }
}
