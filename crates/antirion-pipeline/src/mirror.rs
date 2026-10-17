//! Shadow traffic: when the plan's first position is chosen, each node above it with a `mirror`
//! sends a sampled copy of the request to the first eligible position of its target, once the
//! primary's first attempt is dispatched. A copy runs on a tracked task, is read whole and discarded,
//! is never retried and never changes the client's response; it is settled as a `shadow` ledger line.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use antirion_core::SplitMix64;
use antirion_egress::{InboundBody, OutboundRequest};
use antirion_obs::Fam;
use antirion_providers::{DeploymentRt, UpstreamCall};
use antirion_resilience::{AttemptTimers, KeyMode, KeyPick};
use antirion_router::body_edit::BodyEdit;
use antirion_router::{AttemptPlan, MirrorSlot, Params};
use antirion_runtime::{Runtime, Snapshot};
use bytes::{Bytes, BytesMut};
use http_body_util::BodyExt;
use serde_json::Value;

use crate::account::{ShadowCall, ShadowFacts, settle_shadow};
use crate::attempt::{Upstream, edited_body, with_params};
use crate::cell::{cell, upstream_op};
use crate::reqlog::Usage;
use crate::route::{Tiers, pick_key};
use crate::upbody::{client_anthropic, upstream_body};

/// Ledger line numbers of copies start here, beyond any request's own attempts.
const FIRST_LINE: u32 = 1000;
/// The bytes of a copy's answer kept to read its usage; the rest is read and dropped.
const KEPT_BYTES: usize = 16 << 20;
/// The bound on one copy when the policy sets no attempt or stream timeout.
const DEFAULT_BOUND: Duration = Duration::from_secs(600);

/// A copy to send: the target's position, its own route params, and its in-flight slot.
#[derive(Debug)]
pub(crate) struct Job {
    deployment: Arc<DeploymentRt>,
    params: Vec<Params>,
    body_edit: Option<Arc<BodyEdit>>,
    slot: MirrorSlot,
    route: String,
    /// The mirroring node's path, which its comparisons are recorded under.
    node: String,
}

/// The copies a request sends, planned with its first position and taken by its first attempt.
pub(crate) type Jobs = Arc<Mutex<Vec<Job>>>;

/// Whether `key` falls in the sampled `fraction` of node `node`.
fn sampled(key: &str, node: &str, fraction: f64) -> bool {
    if fraction >= 1.0 {
        return true;
    }
    if fraction <= 0.0 {
        return false;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in node.bytes().chain([0]).chain(key.bytes()) {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    let u = (SplitMix64::new(h).next_u64() >> 11) as f64 / (1u64 << 53) as f64;
    u < fraction
}

/// Plans the copies of the nodes above the first position `node`, from the root down.
pub(crate) fn plan(t: &mut Tiers<'_>, node: usize) {
    let cx = t.filters.cx;
    let route = t.route;
    let mut jobs = Vec::new();
    for i in route.tree.ancestry(node) {
        let Some(n) = route.tree.node(i) else {
            continue;
        };
        let Some(m) = n.mirror.clone() else {
            continue;
        };
        let key = if m.by_session { t.session.clone() } else { None }
            .unwrap_or_else(|| cx.request_id.as_str().to_owned());
        if !sampled(&key, &n.path, m.fraction) {
            continue;
        }
        let Some(slot) = cx.rt.state.routing.begin_mirror(&n.state_key, m.max_in_flight) else {
            cx.rt
                .metrics
                .add(Fam::ShadowRequests, &[&route.tree.name, "dropped"], 1.0);
            continue;
        };
        let tried = std::mem::take(&mut t.filters.tried);
        let seed = cx.rt.with_rng(|r| r.next_u64());
        let step = AttemptPlan::new(&m.target, seed).next(&t.sel, &mut t.filters);
        t.filters.tried = tried;
        let Some(step) = step else {
            continue;
        };
        let Some(deployment) = t.filters.deployment(&step.deployment) else {
            continue;
        };
        let params = m
            .target
            .ancestry(step.node)
            .into_iter()
            .filter_map(|i| m.target.node(i).and_then(|n| n.params.clone()))
            .collect();
        let body_edit = m.target.node(step.node).and_then(|n| n.body_edit.clone());
        jobs.push(Job {
            deployment,
            params,
            body_edit,
            slot,
            route: route.tree.name.clone(),
            node: n.path.clone(),
        });
    }
    if let Ok(mut j) = t.mirrors.lock() {
        *j = jobs;
    }
}

/// Starts the request's planned copies; later attempts find none.
pub(crate) async fn launch(up: &Upstream<'_>, timers: &AttemptTimers) {
    let jobs = match up.mirrors.lock() {
        Ok(mut j) => std::mem::take(&mut *j),
        Err(_) => return,
    };
    for (i, job) in jobs.into_iter().enumerate() {
        let line = FIRST_LINE.saturating_add(u32::try_from(i).unwrap_or(u32::MAX));
        start(up, job, line, timers).await;
    }
}

async fn start(up: &Upstream<'_>, job: Job, line: u32, timers: &AttemptTimers) {
    let (cx, plan) = (up.cx, up.plan);
    let Some(facts) = cx.account.shadow_facts() else {
        return;
    };
    let d = &job.deployment;
    let edited = with_params(plan.parsed, &job.params);
    let parsed = edited.as_ref().unwrap_or(plan.parsed);
    let Some(c) = cell(plan.endpoint, parsed, d) else {
        return;
    };
    let Ok((body, content_type, _)) = upstream_body(cx, plan, parsed, d, c, edited.is_some(), false).await
    else {
        return;
    };
    let body = match &job.body_edit {
        Some(edit) => edited_body(body, edit),
        None => body,
    };
    let key = match pick_key(cx, d, &[], None, KeyMode::Untried) {
        KeyPick::Key(k) => Some(k),
        KeyPick::Keyless => None,
        KeyPick::None => return,
    };
    let anthropic = client_anthropic(plan);
    let bound = if plan.stream {
        timers.timeouts.stream_total
    } else {
        timers.timeouts.attempt
    }
    .unwrap_or(DEFAULT_BOUND);
    let prepared = d.build_request(UpstreamCall {
        op: upstream_op(plan.endpoint, c),
        stream: plan.stream,
        body,
        content_type,
        request_id: &cx.request_id,
        response_timeout: bound,
        connect_timeout: Some(timers.connect),
        anthropic: anthropic.as_ref(),
        key: key.as_deref(),
        byok: None,
    });
    let Ok(prepared) = prepared else {
        return;
    };
    let mut request = prepared.request;
    request.resend_when_closed = false;
    crate::egress::outbound(
        cx,
        plan.client_headers,
        &plan.rules,
        plan.stream,
        &mut request.headers,
    );
    for (n, v) in &plan.extra_headers {
        request.headers.insert(n.clone(), v.clone());
    }
    let pair = crate::compare::Pair::new(Arc::clone(&cx.rt), job.node.clone());
    cx.log.update(|r| r.comparisons.push(Arc::clone(&pair)));
    let copy = Copy {
        rt: Arc::clone(&cx.rt),
        snap: Arc::clone(&cx.snap),
        deployment: Arc::clone(d),
        facts,
        line,
        e_in: plan.input_estimate,
        slot: job.slot,
        pair,
    };
    cx.rt.metrics.add(Fam::ShadowRequests, &[&job.route, "sent"], 1.0);
    let shutdown = cx.rt.shutdown.clone();
    cx.rt.tasks.spawn(async move {
        tokio::select! {
            () = shutdown.cancelled() => {}
            _ = tokio::time::timeout(bound, copy.run(request)) => {}
        }
    });
}

/// One copy in flight, with what its settlement needs.
struct Copy {
    rt: Arc<Runtime>,
    snap: Arc<Snapshot>,
    deployment: Arc<DeploymentRt>,
    facts: ShadowFacts,
    line: u32,
    e_in: u64,
    slot: MirrorSlot,
    pair: Arc<crate::compare::Pair>,
}

impl Copy {
    async fn run(self, mut request: OutboundRequest) {
        let d = &self.deployment;
        let tls = self
            .snap
            .security
            .egress_tls
            .clone()
            .unwrap_or_else(|| d.provider.tls.clone());
        if d.authorize(&mut request, &self.rt.creds, &tls).await.is_err() {
            return;
        }
        let sent = std::time::Instant::now();
        let (answered, usage, raw, said) = match self.rt.egress.send(request).await {
            Ok(resp) => {
                let ok = resp.status.is_success();
                let body = drain(resp.body).await;
                let (usage, raw) = usage_of(&body);
                let said = if ok {
                    crate::compare::answer(&body)
                } else {
                    (Some("error".to_owned()), None)
                };
                (ok, usage, raw, said)
            }
            Err(_) => (false, None, None, (Some("error".to_owned()), None)),
        };
        let latency_ms = sent.elapsed().as_secs_f64() * 1000.0;
        let call = ShadowCall {
            deployment: d,
            line: self.line,
            answered,
            usage,
            raw,
            e_in: self.e_in,
            purpose: antirion_limits::ledger::Purpose::Shadow,
        };
        let cost = settle_shadow(&self.rt, &self.snap, &self.facts, &call).await;
        #[allow(clippy::cast_precision_loss)]
        self.pair.shadow(crate::compare::Side {
            finish: said.0,
            text: said.1,
            latency_ms,
            cost_usd: cost.map_or(0.0, |n| n.0 as f64 / 1e9),
        });
        drop(self.slot);
    }
}

/// Reads a body to its end, keeping its first bytes.
async fn drain(mut body: InboundBody) -> Bytes {
    let mut kept = BytesMut::new();
    while let Some(frame) = body.frame().await {
        let Ok(frame) = frame else { break };
        if let Ok(data) = frame.into_data()
            && kept.len() < KEPT_BYTES
        {
            let room = KEPT_BYTES - kept.len();
            kept.extend_from_slice(data.get(..room.min(data.len())).unwrap_or_default());
        }
    }
    kept.freeze()
}

/// The usage an answer reports: a JSON body, or the merged reports of an SSE stream's events.
fn usage_of(body: &[u8]) -> (Option<Usage>, Option<Value>) {
    if let Ok(v) = serde_json::from_slice::<Value>(body) {
        let u = crate::apis::usage_of(&v);
        let raw = v.get("usage").or_else(|| v.get("usageMetadata")).cloned();
        return ((!u.is_empty()).then_some(u), raw);
    }
    let mut merged: Option<Usage> = None;
    let mut raw = None;
    for line in body.split(|b| *b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let Some(data) = line.strip_prefix(b"data:") else {
            continue;
        };
        let Ok(v) = serde_json::from_slice::<Value>(data.trim_ascii()) else {
            continue;
        };
        for event in [Some(&v), v.get("message"), v.get("response")]
            .into_iter()
            .flatten()
        {
            let u = crate::apis::usage_of(event);
            if !u.is_empty() {
                raw = event.get("usage").or_else(|| event.get("usageMetadata")).cloned();
                merged.get_or_insert_with(Usage::default).merge(u);
            }
        }
    }
    (merged, raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampling_is_exact_at_the_ends_and_proportional_between() {
        assert!(sampled("r", "n", 1.0));
        assert!(!sampled("r", "n", 0.0));
        let hits = (0..2000)
            .filter(|i| sampled(&format!("req-{i}"), "n", 0.2))
            .count();
        assert!((300..500).contains(&hits), "{hits}");
        assert_eq!(sampled("same", "n", 0.5), sampled("same", "n", 0.5));
    }

    #[test]
    fn usage_is_read_from_json_and_from_streams() {
        let (u, raw) = usage_of(br#"{"choices":[],"usage":{"prompt_tokens":1,"completion_tokens":5}}"#);
        let u = u.unwrap();
        assert_eq!((u.input, u.output), (Some(1), Some(5)));
        assert!(raw.is_some());
        let sse = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":1}}}\n\n\
event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":9}}\n\n";
        let (u, _) = usage_of(sse);
        let u = u.unwrap();
        assert_eq!((u.input, u.output), (Some(7), Some(9)));
        assert!(usage_of(b"not json").0.is_none());
    }
}
