//! The cluster view: `GET /cluster`, `GET /cluster/converged`, `GET /runtime/nodes[/{id}]` and the
//! drain action `POST /runtime/nodes/{id}:drain`. A standalone node is a cluster of one.

use std::time::Duration;

use antirion_core::UtcMicros;
use antirion_identity::rbac::Scope;
use antirion_runtime::drain::Origin;
use antirion_store::pg::NodeRow;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;

const CONVERGE_POLL: Duration = Duration::from_millis(50);

fn this_node(call: &Call) -> NodeRow {
    let rt = &call.rt;
    NodeRow {
        node_id: rt.node.id.clone(),
        version: rt.node.version.clone(),
        config_generation: rt.gens.load().generation,
        ready: antirion_runtime::probes::ready(rt),
        draining: rt.drain.is_draining(),
        degraded: rt.degraded().into_iter().map(str::to_owned).collect(),
        last_seen_us: rt.now().0,
        alive: true,
        load: antirion_runtime::probes::load(rt),
    }
}

fn node_json(n: &NodeRow) -> Value {
    json!({
        "node_id": n.node_id,
        "version": n.version,
        "config_version": n.config_generation,
        "ready": n.ready,
        "draining": n.draining,
        "degraded": n.degraded,
        "last_seen": UtcMicros(n.last_seen_us).to_rfc3339_seconds(),
        "in_flight": n.load.in_flight,
        "max_in_flight": n.load.max_in_flight,
        "queue_depth": n.load.queue_depth,
        "started_at": UtcMicros(n.load.started_us).to_rfc3339_seconds(),
        "ready_since": n.load.ready_since_us.map(|t| UtcMicros(t).to_rfc3339_seconds()),
    })
}

/// The sums across ready nodes.
fn totals(nodes: &[NodeRow]) -> Value {
    let ready = nodes.iter().filter(|n| n.ready);
    let (mut in_flight, mut max, mut queue, mut capped) = (0i64, 0i64, 0i64, true);
    for n in ready {
        in_flight += n.load.in_flight;
        queue += n.load.queue_depth;
        match n.load.max_in_flight {
            Some(m) => max += m,
            None => capped = false,
        }
    }
    json!({"in_flight": in_flight, "max_in_flight": capped.then_some(max), "queue_depth": queue})
}

/// The live nodes, this one with its current state; `true` when the store could not be read and
/// the last known list is shown.
pub(crate) async fn live_nodes(call: &Call) -> (Vec<NodeRow>, bool) {
    let me = this_node(call);
    let (Some(cluster), Some(pg)) = (&call.rt.cluster, call.rt.store.postgres()) else {
        return (vec![me], false);
    };
    let (rows, stale) = match pg.nodes(cluster.settings.node_ttl).await {
        Ok(rows) => (rows.into_iter().filter(|n| n.alive).collect(), false),
        Err(_) => (cluster.view().nodes, true),
    };
    let mut out: Vec<NodeRow> = rows.into_iter().filter(|n| n.node_id != me.node_id).collect();
    out.push(me);
    out.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    (out, stale)
}

fn only_get(call: &Call) -> Result<(), AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    Ok(())
}

/// `GET /cluster`: the nodes with their applied versions, and the job leases.
pub(crate) async fn cluster(call: &Call) -> Result<Response, AdminError> {
    only_get(call)?;
    call.allow_params(&[])?;
    call.require("cluster:read", &[Scope::Gateway])?;
    let (nodes, stale) = live_nodes(call).await;
    let jobs: Vec<Value> = match (&call.rt.cluster, call.rt.store.postgres()) {
        (Some(c), Some(pg)) => pg
            .leases()
            .await
            .unwrap_or_else(|_| c.view().leases)
            .iter()
            .map(|l| {
                json!({
                    "name": l.job,
                    "holder": l.holder,
                    "expires_at": UtcMicros(l.expires_us).to_rfc3339(),
                })
            })
            .collect(),
        _ => Vec::new(),
    };
    let mut body = json!({
        "mode": call.rt.mode(),
        "nodes": nodes.iter().map(node_json).collect::<Vec<_>>(),
        "jobs": jobs,
        "totals": totals(&nodes),
    });
    if stale && let Some(o) = body.as_object_mut() {
        o.insert("stale".into(), Value::Bool(true));
    }
    Ok(call.json(StatusCode::OK, &body))
}

/// `GET /cluster/converged`: waits until every ready node applied at least `config_version`.
pub(crate) async fn converged(call: &Call) -> Result<Response, AdminError> {
    only_get(call)?;
    call.allow_params(&["config_version", "timeout"])?;
    call.require("cluster:read", &[Scope::Gateway])?;
    let version: u64 = call
        .param("config_version")
        .ok_or_else(|| AdminError::invalid_parameter("config_version is required"))?
        .parse()
        .map_err(|_| AdminError::invalid_parameter("config_version must be a non-negative integer"))?;
    let timeout = match call.param("timeout") {
        None => Duration::from_secs(5),
        Some(t) => antirion_config::scalar::parse_duration(t)
            .ok_or_else(|| AdminError::invalid_parameter("timeout must be a duration"))?,
    };
    if timeout > call.rt.statics.max_wait {
        return Err(AdminError::invalid_parameter(
            "timeout exceeds admin.api.max_wait",
        ));
    }
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let (nodes, _) = live_nodes(call).await;
        let lagging: Vec<String> = nodes
            .into_iter()
            .filter(|n| n.ready && n.config_generation < version)
            .map(|n| n.node_id)
            .collect();
        if lagging.is_empty() {
            return Ok(call.json(StatusCode::OK, &json!({"converged": true})));
        }
        if tokio::time::Instant::now() + CONVERGE_POLL > deadline {
            return Ok(call.json(StatusCode::OK, &json!({"converged": false, "lagging": lagging})));
        }
        tokio::time::sleep(CONVERGE_POLL).await;
    }
}

/// `GET /runtime/nodes`.
pub(crate) async fn list(call: &Call) -> Result<Response, AdminError> {
    only_get(call)?;
    call.allow_params(&["limit", "cursor"])?;
    call.require("cluster:read", &[Scope::Gateway])?;
    let (nodes, _) = live_nodes(call).await;
    Ok(call.json(
        StatusCode::OK,
        &json!({"items": nodes.iter().map(node_json).collect::<Vec<_>>(), "next_cursor": null}),
    ))
}

/// `GET /runtime/nodes/{id}`, or the `:drain` action on it.
pub(crate) async fn one(call: &Call, segment: &str) -> Result<Response, AdminError> {
    match segment.split_once(':') {
        None => {
            only_get(call)?;
            call.allow_params(&[])?;
            call.require("cluster:read", &[Scope::Gateway])?;
            let (nodes, _) = live_nodes(call).await;
            let node = nodes
                .iter()
                .find(|n| n.node_id == segment)
                .ok_or_else(AdminError::not_found)?;
            Ok(call.json(StatusCode::OK, &node_json(node)))
        }
        Some((id, "drain")) => drain(call, id).await,
        Some(_) => Err(AdminError::not_found()),
    }
}

/// Takes a node out of service: readiness fails and new data-plane requests are refused, while
/// the process and its listeners stay up. Another node of the cluster is asked through the store
/// and starts within its heartbeat interval. `wait=true` (this node only) returns once nothing is
/// in flight or queued, or at the drain deadline.
async fn drain(call: &Call, id: &str) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&["wait"])?;
    call.require("node:drain", &[Scope::Gateway])?;
    let wait = call.bool_param("wait", false)?;
    if id != call.rt.node.id {
        let (nodes, _) = live_nodes(call).await;
        let Some(pg) = call.rt.store.postgres() else {
            return Err(AdminError::not_found());
        };
        if !nodes.iter().any(|n| n.node_id == id) {
            return Err(AdminError::not_found());
        }
        if wait {
            return Err(AdminError::invalid_parameter(
                "wait=true applies only to the node that receives the call",
            ));
        }
        return match pg.request_drain(id).await {
            Ok(true) => Ok(call.json(StatusCode::ACCEPTED, &json!({"status": "draining"}))),
            Ok(false) => Err(AdminError::not_found()),
            Err(_) => Err(AdminError::store_unavailable()),
        };
    }
    let d = &call.rt.drain;
    d.start(Origin::Api);
    if !wait {
        return Ok(call.json(
            StatusCode::ACCEPTED,
            &json!({"status": "draining", "in_flight": d.active()}),
        ));
    }
    let deadline = d.deadline().unwrap_or_else(tokio::time::Instant::now);
    let body = tokio::select! {
        () = d.wait_idle() => json!({"status": "drained"}),
        () = tokio::time::sleep_until(deadline) => json!({"status": "deadline", "in_flight": d.active()}),
    };
    Ok(call.json(StatusCode::OK, &body))
}
