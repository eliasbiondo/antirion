//! Runtime state of providers: how the gateway authenticates to each one and whether it verifies
//! the upstream's TLS certificate.

use antirion_identity::rbac::Scope;
use antirion_providers::{ProviderRt, UpstreamAuth};
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;
use crate::pagination;

const SCOPE: &str = "runtime/providers";

fn status(call: &Call, p: &ProviderRt) -> Value {
    let mut credential = json!({"method": p.auth.method()});
    let state = match &p.auth {
        UpstreamAuth::Header { .. } | UpstreamAuth::Aws(Some(_)) => "valid",
        UpstreamAuth::Aws(None) | UpstreamAuth::AwsAssumed { .. } => "implicit",
        UpstreamAuth::None => "none",
        UpstreamAuth::Gcp(_) | UpstreamAuth::Azure(_) => {
            let t = p.auth.token_source().map_or(
                antirion_providers::creds::TokenState {
                    state: "failed",
                    expires_at: None,
                },
                |s| call.rt.creds.token_state(&s),
            );
            if let (Some(at), Some(obj)) = (t.expires_at, credential.as_object_mut()) {
                let expires = antirion_core::UtcMicros(at.saturating_mul(1_000_000));
                obj.insert("expires_at".into(), Value::from(expires.to_rfc3339_seconds()));
            }
            t.state
        }
        UpstreamAuth::Unsupported(_) => "failed",
    };
    if let Some(obj) = credential.as_object_mut() {
        obj.insert("state".into(), Value::from(state));
    }
    let now = call.rt.now();
    let readings = call.rt.state.headroom.of(&p.name);
    let latest = readings
        .iter()
        .max_by_key(|(_, r)| r.observed_at)
        .map_or(Value::Null, |(_, r)| r.json(now));
    let credentials: Vec<Value> = readings
        .iter()
        .map(|(name, r)| json!({"name": name, "rate_limits": r.json(now)}))
        .collect();
    let account_tier = call
        .snap
        .config
        .doc
        .pointer(&format!("/providers/{}/account_tier", p.name))
        .cloned()
        .unwrap_or(Value::Null);
    json!({
        "name": p.name,
        "type": p.kind.as_str(),
        "credential": credential,
        "tls_verify": p.tls.verify,
        "account_tier": account_tier,
        "rate_limits": latest,
        "credentials": credentials,
    })
}

/// `GET /runtime/providers`.
pub(crate) fn providers(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&["limit", "cursor"])?;
    call.require("providers:read", &[Scope::Gateway])?;
    let (limit, after) = pagination::page_params(call, SCOPE)?;
    let mut names: Vec<&String> = call.snap.providers.providers.keys().collect();
    names.sort();
    let rest: Vec<&String> = names
        .into_iter()
        .filter(|n| after.as_deref().is_none_or(|a| n.as_str() > a))
        .collect();
    let page: Vec<Value> = rest
        .iter()
        .take(limit)
        .filter_map(|n| call.snap.providers.providers.get(*n))
        .map(|p| status(call, p))
        .collect();
    let next = (rest.len() > limit)
        .then(|| rest.get(limit.saturating_sub(1)))
        .flatten()
        .map(|n| pagination::encode(&call.rt.cursor_key, SCOPE, n));
    Ok(call.json(StatusCode::OK, &json!({"items": page, "next_cursor": next})))
}

/// `GET /runtime/providers/<name>`.
pub(crate) fn provider(call: &Call, name: &str) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&[])?;
    call.require("providers:read", &[Scope::Gateway])?;
    let p = call
        .snap
        .providers
        .providers
        .get(name)
        .ok_or_else(AdminError::not_found)?;
    Ok(call.json(StatusCode::OK, &status(call, p)))
}

/// `GET /runtime/plugins`: the hook statistics of every configured extension on this node.
pub(crate) fn plugins(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&["limit", "cursor"])?;
    call.require("plugins:read", &[Scope::Gateway])?;
    let scope = "runtime/plugins";
    let (limit, after) = pagination::page_params(call, scope)?;
    let rest: Vec<&String> = call
        .snap
        .plugins
        .by_name
        .keys()
        .filter(|n| after.as_deref().is_none_or(|a| n.as_str() > a))
        .collect();
    let next = (rest.len() > limit)
        .then(|| rest.get(limit.saturating_sub(1)).map(|n| (*n).clone()))
        .flatten()
        .map(|n| pagination::encode(&call.rt.cursor_key, scope, &n));
    let items: Vec<Value> = rest
        .into_iter()
        .take(limit)
        .map(|n| call.rt.state.extensions.report(n))
        .collect();
    Ok(call.json(StatusCode::OK, &json!({"items": items, "next_cursor": next})))
}

/// `GET /runtime/plugins/<name>`.
pub(crate) fn plugin(call: &Call, name: &str) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&[])?;
    call.require("plugins:read", &[Scope::Gateway])?;
    let configured = call
        .snap
        .config
        .doc
        .get("plugins")
        .and_then(|p| p.get(name))
        .is_some();
    if !configured {
        return Err(AdminError::not_found());
    }
    Ok(call.json(StatusCode::OK, &call.rt.state.extensions.report(name)))
}
