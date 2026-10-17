//! The provider-type catalog (`/provider-types`).

use antirion_identity::rbac::Scope;
use antirion_providers::ProviderType;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;

pub(crate) fn provider_types(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&["limit", "cursor"])?;
    call.require("providers:read", &[Scope::Gateway])?;
    let items: Vec<Value> = ProviderType::ALL
        .iter()
        .map(|t| {
            let c = t.catalog();
            json!({
                "type": t.as_str(),
                "core": t.core(),
                "default_base_url": c.default_base_url,
                "dialects": c.dialects,
                "auth_methods": c.auth_methods,
            })
        })
        .collect();
    Ok(call.json(StatusCode::OK, &json!({"items": items, "next_cursor": null})))
}

/// The catalog's hints of a deployment's model: only those it has.
fn hints(ttft: Option<std::time::Duration>) -> Value {
    match ttft {
        Some(t) => json!({"ttft": {
            "value": antirion_config::scalar::format_duration(t),
            "source": "catalog",
        }}),
        None => json!({}),
    }
}

/// `GET /deployments/{d}/capabilities`: every flag with its value and the layer it came from.
pub(crate) fn capabilities(call: &Call, name: &str) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&[])?;
    call.require("deployments:read", &[Scope::Gateway])?;
    let Some(d) = call.snap.providers.deployments.get(name) else {
        return Err(AdminError::not_found());
    };
    let base_model = call
        .snap
        .config
        .doc
        .get("deployments")
        .and_then(|ds| ds.get(name))
        .and_then(|s| s.get("base_model"))
        .and_then(Value::as_str)
        .unwrap_or(&d.model)
        .to_owned();
    let caps: serde_json::Map<String, Value> = antirion_providers::FLAGS
        .iter()
        .map(|f| {
            let (value, source) = d.capabilities.get(f);
            (
                (*f).to_owned(),
                json!({"value": value, "source": source.as_str()}),
            )
        })
        .collect();
    Ok(call.json(
        StatusCode::OK,
        &json!({
            "deployment": d.name,
            "provider": d.provider.name,
            "type": d.provider.kind.as_str(),
            "base_model": base_model,
            "capabilities": caps,
            "hints": hints(d.capabilities.ttft_hint()),
        }),
    ))
}
