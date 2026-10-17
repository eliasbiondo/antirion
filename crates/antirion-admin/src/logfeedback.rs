//! What the request log holds beside its records: the feedback attached through the data plane,
//! shown with a record, and the erasure of an end user's records (`DELETE /logs?end_user=`).

use antirion_core::UtcMicros;
use antirion_core::audit::Outcome;
use antirion_store::logs::{Arg, LogColumn, LogFilter};
use axum::response::Response;
use http::StatusCode;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::call::Call;
use crate::error::AdminError;
use crate::logquery;

/// Adds the record's `feedback` array (oldest first) to the record shown for `id`.
pub(crate) async fn attach(call: &Call, id: &str, body: &mut Value) -> Result<(), AdminError> {
    let rows = call
        .rt
        .feedback_of(id)
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "metric": r.metric,
                "value": serde_json::from_str::<Value>(&r.value).unwrap_or(Value::Null),
                "created_at": UtcMicros(r.created_us).to_rfc3339_micros(),
            })
        })
        .collect();
    if let Some(obj) = body.as_object_mut() {
        obj.insert("feedback".into(), Value::Array(items));
    }
    Ok(())
}

/// `DELETE /logs?end_user=<id>`: deletes the end user's records in the caller's `logs:delete`
/// scopes, with their feedback, and audits the erasure without the end user's id.
pub(crate) async fn erase(call: &Call) -> Result<Response, AdminError> {
    call.allow_params(&["end_user"])?;
    let end_user = call
        .param("end_user")
        .filter(|v| !v.is_empty())
        .ok_or_else(|| AdminError::invalid_parameter("end_user is required"))?
        .to_owned();
    crate::logs::require_some(call, "logs:delete")?;
    let filter = LogFilter {
        any_of: vec![(LogColumn::EndUser, vec![Arg::Text(end_user.clone())])],
        scopes: logquery::scopes(&call.principal, "logs:delete"),
        ..LogFilter::default()
    };
    let deleted = call
        .rt
        .erase_logs(filter)
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    let digest: String = Sha256::digest(end_user.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    call.audit_with(
        "logs.erase_end_user",
        "logs",
        Outcome::Success,
        StatusCode::OK,
        json!({"end_user_sha256": digest, "deleted": deleted}),
    )
    .await;
    Ok(call.json(StatusCode::OK, &json!({"deleted": deleted})))
}
