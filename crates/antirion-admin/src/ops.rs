//! Discovery and operations: status, detailed health, the OpenAPI document, metrics and the audit log.

use std::sync::OnceLock;

use antirion_config::{Format, parse};
use antirion_identity::rbac::Scope;
use antirion_runtime::operations::ingress::Pause;
use antirion_runtime::operations::maintenance;
use antirion_store::AuditQuery;
use axum::body::Body;
use axum::response::Response;
use http::header::CONTENT_TYPE;
use http::{HeaderValue, Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;
use crate::pagination;

/// The feature areas this build declares in its status: every area it implements.
const FEATURES: &[&str] = &[
    "a2a", "api", "auth", "budget", "cache", "conf", "deploy", "err", "ext", "fail", "guard", "mcp", "obs",
    "perf", "prov", "rate", "route", "sec", "stream", "ui", "xlate",
];

/// The finer features of the providers, streaming and API edge that it declares.
const EDGE_FEATURES: &[&str] = &[
    "upstream_http_version",
    "azure_ad",
    "assume_role",
    "byok",
    "gemini_json_array",
];

/// The finer features of identity, limits and budgets that it declares.
const LIMITS_FEATURES: &[&str] = &["self_service_limits"];

/// The finer features of the endpoints beyond generation that it declares.
const ENDPOINT_FEATURES: &[&str] = &["files"];

/// The finer features of routing, resilience and cost that it declares.
const ROUTING_FEATURES: &[&str] = &["hedging"];

/// The finer features of extensions, deployment and hardening that it declares.
const HARDENING_FEATURES: &[&str] = &[
    "ext_stream_chunk",
    "ext_attempt",
    "ext_wasm",
    "upstream_client_cert",
];

static OPENAPI_YAML: &str = include_str!("../../../openapi/admin-api.yaml");

pub(crate) fn openapi_json() -> &'static Value {
    static DOC: OnceLock<Value> = OnceLock::new();
    DOC.get_or_init(|| parse(OPENAPI_YAML.as_bytes(), Format::Yaml).unwrap_or(Value::Null))
}

fn only_get(call: &Call) -> Result<(), AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&[])
}

pub(crate) async fn status(call: &Call) -> Result<Response, AdminError> {
    only_get(call)?;
    let snap = &call.snap;
    let (nodes, _) = crate::nodes::live_nodes(call).await;
    let nodes: Vec<Value> = nodes
        .iter()
        .map(|n| {
            json!({
                "id": n.node_id,
                "generation": n.config_generation,
                "last_seen": antirion_core::UtcMicros(n.last_seen_us).to_rfc3339_seconds(),
                "in_flight": n.load.in_flight,
                "max_in_flight": n.load.max_in_flight,
                "queue_depth": n.load.queue_depth,
                "started_at": antirion_core::UtcMicros(n.load.started_us).to_rfc3339_seconds(),
                "ready_since": n.load.ready_since_us.map(|t| antirion_core::UtcMicros(t).to_rfc3339_seconds()),
            })
        })
        .collect();
    let mut features = FEATURES.to_vec();
    features.extend_from_slice(EDGE_FEATURES);
    features.extend_from_slice(LIMITS_FEATURES);
    features.extend_from_slice(ENDPOINT_FEATURES);
    features.extend_from_slice(HARDENING_FEATURES);
    features.extend_from_slice(ROUTING_FEATURES);
    if call.rt.node.test_hooks && call.rt.test.is_some() {
        features.push("test_hooks");
    }
    let ui = &call.rt.statics.ui;
    let body = json!({
        "version": call.rt.node.version,
        "commit": call.rt.node.commit,
        "mode": call.rt.mode(),
        "test_mode": call.rt.test.is_some(),
        "config_generation": snap.generation,
        "config_hash": snap.hash,
        "nodes": nodes,
        "features": features,
        "data_plane_url": ui.data_plane_url,
        "trace_viewer_url": snap
            .config
            .doc
            .pointer("/observability/traces/viewer_url")
            .cloned()
            .unwrap_or(Value::Null),
        "console": {"enabled": ui.enabled, "path": if ui.path.is_empty() { "/" } else { ui.path.as_str() }},
        "dry_run_all": snap.routing.settings.dry_run_all,
        "ingress_pauses": call.rt.ingress_pauses().iter().map(Pause::shown).collect::<Vec<_>>(),
        "maintenance_window": maintenance::shown(&snap.config.doc, call.rt.now()),
    });
    let mut body = body;
    if call.require("users:read", &[Scope::Gateway]).is_ok() {
        body["users"] = crate::users_cap::status(&call.rt).await;
    }
    Ok(call.json(StatusCode::OK, &body))
}

pub(crate) async fn health(call: &Call) -> Result<Response, AdminError> {
    only_get(call)?;
    call.require("health:read", &[Scope::Gateway])?;
    let rt = &call.rt;
    let started = std::time::Instant::now();
    let store_up = match rt.store.postgres() {
        Some(pg) if pg.is_down() => false,
        _ => rt.store.ping().await.is_ok(),
    };
    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let last_error = rt.store.postgres().and_then(|pg| pg.health().last_error);
    let mut dependencies = serde_json::Map::new();
    dependencies.insert(
        "store".into(),
        json!({"status": if store_up { "up" } else { "down" }, "since": null, "last_error": last_error, "latency_ms": latency_ms}),
    );
    if rt.accounts.engine().store().valkey().is_some() {
        let counters_up = !rt.accounts.engine().is_store_down();
        dependencies.insert(
            "counters".into(),
            json!({"status": if counters_up { "up" } else { "down" }, "since": null, "last_error": null, "latency_ms": null}),
        );
    }
    let draining = rt.drain.is_draining();
    let adm = rt.admission.state();
    let degraded = !rt.degraded().is_empty() || !store_up;
    let status = match (draining, degraded) {
        (true, _) => "draining",
        (false, false) => "ok",
        (false, true) => "degraded",
    };
    let spool = rt.ledger.state();
    let skew = rt.cluster.as_ref().and_then(|c| c.view().clock_skew_ms);
    let body = json!({
        "status": status,
        "draining": draining,
        "dependencies": dependencies,
        "admission": {
            "in_flight": adm.in_flight,
            "queued": adm.queued,
            "max_in_flight": adm.limits.max_in_flight,
            "max_queue": adm.limits.max_queue,
        },
        "spool": {"bytes": spool.bytes, "max_bytes": spool.max_bytes, "records": spool.records},
        "config": {"generation": call.snap.generation, "hash": call.snap.hash},
        "clock_skew_ms": if rt.cluster.is_some() { json!(skew) } else { json!(0) },
    });
    Ok(call.json(StatusCode::OK, &body))
}

pub(crate) fn openapi(call: &Call) -> Result<Response, AdminError> {
    only_get(call)?;
    Ok(call.json(StatusCode::OK, openapi_json()))
}

/// The exposition of the metrics path on the admin listener, for an authenticated scrape.
pub(crate) fn metrics(call: &Call) -> Result<Response, AdminError> {
    only_get(call)?;
    call.require("metrics:read", &[Scope::Gateway])?;
    let accept = call
        .headers
        .get(http::header::ACCEPT)
        .and_then(|v| v.to_str().ok());
    Ok(exposition(&call.rt, accept))
}

/// The exposition in the format `accept` asks for.
pub(crate) fn exposition(rt: &antirion_runtime::Runtime, accept: Option<&str>) -> Response {
    let format = antirion_obs::Format::negotiate(accept);
    rt.observe_dependencies();
    let mut resp = Response::new(Body::from(rt.metrics.render(format)));
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(format.content_type()));
    resp
}

fn time_param(call: &Call, name: &str) -> Result<Option<antirion_core::UtcMicros>, AdminError> {
    call.param(name)
        .map(|v| {
            antirion_core::UtcMicros::parse_rfc3339(v)
                .map_err(|_| AdminError::invalid_parameter(format!("{name} must be an RFC 3339 time")))
        })
        .transpose()
}

/// `GET /audit`, newest first. The log is append-only: every write method is refused.
pub(crate) async fn audit_list(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&[
        "limit", "cursor", "actor", "resource", "action", "outcome", "from", "to", "kind",
    ])?;
    let kind = call.param("kind").map(str::to_owned);
    if kind.as_deref().is_some_and(|k| k != "read" && k != "write") {
        return Err(AdminError::invalid_parameter("kind must be read or write"));
    }
    call.require("audit:read", &[Scope::Gateway])?;
    let (limit, after) = pagination::page_params(call, "audit")?;
    let before = match after {
        Some(a) => Some(
            a.parse::<i64>()
                .map_err(|_| AdminError::invalid_parameter("the cursor is not valid"))?,
        ),
        None => None,
    };
    let query = AuditQuery {
        actor: call.param("actor").map(str::to_owned),
        resource: call.param("resource").map(str::to_owned),
        action: call.param("action").map(str::to_owned),
        outcome: call.param("outcome").map(str::to_owned),
        kind,
        from: time_param(call, "from")?,
        to: time_param(call, "to")?,
        before,
        limit,
    };
    let page = call
        .rt
        .store
        .audit(query)
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    let next = page
        .next
        .map(|seq| pagination::encode(&call.rt.cursor_key, "audit", &seq.to_string()));
    Ok(call.json(StatusCode::OK, &json!({"items": page.items, "next_cursor": next})))
}

pub(crate) async fn audit_entry(call: &Call, id: &str) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&[])?;
    call.require("audit:read", &[Scope::Gateway])?;
    match call
        .rt
        .store
        .audit_entry(id.to_owned())
        .await
        .map_err(|_| AdminError::store_unavailable())?
    {
        Some(e) => Ok(call.json(StatusCode::OK, &serde_json::to_value(e).unwrap_or(Value::Null))),
        None => Err(AdminError::not_found()),
    }
}
