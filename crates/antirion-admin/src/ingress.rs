//! `GET /runtime/ingress`, `POST /runtime/ingress:pause` and `POST /runtime/ingress:resume`.

use std::time::Duration;

use antirion_config::DetailCode;
use antirion_core::audit::Outcome;
use antirion_identity::rbac::Scope;
use antirion_runtime::operations::ingress::{MAX_DURATION, Pause, Resumed};
use antirion_store::StoreError;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::danger_routes::{body, body_error, duration, only, text};
use crate::error::AdminError;

const MAX_REASON_CHARS: usize = 500;

fn shown(call: &Call) -> Value {
    let pauses: Vec<Value> = call.rt.ingress_pauses().iter().map(Pause::shown).collect();
    json!({"pauses": pauses})
}

/// The audit resource of a pause of `organization` (or of the gateway).
fn resource(organization: Option<&str>) -> String {
    match organization {
        Some(o) => format!("organizations/{o}"),
        None => "runtime/ingress".to_owned(),
    }
}

/// `config:apply` pauses anything; `organizations:update` on the organization pauses it alone.
fn authorize(call: &Call, organization: Option<&str>) -> Result<(), AdminError> {
    let Some(name) = organization else {
        return call.require("config:apply", &[Scope::Gateway]);
    };
    let chain = crate::danger_routes::organization(call, name)?;
    if call.principal.allows("config:apply", &[Scope::Gateway]) {
        return Ok(());
    }
    call.require("organizations:update", &chain)
}

fn store_error(e: &StoreError) -> AdminError {
    match e {
        StoreError::Conflict => AdminError::conflict(
            "conflict",
            "The pause changed concurrently; read it and try again.",
            Vec::new(),
        ),
        _ => AdminError::store_unavailable(),
    }
}

/// `GET /runtime/ingress`.
pub(crate) async fn state(call: &Call) -> Result<Response, AdminError> {
    only(call, Method::GET, "GET")?;
    call.allow_params(&[])?;
    call.require("health:read", &[Scope::Gateway])?;
    Ok(call.json(StatusCode::OK, &shown(call)))
}

/// `POST /runtime/ingress:pause`.
pub(crate) async fn pause(call: &Call) -> Result<Response, AdminError> {
    only(call, Method::POST, "POST")?;
    call.allow_params(&[])?;
    let m = body(call, &["duration", "reason", "organization"])?;
    let org = text(&m, "organization")?.map(str::to_owned);
    authorize(call, org.as_deref()).map_err(|e| e.with_resource(resource(org.as_deref())))?;
    let length = duration(&m, "duration", Duration::from_secs(1), MAX_DURATION)?
        .ok_or_else(|| body_error("duration", DetailCode::Required, "duration is required"))?;
    let reason = text(&m, "reason")?
        .ok_or_else(|| body_error("reason", DetailCode::Required, "reason is required"))?;
    let chars = reason.chars().count();
    if chars == 0 || chars > MAX_REASON_CHARS {
        return Err(body_error(
            "reason",
            DetailCode::OutOfRange,
            "reason must have 1 to 500 characters",
        ));
    }
    if let Some(held) = Box::pin(crate::change_control::second_person(call, "ingress_pause")).await? {
        return Ok(held);
    }
    let now = call.rt.now();
    let pause = Pause {
        organization: org.clone(),
        reason: reason.to_owned(),
        paused_by: call.principal.id.clone(),
        paused_at_us: now.0,
        resumes_at_us: now.saturating_add(length).0,
    };
    let target = resource(org.as_deref());
    if let Err(e) = call.rt.pause_ingress(&pause).await {
        let err = store_error(&e);
        call.audit("runtime.ingress.pause", &target, Outcome::Failed, err.status)
            .await;
        return Err(err);
    }
    call.audit_with(
        "runtime.ingress.pause",
        &target,
        Outcome::Success,
        StatusCode::OK,
        json!({"organization": org, "duration": antirion_config::scalar::format_duration(length),
               "reason": reason, "resumes_at": pause.shown()["resumes_at"]}),
    )
    .await;
    Ok(call.json(StatusCode::OK, &shown(call)))
}

/// `POST /runtime/ingress:resume`: ends the pause of the scope; resuming a scope that is not paused
/// changes nothing.
pub(crate) async fn resume(call: &Call) -> Result<Response, AdminError> {
    only(call, Method::POST, "POST")?;
    call.allow_params(&[])?;
    let m = body(call, &["organization"])?;
    let org = text(&m, "organization")?.map(str::to_owned);
    authorize(call, org.as_deref()).map_err(|e| e.with_resource(resource(org.as_deref())))?;
    let target = resource(org.as_deref());
    let resumed = match call.rt.resume_ingress(org.as_deref()).await {
        Ok(r) => r,
        Err(e) => {
            let err = store_error(&e);
            call.audit("runtime.ingress.resume", &target, Outcome::Failed, err.status)
                .await;
            return Err(err);
        }
    };
    call.audit_with(
        "runtime.ingress.resume",
        &target,
        Outcome::Success,
        StatusCode::OK,
        json!({"organization": org, "was_paused": resumed == Resumed::Ended}),
    )
    .await;
    Ok(call.json(StatusCode::OK, &shown(call)))
}
