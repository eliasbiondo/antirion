//! The test-mode endpoints. They exist only when the process runs in test mode, and only principals
//! with the `gateway_admin` role may call them; every call is audited.

use antirion_config::scalar::parse_duration;
use antirion_config::{ConfigError, DetailCode, DocPath};
use antirion_core::audit::Outcome;
use antirion_core::{Clock, UtcMicros};
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;

fn invalid(field: &str, code: DetailCode, message: &str) -> AdminError {
    AdminError::validation(vec![ConfigError::new(&DocPath::root().key(field), code, message)])
}

pub(crate) async fn handle(call: &Call, action: &str) -> Result<Response, AdminError> {
    let Some(test) = call.rt.test.clone() else {
        return Err(AdminError::not_found());
    };
    let allowed = match action {
        "clock" => "GET, POST",
        "reset" => "POST",
        "seed" => "PUT",
        _ => return Err(AdminError::not_found()),
    };
    let method_ok = match action {
        "clock" => call.method == Method::GET || call.method == Method::POST,
        "reset" => call.method == Method::POST,
        _ => call.method == Method::PUT,
    };
    if !method_ok {
        return Err(AdminError::method_not_allowed(allowed));
    }
    call.allow_params(&[])?;
    let resource = format!("test/{action}");
    call.require_gateway_admin()
        .map_err(|e| e.with_resource(resource.clone()))?;
    if call.method == Method::GET {
        return Ok(call.json(StatusCode::OK, &json!({"now": test.clock.now().to_rfc3339()})));
    }
    let body: Value = if call.body.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&call.body)
            .map_err(|_| invalid("", DetailCode::MalformedDocument, "the body is not JSON"))?
    };
    let resp = match action {
        "clock" => {
            if let Some(advance) = body.get("advance") {
                let d = advance.as_str().and_then(parse_duration).ok_or_else(|| {
                    invalid(
                        "advance",
                        DetailCode::InvalidFormat,
                        "advance must be a non-negative duration",
                    )
                })?;
                test.clock.advance(d);
            } else if let Some(set) = body.get("set") {
                let t = set
                    .as_str()
                    .and_then(|s| UtcMicros::parse_rfc3339(s).ok())
                    .ok_or_else(|| {
                        invalid("set", DetailCode::InvalidFormat, "set must be an RFC 3339 time")
                    })?;
                test.clock.set(t).map_err(|_| {
                    invalid("set", DetailCode::OutOfRange, "the test clock only moves forward")
                })?;
            } else {
                return Err(invalid(
                    "advance",
                    DetailCode::Required,
                    "advance or set is required",
                ));
            }
            call.json(StatusCode::OK, &json!({"now": test.clock.now().to_rfc3339()}))
        }
        "reset" => {
            let scopes: Vec<String> = match body.get("scopes") {
                None => Vec::new(),
                Some(Value::Array(items)) => items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                Some(_) => {
                    return Err(invalid(
                        "scopes",
                        DetailCode::TypeMismatch,
                        "scopes must be a list",
                    ));
                }
            };
            call.rt
                .reset(&scopes)
                .await
                .map_err(|_| AdminError::store_unavailable())?;
            call.no_content()
        }
        _ => {
            let seed = body.get("seed").and_then(Value::as_u64).ok_or_else(|| {
                invalid(
                    "seed",
                    DetailCode::TypeMismatch,
                    "seed must be an unsigned integer",
                )
            })?;
            test.seed(seed);
            call.no_content()
        }
    };
    call.audit(
        &format!("test.{action}"),
        &resource,
        Outcome::Success,
        resp.status(),
    )
    .await;
    Ok(resp)
}
