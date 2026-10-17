//! Active health checks: with `active_checks.enabled`, each deployment (with `when: open_only`,
//! each one whose breaker is not closed) gets a check every `interval`: `GET <base>/models`, or a
//! generation of one output token, with its first usable key and its own timeout. A success counts
//! as a probe success, a failure as a failure. A manually disabled deployment, and one without a
//! usable key, is not checked. Checks carry `x-request-id: antirion-healthcheck-<uuid>` and are
//! no client's requests: nothing is billed or logged for them.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use antirion_core::RequestId;
use antirion_egress::Payload;
use antirion_providers::{DeploymentRt, ProviderType, UpstreamCall, UpstreamOp};
use antirion_resilience::{BreakerEffect, CheckRequest, KeyInfo};
use http::{HeaderValue, Method};
use serde_json::json;
use tokio::task::JoinSet;
use tokio::time::Instant;

use crate::Runtime;

/// How often the task looks for checks that are due; the shortest `interval` allowed.
const TICK: Duration = Duration::from_millis(100);

/// Runs the active checks of every node until it stops.
pub async fn run(rt: Arc<Runtime>) {
    let mut running: JoinSet<(String, Option<bool>)> = JoinSet::new();
    let mut busy: HashSet<String> = HashSet::new();
    let mut last: HashMap<String, Instant> = HashMap::new();
    loop {
        tokio::select! {
            () = rt.shutdown.cancelled() => return,
            Some(done) = running.join_next(), if !running.is_empty() => {
                if let Ok((name, verdict)) = done {
                    busy.remove(&name);
                    if let Some(ok) = verdict {
                        let snap = rt.gens.load();
                        let s = snap.health.for_deployment(&name);
                        rt.state.health.checked(&name, ok, s, rt.now());
                        rt.publish_health_changes(&snap);
                    }
                }
                continue;
            }
            () = tokio::time::sleep(TICK) => {}
        }
        let snap = rt.gens.load();
        if !snap.health.checks_enabled() {
            last.clear();
            continue;
        }
        let now = Instant::now();
        for d in snap.providers.deployments.values() {
            let s = snap.health.for_deployment(&d.name);
            if !s.checks.enabled || !d.usable() || busy.contains(&d.name) {
                continue;
            }
            if last.get(&d.name).is_some_and(|t| now < *t + s.checks.interval) {
                continue;
            }
            let state = rt.state.health.state(&d.name, s, rt.now());
            if state == "disabled" || (s.checks.open_only && state == "closed") {
                continue;
            }
            let keys: Vec<KeyInfo> = d
                .provider
                .keys
                .iter()
                .map(|k| KeyInfo {
                    name: k.name.clone(),
                    weight: k.weight,
                })
                .collect();
            let key = if keys.is_empty() {
                None
            } else {
                match rt
                    .state
                    .health
                    .usable_key(&d.name, &d.provider.name, &keys, rt.now())
                {
                    Some(k) => Some(k),
                    None => continue,
                }
            };
            last.insert(d.name.clone(), now);
            busy.insert(d.name.clone());
            let (rt, d, request, timeout) = (
                Arc::clone(&rt),
                Arc::clone(d),
                snap.health.check_request,
                s.checks.timeout,
            );
            running.spawn(async move {
                let verdict = tokio::time::timeout(timeout, check(&rt, &d, key.as_deref(), request, timeout))
                    .await
                    .unwrap_or(Some(false));
                (d.name.clone(), verdict)
            });
        }
    }
}

/// The request of a check: its operation, method and body.
fn shape(
    d: &DeploymentRt,
    request: CheckRequest,
) -> (UpstreamOp<'static>, Method, Option<serde_json::Value>) {
    let anthropic = d.provider.kind == ProviderType::Anthropic;
    match (request, d.provider.kind) {
        (CheckRequest::MinimalGeneration, ProviderType::Anthropic) => (
            UpstreamOp::Messages,
            Method::POST,
            Some(
                json!({"model": d.model, "max_tokens": 1, "messages": [{"role": "user", "content": "ping"}]}),
            ),
        ),
        (
            CheckRequest::MinimalGeneration,
            ProviderType::OpenAi | ProviderType::AzureOpenAi | ProviderType::OpenAiCompatible,
        ) => (
            UpstreamOp::OpenAi {
                class: "generate",
                path: "/chat/completions",
            },
            Method::POST,
            Some(
                json!({"model": d.model, "max_tokens": 1, "messages": [{"role": "user", "content": "ping"}]}),
            ),
        ),
        _ => (
            UpstreamOp::OpenAi {
                class: "models",
                path: if anthropic { "/v1/models" } else { "/models" },
            },
            Method::GET,
            None,
        ),
    }
}

/// Sends one check: `Some(true)` for a success, `Some(false)` for a verdict the breaker counts as a
/// failure, `None` for a neutral one.
async fn check(
    rt: &Runtime,
    d: &DeploymentRt,
    key: Option<&str>,
    request: CheckRequest,
    timeout: Duration,
) -> Option<bool> {
    let id = RequestId::from_client(Some(
        format!("antirion-healthcheck-{}", RequestId::generate().as_str()).as_bytes(),
    ));
    let (op, method, body) = shape(d, request);
    let payload = body.map_or_else(
        || Payload::from(Vec::new()),
        |b| Payload::from(serde_json::to_vec(&b).unwrap_or_default()),
    );
    let Ok(prepared) = d.build_request(UpstreamCall {
        op,
        stream: false,
        body: payload,
        content_type: (method == Method::POST).then(|| HeaderValue::from_static("application/json")),
        request_id: &id,
        response_timeout: timeout,
        connect_timeout: Some(timeout),
        anthropic: None,
        key,
        byok: None,
    }) else {
        return None;
    };
    let mut req = prepared.request;
    req.method = method;
    req.resend_when_closed = false;
    let snap = rt.gens.load();
    let tls = snap
        .security
        .egress_tls
        .clone()
        .unwrap_or_else(|| d.provider.tls.clone());
    if d.authorize(&mut req, &rt.creds, &tls).await.is_err() {
        return None;
    }
    let class = match rt.egress.send(req).await {
        Ok(resp) => antirion_resilience::classify::status(resp.status.as_u16(), false),
        Err(e) => antirion_resilience::classify::transport(&e),
    };
    match class.breaker() {
        BreakerEffect::Ok => Some(true),
        BreakerEffect::Fail => Some(false),
        BreakerEffect::Neutral => None,
    }
}
