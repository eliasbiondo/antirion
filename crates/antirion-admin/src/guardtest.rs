//! `POST /guardrails/{name}:test`: one guardrail run on a sample text or body.

use antirion_config::{ConfigError, DetailCode, DocPath};
use antirion_identity::rbac::Scope;
use antirion_runtime::guardtest::{Sample, TestError};
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::Value;

use crate::call::Call;
use crate::error::AdminError;

fn invalid(field: &str, code: DetailCode, message: &str) -> AdminError {
    AdminError::validation(vec![ConfigError::new(&DocPath::root().key(field), code, message)])
}

/// The stage and the sample of a test request.
fn sample(call: &Call) -> Result<(bool, Sample), AdminError> {
    let body = call.json_body()?;
    if let Some(extra) = body
        .keys()
        .find(|k| !matches!(k.as_str(), "stage" | "text" | "body"))
    {
        return Err(invalid(
            extra,
            DetailCode::UnknownField,
            "not a field of a guardrail test",
        ));
    }
    let output = match body.get("stage").and_then(Value::as_str) {
        Some("input") => false,
        Some("output") => true,
        Some(_) => {
            return Err(invalid(
                "stage",
                DetailCode::InvalidEnum,
                "must be input or output",
            ));
        }
        None => return Err(invalid("stage", DetailCode::Required, "the stage is required")),
    };
    let sample = match (body.get("text"), body.get("body")) {
        (Some(Value::String(t)), None) => Sample::Text(t.clone()),
        (None, Some(b @ Value::Object(_))) => Sample::Body(b.clone()),
        (Some(_), Some(_)) => {
            return Err(invalid(
                "body",
                DetailCode::Conflict,
                "give either text or body, not both",
            ));
        }
        (Some(_), None) => return Err(invalid("text", DetailCode::TypeMismatch, "expected a string")),
        (None, Some(_)) => return Err(invalid("body", DetailCode::TypeMismatch, "expected an object")),
        (None, None) => {
            return Err(invalid(
                "text",
                DetailCode::Required,
                "a text or a body is required",
            ));
        }
    };
    Ok((output, sample))
}

/// Runs guardrail `name` on the request's sample.
pub(crate) async fn run(call: &Call, name: &str) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&[])?;
    call.require("policies:read", &[Scope::Gateway])?;
    if call.snap.guardrails.get(name).is_none() {
        return Err(AdminError::not_found());
    }
    let (output, sample) = sample(call)?;
    let tester = call
        .rt
        .state
        .guard_tests
        .tester()
        .ok_or_else(AdminError::internal)?;
    match tester
        .test(std::sync::Arc::clone(&call.snap), name.to_owned(), output, sample)
        .await
    {
        Ok(result) => Ok(call.json(StatusCode::OK, &result)),
        Err(TestError::Unknown) => Err(AdminError::not_found()),
        Err(TestError::Invalid(m)) => Err(invalid("stage", DetailCode::Conflict, &m)),
        Err(TestError::Unavailable) => Err(AdminError::internal()),
    }
}
