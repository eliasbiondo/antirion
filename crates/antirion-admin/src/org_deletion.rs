//! `POST /organizations/{name}:schedule_deletion` and `:cancel_deletion`: the organization is
//! disabled at once and deleted with everything below it when the grace ends, unless cancelled.

use antirion_config::model::{self, DEFAULT_TENANT};
use antirion_core::UtcMicros;
use antirion_core::audit::Outcome;
use antirion_runtime::changes::Target;
use antirion_runtime::operations::deletion::{CANCELLED, DELETED, FAILED, KIND, SCHEDULED, Schedule};
use antirion_runtime::operations::{DEFAULT_GRACE, MAX_GRACE};
use antirion_store::PendingRequest;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::commit::{Proposal, api_write};
use crate::danger_routes::{body, duration, only, organization};
use crate::error::AdminError;

fn conflict(message: &str) -> AdminError {
    AdminError::conflict("conflict", message, Vec::new())
}

fn store_err(_: antirion_store::StoreError) -> AdminError {
    AdminError::store_unavailable()
}

/// The organization's `enabled` member in the serving document.
fn enabled(call: &Call, name: &str) -> Option<bool> {
    call.snap
        .config
        .doc
        .pointer(&format!("/organizations/{name}/enabled"))
        .and_then(Value::as_bool)
}

/// Commits the organization's `enabled` as `to` (absent for `None`).
async fn set_enabled(call: &Call, name: &str, action: &str, to: Option<bool>) -> Result<u64, AdminError> {
    let resource = format!("organizations/{name}");
    let org = name.to_owned();
    let committed = Box::pin(api_write(call, action, &resource, StatusCode::OK, |state| {
        let mut doc = state.doc.clone();
        let spec = doc
            .pointer_mut(&format!("/organizations/{org}"))
            .and_then(Value::as_object_mut)
            .ok_or_else(AdminError::not_found)?;
        match to {
            Some(b) => {
                spec.insert("enabled".into(), Value::Bool(b));
            }
            None => {
                spec.remove("enabled");
            }
        }
        Ok(Proposal {
            candidate: doc,
            written: vec![Target::Resource("organizations", org.clone())],
            credentials: Vec::new(),
            meta_edits: Vec::new(),
        })
    }))
    .await?;
    Ok(committed.generation)
}

fn shown(organization: &str, delete_at: Option<UtcMicros>, state: &str) -> Value {
    json!({
        "organization": organization,
        "delete_at": delete_at.map(UtcMicros::to_rfc3339),
        "state": state,
    })
}

/// `POST /organizations/{name}:schedule_deletion`.
pub(crate) async fn schedule(call: &Call, name: &str) -> Result<Response, AdminError> {
    only(call, Method::POST, "POST")?;
    call.allow_params(&["wait"])?;
    call.check_wait()?;
    let resource = format!("organizations/{name}");
    let chain = organization(call, name)?;
    call.require("organizations:delete", &chain)
        .map_err(|e| e.with_resource(&resource))?;
    let m = body(call, &["grace"])?;
    let grace = duration(&m, "grace", std::time::Duration::from_secs(1), MAX_GRACE)?.unwrap_or(DEFAULT_GRACE);
    if name == DEFAULT_TENANT
        || model::collection("organizations")
            .and_then(|c| call.snap.config.resource(c, name))
            .is_none()
    {
        return Err(AdminError::conflict(
            "resource_in_use",
            "The default organization cannot be scheduled for deletion.",
            Vec::new(),
        ));
    }
    let existing = call.rt.store.request(KIND, name).await.map_err(store_err)?;
    if existing
        .as_ref()
        .is_some_and(|r| ![CANCELLED, DELETED, FAILED].contains(&r.state.as_str()))
    {
        return Err(conflict("A deletion of this organization is already scheduled."));
    }
    if let Some(held) = Box::pin(crate::change_control::second_person(call, "organization_delete")).await? {
        return Ok(held);
    }
    let delete_at = call.rt.now().saturating_add(grace);
    let schedule = Schedule {
        organization: name.to_owned(),
        delete_at_us: delete_at.0,
        enabled_before: enabled(call, name),
        scheduled_by: call.principal.id.clone(),
        deleted_at_us: None,
        error: None,
    };
    let body = serde_json::to_string(&schedule).map_err(|_| AdminError::internal())?;
    let written = match &existing {
        Some(row) => call
            .rt
            .store
            .update_request(KIND, name, row.version, SCHEDULED, &body)
            .await
            .map_err(store_err)?,
        None => call
            .rt
            .store
            .insert_request(PendingRequest {
                kind: KIND.to_owned(),
                id: name.to_owned(),
                subject: name.to_owned(),
                state: SCHEDULED.to_owned(),
                author: call.principal.id.clone(),
                created_us: call.rt.now().0,
                version: 0,
                body,
            })
            .await
            .map_err(store_err)?,
    };
    if !written {
        return Err(conflict(
            "A deletion of this organization was scheduled concurrently.",
        ));
    }
    let before = call.snap.generation;
    let generation = match Box::pin(set_enabled(
        call,
        name,
        "organizations.schedule_deletion",
        Some(false),
    ))
    .await
    {
        Ok(g) => g,
        Err(e) => {
            if let Ok(Some(row)) = call.rt.store.request(KIND, name).await {
                let _ = call
                    .rt
                    .store
                    .update_request(KIND, name, row.version, CANCELLED, &row.body)
                    .await;
            }
            return Err(e);
        }
    };
    let details = json!({"delete_at": delete_at.to_rfc3339(),
                         "grace": antirion_config::scalar::format_duration(grace)});
    if generation == before {
        call.audit_with(
            "organizations.schedule_deletion",
            &resource,
            Outcome::Success,
            StatusCode::OK,
            details,
        )
        .await;
    }
    let resp = call.json(StatusCode::OK, &shown(name, Some(delete_at), SCHEDULED));
    Ok(call.mutated(resp, generation).await)
}

/// `POST /organizations/{name}:cancel_deletion`: the schedule ends and the organization's
/// `enabled` is what it was before.
pub(crate) async fn cancel(call: &Call, name: &str) -> Result<Response, AdminError> {
    only(call, Method::POST, "POST")?;
    call.allow_params(&["wait"])?;
    call.check_wait()?;
    let resource = format!("organizations/{name}");
    let chain = organization(call, name)?;
    call.require("organizations:delete", &chain)
        .map_err(|e| e.with_resource(&resource))?;
    body(call, &[])?;
    let row = call.rt.store.request(KIND, name).await.map_err(store_err)?;
    let Some(row) = row.filter(|r| r.state == SCHEDULED) else {
        return Err(conflict("No deletion of this organization is scheduled."));
    };
    let schedule: Schedule = serde_json::from_str(&row.body).map_err(|_| AdminError::internal())?;
    let moved = call
        .rt
        .store
        .update_request(KIND, name, row.version, CANCELLED, &row.body)
        .await
        .map_err(store_err)?;
    if !moved {
        return Err(conflict("The scheduled deletion changed concurrently."));
    }
    let before = call.snap.generation;
    let generation = Box::pin(set_enabled(
        call,
        name,
        "organizations.cancel_deletion",
        schedule.enabled_before,
    ))
    .await?;
    if generation == before {
        call.audit(
            "organizations.cancel_deletion",
            &resource,
            Outcome::Success,
            StatusCode::OK,
        )
        .await;
    }
    let resp = call.json(StatusCode::OK, &shown(name, None, CANCELLED));
    Ok(call.mutated(resp, generation).await)
}

/// The pending deletion a read of the organization reports under `metadata.deletion`: the
/// schedule while it waits or runs, the failure once it failed, nothing otherwise.
fn pending(state: &str, schedule: &Schedule) -> Option<Value> {
    let delete_at = UtcMicros(schedule.delete_at_us).to_rfc3339();
    match state {
        CANCELLED | DELETED => None,
        FAILED => Some(json!({
            "state": FAILED,
            "delete_at": delete_at,
            "scheduled_by": schedule.scheduled_by,
            "error": schedule.error,
        })),
        _ => Some(json!({
            "state": SCHEDULED,
            "delete_at": delete_at,
            "scheduled_by": schedule.scheduled_by,
            "error": null,
        })),
    }
}

/// Adds `metadata.deletion` to an organization's representation when a deletion is pending or
/// failed. A store that cannot answer leaves the read as it is.
pub(crate) async fn decorate(call: &Call, name: &str, item: &mut Value) {
    let Ok(Some(row)) = call.rt.store.request(KIND, name).await else {
        return;
    };
    let Ok(schedule) = serde_json::from_str::<Schedule>(&row.body) else {
        return;
    };
    if let (Some(deletion), Some(meta)) = (
        pending(&row.state, &schedule),
        item.get_mut("metadata").and_then(Value::as_object_mut),
    ) {
        meta.insert("deletion".into(), deletion);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_representation_matches_the_contract() {
        let v = shown("acme", Some(UtcMicros(86_400_000_000)), SCHEDULED);
        assert_eq!(
            v,
            json!({"organization": "acme", "delete_at": "1970-01-02T00:00:00Z", "state": "scheduled"})
        );
        assert!(shown("acme", None, CANCELLED)["delete_at"].is_null());
    }

    #[test]
    fn a_read_reports_only_pending_and_failed_deletions() {
        let schedule = Schedule {
            organization: "acme".into(),
            delete_at_us: 86_400_000_000,
            enabled_before: None,
            scheduled_by: "ada".into(),
            deleted_at_us: None,
            error: Some("the configuration would be invalid".into()),
        };
        assert_eq!(
            pending(SCHEDULED, &schedule),
            Some(json!({"state": "scheduled", "delete_at": "1970-01-02T00:00:00Z",
                        "scheduled_by": "ada", "error": null}))
        );
        assert_eq!(
            pending("deleting", &schedule).map(|v| v["state"].clone()),
            Some(json!("scheduled"))
        );
        assert_eq!(
            pending(FAILED, &schedule).map(|v| v["error"].clone()),
            Some(json!("the configuration would be invalid"))
        );
        assert!(pending(CANCELLED, &schedule).is_none());
        assert!(pending(DELETED, &schedule).is_none());
    }
}
