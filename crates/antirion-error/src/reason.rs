//! Registered reasons: the finer, stable cause carried in `antirion.reason`.

use crate::Code;

/// The definition of a reason: its name and the one code it belongs to.
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct ReasonDef {
    pub name: &'static str,
    pub code: Code,
}

/// A reason bound to exactly one code. Declared with [`reasons!`](crate::reasons).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Reason(pub &'static ReasonDef);

impl Reason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0.name
    }

    #[must_use]
    pub const fn code(self) -> Code {
        self.0.code
    }
}

/// Declares reason constants, each bound to one code, and a `REASONS` list of them.
#[macro_export]
macro_rules! reasons {
    ($($name:ident = $value:literal => $code:ident),* $(,)?) => {
        $(
            pub const $name: $crate::Reason =
                $crate::Reason(&$crate::reason::ReasonDef { name: $value, code: $crate::Code::$code });
        )*
        /// Every reason declared in this module.
        pub const REASONS: &[$crate::Reason] = &[$($name),*];
    };
}

/// Reasons of the request-hygiene and upstream-error rules shared by every dialect.
pub mod hygiene {
    crate::reasons! {
        INVALID_JSON = "invalid_json" => InvalidRequest,
        EMPTY_BODY = "empty_body" => InvalidRequest,
        INVALID_PARAMETER = "invalid_parameter" => InvalidRequest,
        INVALID_HEADER = "invalid_header" => InvalidRequest,
        DUPLICATE_KEY = "duplicate_key" => InvalidRequest,
        JSON_TOO_DEEP = "json_too_deep" => InvalidRequest,
        INVALID_PATH = "invalid_path" => InvalidRequest,
        URI_TOO_LONG = "uri_too_long" => InvalidRequest,
        FRAMING_ERROR = "framing_error" => InvalidRequest,
        MISSING_MODEL = "missing_model" => InvalidRequest,
        UNKNOWN_ENDPOINT = "unknown_endpoint" => NotFound,
        ENDPOINT_NOT_SUPPORTED = "endpoint_not_supported" => NotFound,
        MCP_GATEWAY_NOT_FOUND = "mcp_gateway_not_found" => NotFound,
        BODY_TOO_LARGE = "body_too_large" => PayloadTooLarge,
        UNSUPPORTED_CONTENT_ENCODING = "unsupported_content_encoding" => UnsupportedMediaType,
        CORS_ORIGIN_NOT_ALLOWED = "cors_origin_not_allowed" => Forbidden,
        UPSTREAM_CREDENTIAL_REJECTED = "upstream_credential_rejected" => UpstreamAuthFailed,
        RATE_LIMIT_EXCEEDED = "rate_limit_exceeded" => UpstreamRateLimited,
        UPSTREAM_SERVER_ERROR = "upstream_server_error" => UpstreamUnavailable,
        UPSTREAM_OVERLOADED = "upstream_overloaded" => UpstreamUnavailable,
        CONNECTION_FAILED = "connection_failed" => UpstreamUnavailable,
        BODYLESS_STATUS = "bodyless_status" => UpstreamBadResponse,
        REDIRECT_NOT_FOLLOWED = "redirect_not_followed" => UpstreamBadResponse,
        TRUNCATED_BODY = "truncated_body" => UpstreamBadResponse,
        UNEXPECTED_CONTENT_TYPE = "unexpected_content_type" => UpstreamBadResponse,
        INVALID_BODY = "invalid_body" => UpstreamBadResponse,
        TRANSLATION_UNSUPPORTED = "translation_unsupported" => UnsupportedCapability,
        PANIC = "panic" => Internal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hygiene_reasons_are_unique() {
        let mut names: Vec<&str> = hygiene::REASONS.iter().map(|r| r.as_str()).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len());
    }

    #[test]
    fn a_reason_carries_its_code() {
        assert_eq!(hygiene::BODY_TOO_LARGE.code(), Code::PayloadTooLarge);
        assert_eq!(hygiene::FRAMING_ERROR.as_str(), "framing_error");
    }
}
