//! Deployment health (`/runtime/deployments`): breaker, cooldown and key state of this node, and
//! the manual disable, enable and reset actions, which are audited.

use std::sync::Arc;

use antirion_core::UtcMicros;
use antirion_core::audit::Outcome;
use antirion_identity::rbac::Scope;
use antirion_providers::DeploymentRt;
use antirion_resilience::{DeploymentView, KeyInfo};
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;
use crate::pagination;

const SCOPE: &str = "runtime/deployments";

fn time(t: Option<UtcMicros>) -> Value {
    t.map_or(Value::Null, |t| Value::from(t.to_rfc3339()))
}

fn keys_of(d: &DeploymentRt) -> Vec<KeyInfo> {
    d.provider
        .keys
        .iter()
        .map(|k| KeyInfo {
            name: k.name.clone(),
            weight: k.weight,
        })
        .collect()
}

fn view_json(name: &str, v: &DeploymentView) -> Value {
    let keys: Vec<Value> = v
        .keys
        .iter()
        .map(|k| {
            json!({
                "key": k.key,
                "scope_state": if k.scope_dead { "dead" } else { "alive" },
                "dead_reason": k.dead_reason.map(|r| r.as_str()),
                "recheck_at": time(k.recheck_at),
                "pair_state": k.pair_state,
                "cooldown_until": time(k.cooldown_until),
                "last_success_at": time(k.last_success_at),
            })
        })
        .collect();
    json!({
        "name": name,
        "deployment": name,
        "state": v.state,
        "trip_error_rate": v.trip_error_rate,
        "open_until": time(v.open_until),
        "trips": v.trips,
        "consecutive": v.consecutive,
        "window": {"requests": v.window_requests, "failures": v.window_failures},
        "cooldown_until": time(v.cooldown_until),
        "disabled_until": time(v.disabled_until),
        "keys": keys,
    })
}

fn view(call: &Call, d: &DeploymentRt) -> Value {
    let settings = call.snap.health.for_deployment(&d.name);
    let v = call
        .rt
        .state
        .health
        .view(&d.name, &d.provider.name, &keys_of(d), settings, call.rt.now());
    view_json(&d.name, &v)
}

/// `GET /runtime/deployments`.
pub(crate) fn list(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&["limit", "cursor"])?;
    call.require("health:read", &[Scope::Gateway])?;
    let (limit, after) = pagination::page_params(call, SCOPE)?;
    let mut names: Vec<&String> = call.snap.providers.deployments.keys().collect();
    names.sort();
    let rest: Vec<&String> = names
        .into_iter()
        .filter(|n| after.as_deref().is_none_or(|a| n.as_str() > a))
        .collect();
    let page: Vec<Value> = rest
        .iter()
        .take(limit)
        .filter_map(|n| call.snap.providers.deployments.get(*n))
        .map(|d| view(call, d))
        .collect();
    let next = (rest.len() > limit)
        .then(|| rest.get(limit.saturating_sub(1)))
        .flatten()
        .map(|n| pagination::encode(&call.rt.cursor_key, SCOPE, n));
    Ok(call.json(StatusCode::OK, &json!({"items": page, "next_cursor": next})))
}

fn deployment(call: &Call, name: &str) -> Result<Arc<DeploymentRt>, AdminError> {
    call.snap
        .providers
        .deployments
        .get(name)
        .cloned()
        .ok_or_else(AdminError::not_found)
}

/// `GET /runtime/deployments/<name>` and the `:disable`, `:enable` and `:reset` actions.
pub(crate) async fn one(call: &Call, segment: &str) -> Result<Response, AdminError> {
    let (name, action) = match segment.split_once(':') {
        Some((n, a)) => (n, Some(a)),
        None => (segment, None),
    };
    let Some(action) = action else {
        if call.method != Method::GET {
            return Err(AdminError::method_not_allowed("GET"));
        }
        call.allow_params(&[])?;
        call.require("health:read", &[Scope::Gateway])?;
        let d = deployment(call, name)?;
        return Ok(call.json(StatusCode::OK, &view(call, &d)));
    };
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&[])?;
    call.require("health:write", &[Scope::Gateway])?;
    let d = deployment(call, name)?;
    let health = &call.rt.state.health;
    match action {
        "disable" => {
            let body = if call.body.is_empty() {
                serde_json::Map::new()
            } else {
                call.json_body()?
            };
            let until = match body.get("until") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) => Some(
                    UtcMicros::parse_rfc3339(s)
                        .map_err(|_| AdminError::invalid_parameter("until must be an RFC 3339 time"))?,
                ),
                Some(_) => return Err(AdminError::invalid_parameter("until must be an RFC 3339 time")),
            };
            health.disable(name, until, call.rt.now());
            call.rt.state.affinity.forget_deployment(name);
        }
        "enable" => health.enable(name, call.rt.now()),
        "reset" => health.reset(name, &d.provider.name, call.rt.now()),
        _ => return Err(AdminError::not_found()),
    }
    call.rt.persist_disables().await;
    call.rt.publish_health_changes(&call.snap);
    call.audit(
        &format!("deployments.{action}"),
        &format!("runtime/deployments/{name}"),
        Outcome::Success,
        StatusCode::OK,
    )
    .await;
    Ok(call.json(StatusCode::OK, &view(call, &d)))
}

/// `POST /runtime/providers/<provider>/keys/<key>:reset`.
pub(crate) async fn reset_key(call: &Call, provider: &str, action: &str) -> Result<Response, AdminError> {
    let Some(key) = action.strip_suffix(":reset") else {
        return Err(AdminError::not_found());
    };
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&[])?;
    call.require("health:write", &[Scope::Gateway])?;
    let p = call
        .snap
        .providers
        .providers
        .get(provider)
        .ok_or_else(AdminError::not_found)?;
    if !p.keys.iter().any(|k| k.name == key) {
        return Err(AdminError::not_found());
    }
    let deployments: Vec<&str> = call
        .snap
        .providers
        .deployments
        .values()
        .filter(|d| d.provider.name == provider)
        .map(|d| d.name.as_str())
        .collect();
    call.rt
        .state
        .health
        .reset_key(provider, key, &deployments, call.rt.now());
    call.audit(
        "keys.reset",
        &format!("runtime/providers/{provider}/keys/{key}"),
        Outcome::Success,
        StatusCode::OK,
    )
    .await;
    Ok(call.no_content())
}

/// `DELETE /routes/<route>/affinity`: ends every session binding of the route, so its sessions
/// are balanced afresh on their next request.
pub(crate) async fn route_affinity(call: &Call, route: &str) -> Result<Response, AdminError> {
    if call.method != Method::DELETE {
        return Err(AdminError::method_not_allowed("DELETE"));
    }
    call.allow_params(&[])?;
    call.require("models:write", &[Scope::Gateway])?;
    if call.snap.routing.route(route).is_none() {
        return Err(AdminError::not_found());
    }
    let deleted = call
        .rt
        .affinity_unbind_route(route)
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    call.audit_with(
        "routes.affinity.delete",
        &format!("routes/{route}/affinity"),
        Outcome::Success,
        StatusCode::OK,
        json!({"deleted": deleted}),
    )
    .await;
    Ok(call.json(StatusCode::OK, &json!({"deleted": deleted})))
}

/// `GET /routes/<route>/state`: each composite node's children with their selection state on
/// this node.
pub(crate) fn route_state(call: &Call, route: &str) -> Result<Response, AdminError> {
    use antirion_router::NodeKind;
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&["limit", "cursor"])?;
    call.require("models:read", &[Scope::Gateway])?;
    let r = call
        .snap
        .routing
        .route(route)
        .cloned()
        .ok_or_else(AdminError::not_found)?;
    let tree = &r.tree;
    let now = call.rt.now();
    let mono = std::time::Instant::now();
    let state = &call.rt.state.routing;
    let leaves_under = |i: usize| -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![i];
        while let Some(n) = stack.pop() {
            let Some(node) = tree.node(n) else { continue };
            if let NodeKind::Deployment(d) = &node.kind {
                out.push(d.clone());
            }
            stack.extend(node.children.iter().copied());
        }
        out
    };
    let healthy = |d: &str| -> bool {
        call.snap.providers.deployments.get(d).is_some_and(|dep| {
            dep.usable()
                && call
                    .rt
                    .state
                    .health
                    .status(
                        d,
                        &dep.provider.name,
                        &keys_of(dep),
                        call.snap.health.for_deployment(d),
                        now,
                    )
                    .is_ok()
        })
    };
    let mut items = Vec::new();
    for node in &tree.nodes {
        if (node.kind.is_leaf() || node.children.is_empty()) && node.mirror.is_none() {
            continue;
        }
        let decay = match &node.kind {
            NodeKind::LeastLatency { decay } => *decay,
            _ => std::time::Duration::from_secs(10),
        };
        let swrr = state.swrr_state(&node.state_key);
        let children: Vec<Value> = node
            .children
            .iter()
            .enumerate()
            .filter_map(|(k, c)| tree.node(*c).map(|n| (k, *c, n)))
            .map(|(k, c, n)| {
                let leaves = leaves_under(c);
                let eligible = leaves.iter().any(|d| healthy(d));
                let segment = match &n.kind {
                    NodeKind::Deployment(d) => d.clone(),
                    _ => n.segment.clone().unwrap_or_default(),
                };
                let first = leaves.first().map(String::as_str).unwrap_or_default();
                let stats = state.view(first, decay, mono);
                let latency: serde_json::Map<String, Value> = stats
                    .latency_ms
                    .iter()
                    .map(|(slot, v)| {
                        let prior = call
                            .snap
                            .routing
                            .settings
                            .default_estimate
                            .get(slot_index(*slot))
                            .copied();
                        (
                            slot.as_str().to_owned(),
                            v.or(prior).map_or(Value::Null, Value::from),
                        )
                    })
                    .collect();
                let rule = match &node.kind {
                    NodeKind::Conditional { rules, .. } => rules
                        .get(k)
                        .map(|r| json!({"index": k, "name": r.name, "enabled": r.enabled})),
                    _ => None,
                };
                json!({
                    "segment": segment,
                    "rule": rule,
                    "eligible": eligible,
                    "reason": if eligible { Value::Null } else { Value::from("health") },
                    "weight": n.weight,
                    "swrr_current": swrr.get(k).copied().flatten(),
                    "latency_ms": latency,
                    "success": stats.success,
                    "inflight": leaves.iter().map(|d| state.inflight(d)).sum::<u32>(),
                    "price_estimate": Value::Null,
                })
            })
            .collect();
        let mut item = json!({
            "path": node.path,
            "type": node.kind.type_name(),
            "children": children,
            "replica": call.rt.node.id,
            "affinity_bindings": call.rt.state.affinity.count(&tree.name, now),
        });
        if let (NodeKind::Weighted { canary: Some(c), .. }, Some(o)) = (&node.kind, item.as_object_mut()) {
            let v = state.canaries.view(c, now);
            let child = node
                .children
                .get(c.child)
                .and_then(|i| tree.node(*i))
                .and_then(|n| n.segment.clone());
            o.insert(
                "canary".into(),
                json!({"child": child, "state": v.state, "weight": v.weight, "error_rate": v.error_rate}),
            );
        }
        if let (NodeKind::Pool(spec), Some(o)) = (&node.kind, item.as_object_mut())
            && let Some(p) = &spec.picker
        {
            o.insert("picker".into(), picker_state(p, &state.pickers.view(&node.path)));
        }
        if let (Some(_), Some(o)) = (&node.mirror, item.as_object_mut()) {
            let v = state.shadows.view(&node.path, now);
            o.insert(
                "mirror".into(),
                json!({
                    "comparisons": v.comparisons,
                    "divergence_rate": v.divergence_rate,
                    "latency_delta_p50_ms": v.latency_delta_p50_ms,
                    "cost_delta_usd": format!("{:.9}", v.cost_delta_usd),
                }),
            );
        }
        items.push(item);
    }
    // What the route does with a request its root's rules do not match, and the Models that send
    // requests through it at all: a route no Model names handles nothing.
    let unmatched = match tree.node(0).map(|n| &n.kind) {
        Some(NodeKind::Conditional {
            has_default: false, ..
        }) => "error",
        Some(NodeKind::Conditional {
            has_default: true, ..
        }) => "default",
        _ => "none",
    };
    let mut models: Vec<&str> = call
        .snap
        .config
        .doc
        .get("models")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(_, m)| m.get("route").and_then(Value::as_str) == Some(route))
        .map(|(name, _)| name.as_str())
        .collect();
    models.sort_unstable();
    Ok(call.json(
        StatusCode::OK,
        &json!({"items": items, "next_cursor": null, "unmatched": unmatched, "models": models}),
    ))
}

/// A pool's external picker on the answering replica: its address, how its last ask ended, and
/// what it last chose.
fn picker_state(p: &antirion_router::pool::Picker, v: &antirion_router::pickers::PickerView) -> Value {
    json!({
        "url": p.url,
        "timeout": if p.timeout.subsec_nanos() == 0 {
            antirion_config::scalar::format_duration(p.timeout)
        } else {
            format!("{}ms", p.timeout.as_millis())
        },
        "on_error": if p.fail_open { "builtin" } else { "reject" },
        "state": v.state(),
        "asks": v.asks,
        "failures": v.failures,
        "last_asked_at": v.last.as_ref().map(|(at, _)| at.to_rfc3339()),
        "last_pick": v.last_pick.as_ref().map(|(at, chosen)| json!({
            "at": at.to_rfc3339(),
            "deployments": chosen,
            "served": v.last_served,
        })),
        "last_failure": v.last_failure.as_ref().map(|(at, why)| json!({"at": at.to_rfc3339(), "reason": why})),
    })
}

fn slot_index(s: antirion_router::Slot) -> usize {
    match s {
        antirion_router::Slot::Ttft => 0,
        antirion_router::Slot::PerToken => 1,
        antirion_router::Slot::E2e => 2,
    }
}
