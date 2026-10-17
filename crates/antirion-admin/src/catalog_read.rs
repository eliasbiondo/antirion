//! `GET /catalog`: the effective model catalog of the serving generation and where it comes from.

use antirion_catalog::Catalog;
use antirion_identity::rbac::Scope;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;

pub(crate) fn get(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&[])?;
    call.require("models:read", &[Scope::Gateway])?;
    let catalog = Catalog::from_doc(&call.snap.config.doc);
    let meta = call.snap.config.meta.settings.get("catalog");
    let runtime = catalog.runtime.as_ref().map(|r| {
        json!({
            "mode": r.mode.as_str(),
            "version": r.version,
            "entries": r.entries,
            "updated_at": meta.map(|m| m.updated_at.clone()),
            "generation": meta.map(|m| m.generation),
        })
    });
    let items: Vec<Value> = catalog
        .items()
        .map(|(pattern, entry, source)| {
            let mut v = entry.to_value();
            if let Some(o) = v.as_object_mut() {
                o.insert("pattern".into(), Value::from(pattern));
                o.insert("source".into(), Value::from(source.as_str()));
            }
            v
        })
        .collect();
    Ok(call.json(
        StatusCode::OK,
        &json!({
            "bundled": {
                "version": catalog.bundled.version,
                "price_version": catalog.bundled.price_version,
                "entries": catalog.bundled.entries,
            },
            "runtime": runtime,
            "items": items,
        }),
    ))
}
