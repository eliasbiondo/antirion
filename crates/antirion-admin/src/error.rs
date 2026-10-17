//! The Admin API error envelope: `{"error": {code, message, request_id, details?}}`.

use antirion_config::ConfigError;
use antirion_core::RequestId;
use axum::body::Body;
use axum::response::Response;
use http::header::{ALLOW, CONTENT_TYPE, RETRY_AFTER, WWW_AUTHENTICATE};
use http::{HeaderValue, StatusCode};
use serde_json::{Map, Value, json};

/// An Admin API failure.
#[derive(Debug)]
pub(crate) struct AdminError {
    pub(crate) status: StatusCode,
    pub(crate) code: &'static str,
    pub(crate) message: String,
    pub(crate) details: Option<Vec<ConfigError>>,
    pub(crate) allow: Option<&'static str>,
    /// What the audit entry of a denied call names, when more precise than the path.
    pub(crate) resource: Option<String>,
    /// Seconds for `Retry-After` on a 429.
    pub(crate) retry_after: Option<u32>,
}

impl AdminError {
    pub(crate) fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> AdminError {
        AdminError {
            status,
            code,
            message: message.into(),
            details: None,
            allow: None,
            resource: None,
            retry_after: None,
        }
    }

    pub(crate) fn not_found() -> AdminError {
        AdminError::new(StatusCode::NOT_FOUND, "not_found", "The resource does not exist.")
    }

    pub(crate) fn forbidden(permission: &str) -> AdminError {
        AdminError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            format!("This call needs the {permission} permission."),
        )
    }

    /// A self-service call made by a principal that is not a human user.
    pub(crate) fn needs_human() -> AdminError {
        AdminError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "This call needs a human user account: service accounts and the bootstrap credential have no \
             profile, preferences, second factors or personal tokens.",
        )
    }

    pub(crate) fn unauthenticated(message: &str) -> AdminError {
        AdminError::new(StatusCode::UNAUTHORIZED, "unauthenticated", message)
    }

    pub(crate) fn invalid_parameter(message: impl Into<String>) -> AdminError {
        AdminError::new(StatusCode::BAD_REQUEST, "invalid_parameter", message)
    }

    pub(crate) fn validation(details: Vec<ConfigError>) -> AdminError {
        let n = details.len();
        let mut e = AdminError::new(
            StatusCode::BAD_REQUEST,
            "validation_failed",
            if n == 1 {
                "1 validation error".to_owned()
            } else {
                format!("{n} validation errors")
            },
        );
        e.details = Some(details);
        e
    }

    pub(crate) fn malformed(details: Vec<ConfigError>) -> AdminError {
        let mut e = AdminError::new(
            StatusCode::BAD_REQUEST,
            "malformed_document",
            "The document cannot be parsed.",
        );
        e.details = Some(details);
        e
    }

    pub(crate) fn method_not_allowed(allow: &'static str) -> AdminError {
        let mut e = AdminError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "Method not allowed.",
        );
        e.allow = Some(allow);
        e
    }

    pub(crate) fn precondition_failed() -> AdminError {
        AdminError::new(
            StatusCode::PRECONDITION_FAILED,
            "precondition_failed",
            "The precondition does not hold.",
        )
    }

    pub(crate) fn conflict(
        code: &'static str,
        message: impl Into<String>,
        details: Vec<ConfigError>,
    ) -> AdminError {
        let mut e = AdminError::new(StatusCode::CONFLICT, code, message);
        if !details.is_empty() {
            e.details = Some(details);
        }
        e
    }

    pub(crate) fn store_unavailable() -> AdminError {
        AdminError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "store_unavailable",
            "The store cannot be written.",
        )
    }

    pub(crate) fn internal() -> AdminError {
        AdminError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "Internal error.")
    }

    /// The answer of an operator-identity failure.
    pub(crate) fn identity(e: antirion_runtime::operators::IdentityError) -> AdminError {
        use antirion_runtime::operators::IdentityError as E;
        match e {
            E::Unauthenticated => AdminError::unauthenticated("The credentials are not valid."),
            E::NotEnabled | E::NotFound => AdminError::not_found(),
            E::LocalLoginRefused => {
                let mut e = AdminError::new(
                    StatusCode::FORBIDDEN,
                    "forbidden",
                    "Password sign-in is not allowed for this account; use single sign-on.",
                );
                e.details = Some(vec![ConfigError {
                    path: String::new(),
                    code: antirion_config::DetailCode::LocalLoginDisabled,
                    message: "local sign-in is disabled for this account".to_owned(),
                }]);
                e
            }
            E::Throttled { retry_after } => {
                let mut e = AdminError::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    "rate_limited",
                    "Too many attempts; try again later.",
                );
                e.retry_after = Some(u32::try_from(retry_after.as_secs()).unwrap_or(u32::MAX).max(1));
                e
            }
            E::Policy(rule) => {
                let mut e = AdminError::new(
                    StatusCode::BAD_REQUEST,
                    "validation_failed",
                    "The new password does not follow the password policy.",
                );
                e.details = Some(vec![ConfigError {
                    path: rule.as_str().to_owned(),
                    code: antirion_config::DetailCode::PasswordPolicy,
                    message: rule.as_str().to_owned(),
                }]);
                e
            }
            E::Invalid(m) => AdminError::invalid_parameter(m),
            E::Conflict(m) => AdminError::conflict("conflict", m, Vec::new()),
            E::Store(_) => AdminError::store_unavailable(),
            E::Internal => AdminError::internal(),
        }
    }

    pub(crate) fn with_resource(mut self, resource: impl Into<String>) -> AdminError {
        self.resource = Some(resource.into());
        self
    }

    pub(crate) fn into_response(self, request_id: &RequestId) -> Response {
        let mut error = Map::new();
        error.insert("code".into(), Value::from(self.code));
        error.insert("message".into(), Value::from(self.message));
        error.insert("request_id".into(), Value::from(request_id.as_str()));
        if let Some(details) = &self.details {
            error.insert(
                "details".into(),
                serde_json::to_value(details).unwrap_or(Value::Array(Vec::new())),
            );
        }
        let body = json!({ "error": Value::Object(error) });
        let mut resp = Response::new(Body::from(serde_json::to_vec(&body).unwrap_or_default()));
        *resp.status_mut() = self.status;
        let h = resp.headers_mut();
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(allow) = self.allow {
            h.insert(ALLOW, HeaderValue::from_static(allow));
        }
        if self.status == StatusCode::UNAUTHORIZED {
            h.insert(
                WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"antirion\""),
            );
        }
        if self.status == StatusCode::SERVICE_UNAVAILABLE {
            h.insert(RETRY_AFTER, HeaderValue::from_static("1"));
        }
        if let Some(secs) = self.retry_after {
            h.insert(RETRY_AFTER, HeaderValue::from(secs));
        }
        resp
    }
}
