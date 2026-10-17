//! The closed table of data-plane error codes and their per-dialect names.

/// `antirion.code`: one row of the closed status table. Every property of a code is a lookup into
/// one table, so a code can never be rendered with the wrong status or type.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Code {
    InvalidRequest,
    UnsupportedCapability,
    NoRouteMatched,
    GuardrailBlocked,
    Unauthenticated,
    BudgetExceeded,
    Forbidden,
    ModelNotFound,
    NotFound,
    MethodNotAllowed,
    NotAcceptable,
    RequestTimeout,
    PayloadTooLarge,
    UnsupportedMediaType,
    HeadersTooLarge,
    RateLimited,
    RequestExceedsLimit,
    UpstreamRejected,
    UpstreamAuthFailed,
    UpstreamQuotaExhausted,
    UpstreamBadResponse,
    UpstreamRateLimited,
    UpstreamUnavailable,
    UpstreamTimeout,
    NoHealthyDeployment,
    DependencyUnavailable,
    Overloaded,
    Draining,
    Internal,
}

/// Every code, for exhaustive tests.
pub const ALL_CODES: [Code; 29] = [
    Code::InvalidRequest,
    Code::UnsupportedCapability,
    Code::NoRouteMatched,
    Code::GuardrailBlocked,
    Code::Unauthenticated,
    Code::BudgetExceeded,
    Code::Forbidden,
    Code::ModelNotFound,
    Code::NotFound,
    Code::MethodNotAllowed,
    Code::NotAcceptable,
    Code::RequestTimeout,
    Code::PayloadTooLarge,
    Code::UnsupportedMediaType,
    Code::HeadersTooLarge,
    Code::RateLimited,
    Code::RequestExceedsLimit,
    Code::UpstreamRejected,
    Code::UpstreamAuthFailed,
    Code::UpstreamQuotaExhausted,
    Code::UpstreamBadResponse,
    Code::UpstreamRateLimited,
    Code::UpstreamUnavailable,
    Code::UpstreamTimeout,
    Code::NoHealthyDeployment,
    Code::DependencyUnavailable,
    Code::Overloaded,
    Code::Draining,
    Code::Internal,
];

struct Row {
    name: &'static str,
    status: u16,
    retryable: bool,
    openai_type: &'static str,
    openai_code: &'static str,
    anthropic_type: &'static str,
    gemini_status: &'static str,
    bedrock_type: &'static str,
    jsonrpc: i32,
}

const fn row(code: Code) -> Row {
    use Code::*;
    let (name, status, retryable) = match code {
        InvalidRequest => ("invalid_request", 400, false),
        UnsupportedCapability => ("unsupported_capability", 400, false),
        NoRouteMatched => ("no_route_matched", 400, false),
        GuardrailBlocked => ("guardrail_blocked", 400, false),
        Unauthenticated => ("unauthenticated", 401, false),
        BudgetExceeded => ("budget_exceeded", 402, false),
        Forbidden => ("forbidden", 403, false),
        ModelNotFound => ("model_not_found", 404, false),
        NotFound => ("not_found", 404, false),
        MethodNotAllowed => ("method_not_allowed", 405, false),
        NotAcceptable => ("not_acceptable", 406, false),
        RequestTimeout => ("request_timeout", 408, true),
        PayloadTooLarge => ("payload_too_large", 413, false),
        UnsupportedMediaType => ("unsupported_media_type", 415, false),
        HeadersTooLarge => ("headers_too_large", 431, false),
        RateLimited => ("rate_limited", 429, true),
        RequestExceedsLimit => ("request_exceeds_limit", 429, false),
        UpstreamRejected => ("upstream_rejected", 400, false),
        UpstreamAuthFailed => ("upstream_auth_failed", 502, false),
        UpstreamQuotaExhausted => ("upstream_quota_exhausted", 502, false),
        UpstreamBadResponse => ("upstream_bad_response", 502, true),
        UpstreamRateLimited => ("upstream_rate_limited", 429, true),
        UpstreamUnavailable => ("upstream_unavailable", 502, true),
        UpstreamTimeout => ("upstream_timeout", 504, true),
        NoHealthyDeployment => ("no_healthy_deployment", 503, true),
        DependencyUnavailable => ("dependency_unavailable", 503, true),
        Overloaded => ("overloaded", 503, true),
        Draining => ("draining", 503, true),
        Internal => ("internal", 500, true),
    };
    let (openai_type, openai_code) = match code {
        InvalidRequest => ("invalid_request_error", "invalid_value"),
        UnsupportedCapability => ("invalid_request_error", "unsupported_parameter"),
        NoRouteMatched => ("invalid_request_error", "invalid_value"),
        GuardrailBlocked => ("invalid_request_error", "content_policy_violation"),
        Unauthenticated => ("invalid_request_error", "invalid_api_key"),
        BudgetExceeded => ("insufficient_quota", "insufficient_quota"),
        Forbidden => ("invalid_request_error", "permission_denied"),
        ModelNotFound => ("invalid_request_error", "model_not_found"),
        NotFound => ("invalid_request_error", "not_found"),
        MethodNotAllowed => ("invalid_request_error", "method_not_allowed"),
        NotAcceptable => ("invalid_request_error", "not_acceptable"),
        RequestTimeout => ("invalid_request_error", "request_timeout"),
        PayloadTooLarge => ("invalid_request_error", "payload_too_large"),
        UnsupportedMediaType => ("invalid_request_error", "unsupported_media_type"),
        HeadersTooLarge => ("invalid_request_error", "headers_too_large"),
        RateLimited | RequestExceedsLimit | UpstreamRateLimited => {
            ("rate_limit_error", "rate_limit_exceeded")
        }
        UpstreamRejected => ("invalid_request_error", "upstream_rejected"),
        UpstreamAuthFailed => ("server_error", "upstream_auth_failed"),
        UpstreamQuotaExhausted => ("server_error", "upstream_quota_exhausted"),
        UpstreamBadResponse => ("server_error", "upstream_bad_response"),
        UpstreamUnavailable => ("server_error", "upstream_unavailable"),
        UpstreamTimeout => ("server_error", "upstream_timeout"),
        NoHealthyDeployment => ("server_error", "no_healthy_deployment"),
        DependencyUnavailable => ("server_error", "dependency_unavailable"),
        Overloaded => ("server_error", "overloaded"),
        Draining => ("server_error", "draining"),
        Internal => ("server_error", "internal"),
    };
    let anthropic_type = match code {
        InvalidRequest
        | UnsupportedCapability
        | NoRouteMatched
        | GuardrailBlocked
        | MethodNotAllowed
        | NotAcceptable
        | UnsupportedMediaType
        | HeadersTooLarge
        | UpstreamRejected => "invalid_request_error",
        Unauthenticated => "authentication_error",
        BudgetExceeded => "billing_error",
        Forbidden => "permission_error",
        ModelNotFound | NotFound => "not_found_error",
        RequestTimeout | UpstreamTimeout => "timeout_error",
        PayloadTooLarge => "request_too_large",
        RateLimited | RequestExceedsLimit | UpstreamRateLimited => "rate_limit_error",
        UpstreamAuthFailed
        | UpstreamQuotaExhausted
        | UpstreamBadResponse
        | UpstreamUnavailable
        | NoHealthyDeployment
        | DependencyUnavailable
        | Internal => "api_error",
        Overloaded | Draining => "overloaded_error",
    };
    let gemini_status = match code {
        InvalidRequest
        | UnsupportedCapability
        | NoRouteMatched
        | GuardrailBlocked
        | MethodNotAllowed
        | NotAcceptable
        | PayloadTooLarge
        | UnsupportedMediaType
        | HeadersTooLarge
        | UpstreamRejected => "INVALID_ARGUMENT",
        Unauthenticated => "UNAUTHENTICATED",
        BudgetExceeded | RateLimited | RequestExceedsLimit | UpstreamRateLimited => "RESOURCE_EXHAUSTED",
        Forbidden => "PERMISSION_DENIED",
        ModelNotFound | NotFound => "NOT_FOUND",
        RequestTimeout | UpstreamTimeout => "DEADLINE_EXCEEDED",
        UpstreamAuthFailed
        | UpstreamQuotaExhausted
        | UpstreamBadResponse
        | UpstreamUnavailable
        | Internal => "INTERNAL",
        NoHealthyDeployment | DependencyUnavailable | Overloaded | Draining => "UNAVAILABLE",
    };
    let bedrock_type = match code {
        InvalidRequest
        | UnsupportedCapability
        | NoRouteMatched
        | GuardrailBlocked
        | MethodNotAllowed
        | NotAcceptable
        | PayloadTooLarge
        | UnsupportedMediaType
        | HeadersTooLarge
        | UpstreamRejected => "ValidationException",
        Unauthenticated => "UnrecognizedClientException",
        BudgetExceeded => "ServiceQuotaExceededException",
        Forbidden => "AccessDeniedException",
        ModelNotFound | NotFound => "ResourceNotFoundException",
        RequestTimeout => "RequestTimeoutException",
        RateLimited | RequestExceedsLimit | UpstreamRateLimited => "ThrottlingException",
        UpstreamAuthFailed
        | UpstreamQuotaExhausted
        | UpstreamBadResponse
        | UpstreamUnavailable
        | Internal => "InternalServerException",
        UpstreamTimeout => "ModelTimeoutException",
        NoHealthyDeployment | DependencyUnavailable | Overloaded | Draining => "ServiceUnavailableException",
    };
    let jsonrpc = match code {
        InvalidRequest
        | UnsupportedCapability
        | NoRouteMatched
        | MethodNotAllowed
        | NotAcceptable
        | RequestTimeout
        | UnsupportedMediaType
        | HeadersTooLarge => -32600,
        Unauthenticated => -32010,
        Forbidden => -32011,
        NotFound | ModelNotFound => -32012,
        RateLimited | RequestExceedsLimit | UpstreamRateLimited => -32013,
        BudgetExceeded => -32014,
        GuardrailBlocked => -32015,
        UpstreamRejected
        | UpstreamAuthFailed
        | UpstreamQuotaExhausted
        | UpstreamBadResponse
        | UpstreamUnavailable
        | NoHealthyDeployment => -32016,
        UpstreamTimeout => -32017,
        Overloaded | Draining | DependencyUnavailable => -32018,
        PayloadTooLarge => -32019,
        Internal => -32603,
    };
    Row {
        name,
        status,
        retryable,
        openai_type,
        openai_code,
        anthropic_type,
        gemini_status,
        bedrock_type,
        jsonrpc,
    }
}

impl Code {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        row(self).name
    }

    /// The status of the code's row; `upstream_rejected` uses the upstream's status instead.
    #[must_use]
    pub const fn status(self) -> u16 {
        row(self).status
    }

    #[must_use]
    pub const fn retryable(self) -> bool {
        row(self).retryable
    }

    #[must_use]
    pub const fn openai_type(self) -> &'static str {
        row(self).openai_type
    }

    #[must_use]
    pub const fn openai_code(self) -> &'static str {
        row(self).openai_code
    }

    #[must_use]
    pub const fn anthropic_type(self) -> &'static str {
        row(self).anthropic_type
    }

    #[must_use]
    pub const fn gemini_status(self) -> &'static str {
        row(self).gemini_status
    }

    #[must_use]
    pub const fn bedrock_type(self) -> &'static str {
        row(self).bedrock_type
    }

    #[must_use]
    pub const fn jsonrpc_code(self) -> i32 {
        row(self).jsonrpc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_snake_case() {
        let mut names: Vec<&str> = ALL_CODES.iter().map(|c| c.as_str()).collect();
        assert!(
            names
                .iter()
                .all(|n| n.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
        );
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), ALL_CODES.len());
    }

    #[test]
    fn every_row_has_a_status_and_types() {
        for code in ALL_CODES {
            assert!((400..=504).contains(&code.status()), "{code:?}");
            assert!(!code.openai_type().is_empty() && !code.openai_code().is_empty());
            assert!(!code.anthropic_type().is_empty() && !code.gemini_status().is_empty());
            assert!(code.bedrock_type().ends_with("Exception"));
        }
    }

    #[test]
    fn selected_rows() {
        assert_eq!(Code::Unauthenticated.openai_code(), "invalid_api_key");
        assert_eq!(Code::RateLimited.openai_type(), "rate_limit_error");
        assert_eq!(Code::PayloadTooLarge.anthropic_type(), "request_too_large");
        assert_eq!(Code::NotFound.jsonrpc_code(), -32012);
        assert!(!Code::RequestExceedsLimit.retryable());
        assert!(Code::Internal.retryable());
    }
}
