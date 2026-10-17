//! MCP runtime resources: a server's discovery status, approval of its tool definitions, and the
//! per-gateway session counters.

use std::sync::Arc;

use antirion_identity::rbac::Scope;
use antirion_mcp::{ApproveError, Env, Hooks};
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;
use crate::pagination;

/// `GET /mcp/servers/<name>/status`.
pub(crate) fn status(call: &Call, name: &str) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&[])?;
    call.require("mcp:read", &[Scope::Gateway])?;
    let body = antirion_mcp::server_status(&call.snap.mcp, &call.rt.state.mcp, name)
        .ok_or_else(AdminError::not_found)?;
    Ok(call.json(StatusCode::OK, &body))
}

/// `POST /mcp/servers/<name>:approve_tools`.
pub(crate) async fn approve_tools(call: &Call, name: &str) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&[])?;
    call.require("mcp:write", &[Scope::Gateway])?;
    let egress = call.rt.mcp_egress(&call.snap, &call.request_id).ok_or_else(|| {
        AdminError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "dependency_unavailable",
            "The egress TLS profile is not available.",
        )
    })?;
    let env = Env {
        snap: Arc::clone(&call.snap.mcp),
        state: Arc::clone(&call.rt.state.mcp),
        egress,
        now: call.rt.now(),
        request_id: call.request_id.clone(),
        tasks: call.rt.tasks.clone(),
        shutdown: call.rt.shutdown.clone(),
        public_base: String::new(),
        hooks: Hooks::default(),
        log: Arc::default(),
    };
    match antirion_mcp::approve_tools(&env, name).await {
        Ok(approval) => Ok(call.json(StatusCode::OK, &approval.to_json())),
        Err(ApproveError::UnknownServer) => Err(AdminError::not_found()),
        Err(ApproveError::Unreachable) => Err(AdminError::new(
            StatusCode::BAD_GATEWAY,
            "upstream_unavailable",
            "The MCP server could not be asked for its tools.",
        )),
    }
}

/// `GET /runtime/mcp/sessions`.
pub(crate) fn sessions(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&["limit", "cursor"])?;
    call.require("mcp:read", &[Scope::Gateway])?;
    let scope = "runtime/mcp/sessions";
    let (limit, after) = pagination::page_params(call, scope)?;
    let stats = antirion_mcp::session_stats(&call.snap.mcp, &call.rt.state.mcp);
    let rest: Vec<_> = stats
        .into_iter()
        .filter(|s| after.as_deref().is_none_or(|a| s.gateway.as_str() > a))
        .collect();
    let next = (rest.len() > limit)
        .then(|| rest.get(limit.saturating_sub(1)).map(|s| s.gateway.clone()))
        .flatten()
        .map(|g| pagination::encode(&call.rt.cursor_key, scope, &g));
    let items: Vec<Value> = rest
        .into_iter()
        .take(limit)
        .map(|s| {
            json!({
                "gateway": s.gateway,
                "session_mode": s.session_mode,
                "issued": s.issued,
                "rejected": s.rejected,
                "revoked": s.revoked,
                "key_ids": s.key_ids,
            })
        })
        .collect();
    Ok(call.json(StatusCode::OK, &json!({"items": items, "next_cursor": next})))
}
