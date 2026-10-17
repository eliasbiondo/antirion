//! Effective prices: one deployment's (`GET /pricing/deployments/{name}`) and every deployment's,
//! paginated or as CSV (`GET /pricing/deployments`), filterable by provider and by the Models whose
//! trees reach the deployment.

use std::collections::BTreeSet;

use antirion_identity::rbac::Scope;
use antirion_providers::DeploymentRt;
use axum::body::Body;
use axum::response::Response;
use http::{HeaderValue, Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;
use crate::export::field;
use crate::pagination;

/// One deployment's effective price, as both reads show it.
pub(crate) fn effective(call: &Call, d: &DeploymentRt) -> Value {
    let price = call
        .snap
        .pricing
        .price(d.provider.kind.as_str(), &d.base_model, &d.name);
    let (price, version) = match price {
        Some(p) => (p.to_json(), Value::from(p.version())),
        None => (Value::Null, Value::Null),
    };
    json!({
        "deployment": d.name,
        "provider": d.provider.name,
        "provider_type": d.provider.kind.as_str(),
        "upstream_model": d.model,
        "price": price,
        "price_version": version,
    })
}

/// `GET /pricing/deployments`.
pub(crate) fn list(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&["limit", "cursor", "provider", "model", "format"])?;
    call.require("pricing:read", &[Scope::Gateway])?;
    let csv = match call.param("format") {
        None | Some("json") => false,
        Some("csv") => true,
        Some(_) => return Err(AdminError::invalid_parameter("format is json or csv")),
    };
    let reached: Option<BTreeSet<String>> = call.param("model").map(|m| {
        call.snap
            .routing
            .models
            .get(m)
            .and_then(|model| call.snap.routing.route_of(model))
            .map(|r| {
                antirion_router::deployments_of(&r.tree)
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    });
    let provider = call.param("provider");
    let mut names: Vec<&String> = call.snap.providers.deployments.keys().collect();
    names.sort_unstable();
    let selected = names.into_iter().filter_map(|n| {
        let d = call.snap.providers.deployments.get(n)?;
        let keep = provider.is_none_or(|p| d.provider.name == p)
            && reached.as_ref().is_none_or(|r| r.contains(n.as_str()));
        keep.then_some(d)
    });
    if csv {
        let items: Vec<Value> = selected.map(|d| effective(call, d)).collect();
        return Ok(csv_response(call, &items));
    }
    let (limit, after) = pagination::page_params(call, "pricing/deployments")?;
    let mut items = Vec::new();
    let mut next = None;
    for d in selected.filter(|d| after.as_ref().is_none_or(|a| d.name.as_str() > a.as_str())) {
        if items.len() == limit {
            next = items
                .last()
                .and_then(|i: &Value| i.get("deployment"))
                .and_then(Value::as_str)
                .map(|n| pagination::encode(&call.rt.cursor_key, "pricing/deployments", n));
            break;
        }
        items.push(effective(call, d));
    }
    Ok(call.json(StatusCode::OK, &json!({"items": items, "next_cursor": next})))
}

/// The list as CSV: identity columns, then every scalar price member any row has, in name order,
/// then `tiers` as JSON.
fn csv_response(call: &Call, items: &[Value]) -> Response {
    let mut price_cols: BTreeSet<String> = BTreeSet::new();
    for i in items {
        for (k, v) in i.get("price").and_then(Value::as_object).into_iter().flatten() {
            if !v.is_object() && !v.is_array() {
                price_cols.insert(k.clone());
            }
        }
    }
    let fixed = [
        "deployment",
        "provider",
        "provider_type",
        "upstream_model",
        "price_version",
    ];
    let mut out = String::new();
    let header: Vec<String> = fixed
        .iter()
        .map(|c| (*c).to_owned())
        .chain(price_cols.iter().cloned())
        .chain(std::iter::once("tiers".to_owned()))
        .collect();
    out.push_str(&header.join(","));
    out.push_str("\r\n");
    for i in items {
        let mut row: Vec<String> = fixed.iter().map(|c| field(i.get(*c))).collect();
        for c in &price_cols {
            row.push(field(i.get("price").and_then(|p| p.get(c))));
        }
        let tiers = i.pointer("/price/tiers").map(Value::to_string).map(Value::String);
        row.push(field(tiers.as_ref()));
        out.push_str(&row.join(","));
        out.push_str("\r\n");
    }
    let mut resp = call.json(StatusCode::OK, &Value::Null);
    *resp.body_mut() = Body::from(out);
    resp.headers_mut().insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    resp.headers_mut().remove(http::header::CONTENT_LENGTH);
    resp
}
