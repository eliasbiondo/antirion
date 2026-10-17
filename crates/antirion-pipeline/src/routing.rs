//! Stages `route` and `attempt` of one request: the attempt plan of the resolved route (pins and
//! session affinity included) walked by the attempt loop, then the routing facts of the response
//! (route path, affinity outcome, skipped candidates) and the binding written on success.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use antirion_core::headers as h;
use antirion_dialects::Dialect;
use antirion_error::GatewayError;
use antirion_providers::DeploymentRt;
use antirion_resilience::{LoopCx, Outcome, Policy, RequestKind};
use antirion_router::tags::RequestTags;
use antirion_router::{
    AttemptPlan, Captures, CompiledRoute, NodeKind, Params, RouteRt, SelectCx, Slot, binding_key,
};
use antirion_runtime::cluster::Lookup;
use serde_json::{Map, Value, json};

use crate::attempt::{Done, Plan, Upstream};
use crate::authorize;
use crate::controls::PIN_DEPLOYMENT;
use crate::ctx::Ctx;
use crate::fallbacks::{self, Fallbacks};
use crate::infer::Request;
use crate::route::{
    Filters, Position, Prices, RequestFacts, Tiers, leaf_by_path, pinned_leaf, session_value,
};

fn dialect_name(d: Dialect) -> &'static str {
    match d {
        Dialect::AnthropicMessages => "anthropic",
        Dialect::Gemini => "gemini",
        Dialect::BedrockConverse | Dialect::BedrockInvoke => "bedrock",
        _ => "openai",
    }
}

pub(crate) fn facts<'a>(req: &Request<'a>, sample: (&'a str, u64)) -> RequestFacts<'a> {
    let key = &req.auth.key;
    let body = req.parsed.doc();
    let member = |name: &str| body.and_then(|b| b.get(name));
    let messages: Vec<&Value> = member("messages")
        .or_else(|| member("input"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect();
    let part_types: Vec<&str> = messages
        .iter()
        .filter_map(|m| m.get("content").and_then(Value::as_array))
        .flatten()
        .filter_map(|p| p.get("type").and_then(Value::as_str))
        .collect();
    let has = |types: &[&str]| part_types.iter().any(|t| types.contains(t));
    let mut request = Map::new();
    request.insert("endpoint".into(), json!(req.endpoint.id));
    request.insert("dialect".into(), json!(dialect_name(req.endpoint.dialect)));
    request.insert("path".into(), json!(req.parts.uri.path()));
    request.insert("stream".into(), json!(req.parsed.stream()));
    request.insert("input_tokens".into(), json!(req.sizes.input));
    if let Some(o) = req.sizes.output {
        request.insert("max_output_tokens".into(), json!(o));
    }
    request.insert(
        "has_tools".into(),
        json!(
            member("tools")
                .and_then(Value::as_array)
                .is_some_and(|t| !t.is_empty())
        ),
    );
    request.insert(
        "has_images".into(),
        json!(has(&["image_url", "image", "input_image"])),
    );
    request.insert("has_audio".into(), json!(has(&["input_audio", "audio"])));
    request.insert("model".into(), json!(req.requested));
    let mut identity = Map::new();
    identity.insert("organization".into(), json!(key.organization));
    identity.insert("team".into(), json!(key.team));
    identity.insert("project".into(), json!(key.project));
    identity.insert("key".into(), json!(key.name));
    if let Some(u) = req.end_user {
        identity.insert("end_user".into(), json!(u));
    }
    identity.insert("tags".into(), json!(req.tags));
    RequestFacts {
        headers: &req.parts.headers,
        body,
        metadata: req.auth.controls.metadata.clone(),
        request,
        identity,
        session: req.auth.controls.session.clone(),
        sample: Some(sample),
        guards: None,
    }
}

pub(crate) fn slot(req: &Request<'_>) -> Slot {
    if req.parsed.stream() {
        Slot::Ttft
    } else if matches!(
        req.endpoint.id,
        "chat_completions" | "completions" | "responses" | "messages"
    ) {
        Slot::PerToken
    } else {
        Slot::E2e
    }
}

const PREVIOUS_RESPONSE_ID: &str = "previous_response_id";

/// Routes and attempts the request.
pub(crate) async fn run(
    cx: &Ctx,
    plan: &Plan<'_>,
    req: &Request<'_>,
    route: &Arc<RouteRt>,
    alias: Option<Params>,
    captures: Option<&Captures>,
    direct: Option<Arc<DeploymentRt>>,
) -> Result<Done, GatewayError> {
    let snap = Arc::clone(&cx.snap);
    let rt = Arc::clone(&cx.rt);
    let key = &req.auth.key;
    let controls = &req.auth.controls;
    let levels = snap.identity.access_levels(key);
    let mut facts = facts(req, (cx.request_id.as_str(), snap.routing.settings.hash_seed));
    if route.reads_guards {
        let results = Arc::new(crate::guardsel::GuardResults::default());
        facts.guards = Some(Arc::clone(&results));
        Box::pin(crate::guardsel::run(cx, route, &facts, &results)).await;
    }
    cx.log.update(|r| r.routed = true);

    let test_pin = rt
        .test
        .as_ref()
        .and_then(|_| req.parts.headers.get(h::TEST_DEPLOYMENT))
        .and_then(|v| v.to_str().ok())
        .map(|p| (p, h::TEST_DEPLOYMENT));
    let continuation = plan.turn.as_ref().and_then(|t| t.pin.clone());
    let pin = continuation
        .as_deref()
        .map(|d| (d, PREVIOUS_RESPONSE_ID))
        .or(test_pin)
        .or_else(|| controls.pin.as_deref().map(|p| (p, PIN_DEPLOYMENT)));
    let seed = rt.with_rng(|r| r.next_u64());
    let mut attempt_plan = match pin {
        Some((p, param)) => {
            let node = match pinned_leaf(route, p, param) {
                Ok(n) => n,
                Err(_) if continuation.is_some() => return Err(crate::respstate::affinity_unavailable()),
                Err(e) => return Err(e),
            };
            let usable = snap
                .providers
                .deployments
                .get(p)
                .is_some_and(|d| authorize::deployment_allowed(&levels, d));
            if !usable {
                return Err(authorize::no_allowed_deployment());
            }
            AttemptPlan::pinned(&route.tree, node)
        }
        None => AttemptPlan::new(&route.tree, seed),
    };
    let mut filters = Filters {
        cx,
        snap: &snap,
        endpoint: req.endpoint,
        parsed: req.parsed,
        native_only: req.native_only,
        xlate: plan.xlate.as_deref(),
        verdicts: req.verdicts.clone(),
        needs: req.needs,
        sizes: req.sizes,
        levels,
        model: req.model_name.to_owned(),
        tried: Vec::new(),
        synthetic: HashMap::new(),
        ignore_health: Vec::new(),
        client_keys: plan.client_keys.as_ref(),
        tagged: tagged(route, req.tags),
    };
    if let Some(d) = &direct {
        filters
            .synthetic
            .insert(format!("{}/{}", d.provider.name, d.model), Arc::clone(d));
    }
    let settings = &snap.routing.settings;
    let sticky = |s: &str| facts.sticky(s);
    let output = req.sizes.output.unwrap_or(settings.cost_output_estimate);
    let prices = Prices {
        snap: &snap,
        input: req.sizes.input as f64 / 1e6,
        output: output as f64 / 1e6,
    };
    let sel = SelectCx {
        state: &rt.state.routing,
        facts: &facts,
        cost: &prices,
        slot: slot(req),
        default_estimate: settings.default_estimate,
        hash_seed: settings.hash_seed,
        sticky: &sticky,
        captures,
        now: rt.now(),
        dry: false,
    };

    let mut pickers = Vec::new();
    if pin.is_none() && route.decides_ahead {
        let session = session_value(&route.affinity.sources, &facts, controls.session.as_deref());
        pickers = crate::decide::take(
            cx,
            req,
            route,
            &mut attempt_plan,
            &sel,
            session.as_ref().map(|(_, v)| v.as_str()),
        )
        .await?;
    }

    let now = rt.now();
    let aff = &route.affinity;
    let mut strict = false;
    let mut binding_key_value: Option<String> = None;
    let mut bound_path: Option<String> = None;
    let affinity_on = (aff.session || aff.prompt_prefix) && direct.is_none();
    let mut outcome_label: Option<&'static str> = None;
    if pin.is_some() {
        outcome_label = Some("pinned");
    } else if affinity_on {
        if controls.affinity_off {
            outcome_label = Some("off");
        } else if let Some((source, value)) = aff
            .session
            .then(|| session_value(&aff.sources, &facts, controls.session.as_deref()))
            .flatten()
            .or_else(|| {
                aff.prompt_prefix
                    .then(|| facts.body.and_then(antirion_router::prefix_key::value))
                    .flatten()
                    .map(|v| ("prompt_prefix".to_owned(), v))
            })
        {
            let prefix = source == "prompt_prefix";
            let scope: Vec<&str> = match aff.scope.as_str() {
                "key" => vec![key.name.as_str()],
                "organization" => vec![key.organization.as_str()],
                _ => vec![key.organization.as_str(), key.project.as_str()],
            };
            let k = binding_key(&scope, &route.tree.name, &source, &value);
            match rt.affinity_lookup(&k, now, settings.affinity_max_entries).await {
                Lookup::Bound(b) => {
                    let node = leaf_by_path(route, &b.route_path);
                    let placed = node.is_some_and(|n| attempt_plan.prefer(n, &sel, &mut filters));
                    if aff.strict && !prefix {
                        let (Some(node), true) = (node, placed) else {
                            return Err(strict_unavailable(cx, &b.deployment));
                        };
                        attempt_plan = AttemptPlan::pinned(&route.tree, node);
                        strict = true;
                    }
                    bound_path = Some(b.route_path);
                    binding_key_value = Some(k);
                }
                Lookup::Unbound => binding_key_value = Some(k),
                Lookup::Unavailable => {
                    cx.guard.degraded("affinity");
                    cx.log.update(|r| {
                        if !r.degraded.iter().any(|(c, _)| *c == "affinity") {
                            r.degraded.push(("affinity", Some("store_unavailable")));
                        }
                    });
                }
            }
        } else {
            outcome_label = Some("none");
        }
    }

    let base = Policy::from_value(&req.resilience);
    let class_names = fallbacks::class_names(&req.resilience, route);
    let fallbacks = Fallbacks::new(&snap, key, &controls.fallbacks, &class_names);
    let ignore_health = if pin.is_none() && !strict {
        all_unhealthy(&snap, route, &mut filters)
    } else {
        Vec::new()
    };
    if !ignore_health.is_empty() {
        cx.guard.degraded("routing");
        cx.log
            .update(|r| r.degraded.push(("routing", Some("all_unhealthy"))));
    }
    filters.ignore_health.clone_from(&ignore_health);
    let positions = Arc::new(Mutex::new(HashMap::<String, Position>::new()));
    let mirrors: crate::mirror::Jobs = Arc::default();
    let decay = route
        .tree
        .nodes
        .iter()
        .find_map(|n| match &n.kind {
            NodeKind::LeastLatency { decay } => Some(*decay),
            _ => None,
        })
        .unwrap_or(Duration::from_secs(10));
    let mut tiers = Tiers {
        plan: attempt_plan,
        sel,
        filters,
        positions: Arc::clone(&positions),
        route,
        alias,
        base: req.resilience.clone(),
        max_attempts: controls.max_attempts,
        attempts_relaxed: controls.max_attempts_relaxed,
        stash: None,
        tier_first: true,
        last: None,
        policies: HashMap::new(),
        skipped: Vec::new(),
        limit_refused: None,
        visited: Vec::new(),
        session: facts.session.clone(),
        mirrors: Arc::clone(&mirrors),
        mirrored: false,
        fallbacks: &fallbacks,
        pending: fallbacks
            .appended
            .iter()
            .filter_map(|(_, seg)| seg.as_ref().ok())
            .collect(),
        segment: 0,
        entering: None,
        seed,
        tags: req.tags.to_vec(),
        assigned: false,
        causes: Vec::new(),
        blocked: None,
    };
    for (name, seg) in &fallbacks.appended {
        if let Err(reason) = seg {
            tiers.skipped.push((name.clone(), reason));
        }
    }
    let mut upstream = Upstream {
        cx,
        plan,
        positions,
        slot: slot(req),
        decay,
        refused: std::sync::Mutex::default(),
        raced: std::sync::atomic::AtomicBool::new(false),
        mirrors,
        fallbacks: &fallbacks,
        tree: &route.tree,
        canaries: route.has_canary,
    };
    let max_attempts = controls.attempts(base.retry.max_attempts);
    let read = crate::controls::read_timeout(&req.parts.headers);
    let configured = controls.timeout.or(base.timeouts.deadline);
    let deadline = match (configured, read.filter(|_| !plan.stream)) {
        (Some(d), Some(r)) => Some(d.min(r)),
        (d, r) => d.or(r),
    };
    let outcome = antirion_resilience::run(
        &mut tiers,
        &mut upstream,
        LoopCx {
            health: &rt.state.health,
            health_cfg: &snap.health,
            clock: rt.clock.as_ref(),
            cancel: &cx.gone,
            kind: RequestKind::Generation,
            max_attempts,
            min_attempt_time: base.retry.min_attempt_time,
            deadline,
            fallback: base.retry.fallback && !controls.no_fallback && pin.is_none() && !strict,
            seed,
            read_timeout: read.filter(|_| plan.stream),
            ignore_health: &ignore_health,
        },
    )
    .await;
    rt.publish_health_changes(&snap);
    if let Some((block, path)) = tiers.blocked.take() {
        let rules = rule_hits(&route.tree, route.tree.nodes.iter().position(|n| n.path == path));
        cx.log.update(|r| {
            r.route_path = Some(path.clone());
            r.route_rules = rules;
            r.plan = vec![path.clone()];
        });
        return Err(route_block(cx, &block, &path));
    }
    let served = tiers.last_path();
    if !pickers.is_empty() {
        let leaf = tiers.last.filter(|(seg, _)| *seg == 0).map(|(_, n)| n);
        rt.tasks.spawn(async move {
            for p in pickers {
                p.finish(leaf).await;
            }
        });
    }
    let served_rules = rule_hits(
        &route.tree,
        tiers.last.filter(|(seg, _)| *seg == 0).map(|(_, n)| n),
    );
    let success = matches!(outcome, Outcome::Success(_));
    if success && let (Some(k), Some(path)) = (&binding_key_value, &served) {
        let deployment = path.rsplit('/').next().unwrap_or_default();
        outcome_label = Some(match &bound_path {
            Some(b) if b == path => "hit",
            Some(_) => "moved",
            None => "new",
        });
        rt.affinity_bind(
            k,
            (&route.tree.name, path, deployment),
            aff.ttl,
            rt.now(),
            settings.affinity_max_entries,
        );
    }
    let classes = tiers.plan.classes.clone();
    cx.log.update(|r| {
        r.route_path.clone_from(&served);
        r.route_rules = served_rules;
        r.plan.clone_from(&tiers.visited);
        r.skipped.clone_from(&tiers.skipped);
        r.classes = classes;
        if affinity_on || pin.is_some() {
            r.affinity = outcome_label;
        }
    });
    match outcome {
        Outcome::Success(Done::Response(mut resp))
            if upstream.raced.load(std::sync::atomic::Ordering::Relaxed) =>
        {
            let made = cx.log.read(|r| r.attempts.len()).unwrap_or(0);
            if let Ok(v) = http::HeaderValue::from_str(&made.to_string()) {
                resp.headers_mut().insert(antirion_core::headers::ATTEMPTS, v);
            }
            Ok(Done::Response(resp))
        }
        Outcome::Success(done) => Ok(done),
        Outcome::Failed(f) => Err(*f.error),
        Outcome::NoCandidate => Err(tiers.no_candidate(continuation.is_some())),
        Outcome::Cancelled => Ok(Done::Cancelled),
    }
}

/// The leaves of `route` the request's tags allow, when the route filters by tag.
pub(crate) fn tagged(route: &RouteRt, tags: &[String]) -> Option<HashSet<usize>> {
    let default_tag = route.tag_filter.as_deref()?;
    let parsed = RequestTags::parse(tags.iter().map(String::as_str));
    Some(antirion_router::tags::allowed(&route.tree, &parsed, default_tag))
}

/// Records the arm of each experiment above the first position: the child of the experiment node
/// on the position's path.
pub(crate) fn experiment(cx: &Ctx, tree: &CompiledRoute, node: usize) {
    let chain = tree.ancestry(node);
    for pair in chain.windows(2) {
        let [parent, child] = pair else { continue };
        let Some(NodeKind::Weighted {
            experiment: Some(e), ..
        }) = tree.node(*parent).map(|n| &n.kind)
        else {
            continue;
        };
        let Some(arm) = tree.node(*child).and_then(|n| n.segment.clone()) else {
            continue;
        };
        let e = Arc::clone(e);
        cx.log.update(|r| {
            if r.experiment.is_none() {
                r.experiment = Some((e.name.clone(), arm, e.expose));
            }
        });
    }
}

/// A routing decision taken in a fail-open or degraded mode: component `routing`, with its cause.
pub(crate) fn degraded(cx: &Ctx, cause: &'static str) {
    cx.guard.degraded("routing");
    cx.log.update(|r| {
        if !r.degraded.contains(&("routing", Some(cause))) {
            r.degraded.push(("routing", Some(cause)));
        }
    });
}

/// The conditional rules a route position was reached through, as the record shows them.
fn rule_hits(tree: &CompiledRoute, node: Option<usize>) -> Vec<Value> {
    node.map(|n| tree.rules_to(n))
        .unwrap_or_default()
        .into_iter()
        .map(|h| json!({"node": h.node, "index": h.index, "name": h.name}))
        .collect()
}

/// The answer of a `block` node: a guardrail block at stage `route` with the node's status,
/// recorded and counted like a guardrail's block.
pub(crate) fn route_block(cx: &Ctx, block: &antirion_router::tree::Block, path: &str) -> GatewayError {
    let e = block_error(block, path);
    let category = antirion_guardrails::Category::parse(&block.category)
        .unwrap_or(antirion_guardrails::Category::Custom);
    cx.log.update(|r| {
        r.guardrails.push(json!({
            "name": path,
            "stage": "route",
            "verdict": "block",
            "duration_ms": 0.0,
            "failed_open": false,
            "category": category.as_str(),
            "severity": "high",
            "confidence": 1.0,
            "score": Value::Null,
            "spans": [],
        }));
    });
    cx.rt.metrics.add(
        antirion_obs::metrics::Fam::GuardrailChecks,
        &[path, "route", "block"],
        1.0,
    );
    e
}

/// The error a `block` node answers with.
pub(crate) fn block_error(block: &antirion_router::tree::Block, path: &str) -> GatewayError {
    let category = antirion_guardrails::Category::parse(&block.category)
        .unwrap_or(antirion_guardrails::Category::Custom);
    let mut e = GatewayError::new(
        antirion_error::Code::GuardrailBlocked,
        antirion_core::Stage::Route,
        antirion_error::Message::built(block.message.clone()),
    )
    .openai_code("content_policy_violation");
    e.reason = Some(crate::guard::category_reason(category));
    e.guardrail = Some(Box::new(antirion_error::GuardrailFacts {
        shown: json!({
            "name": path,
            "type": "route_block",
            "stage": "route",
            "category": category.as_str(),
        }),
        status: Some(block.status),
    }));
    e
}

/// The error of a strict session binding whose position cannot serve now.
fn strict_unavailable(cx: &Ctx, deployment: &str) -> GatewayError {
    let mut e = crate::respstate::affinity_unavailable();
    let now = cx.rt.now();
    let wait = cx
        .rt
        .state
        .health
        .recovery(deployment, now)
        .filter(|at| *at > now)
        .map(|at| at.since(now));
    e.retry_after = Some(wait.unwrap_or(Duration::from_secs(1)));
    e
}

/// The deployments a request may use as if they were healthy: every candidate of the route that
/// the key may use and that can serve the request is ineligible only because of its breaker or a
/// deployment cooldown, and the route's `on_all_unhealthy` (else each deployment's
/// `all_unhealthy`) is `ignore_health`. Empty when some candidate is healthy or blocked otherwise.
fn all_unhealthy(
    snap: &antirion_runtime::Snapshot,
    route: &RouteRt,
    filters: &mut crate::route::Filters<'_>,
) -> Vec<String> {
    let route_says = route.affinity.ignore_when_all_unhealthy;
    if route_says == Some(false) || (route_says.is_none() && !snap.health.ignores_any()) {
        return Vec::new();
    }
    let mut names: Vec<&str> = antirion_router::deployments_of(&route.tree);
    names.sort_unstable();
    names.dedup();
    let mut out = Vec::new();
    for name in names {
        let Ok(d) = filters.serves(name) else {
            continue;
        };
        match filters.health(&d) {
            Err(u) if u.breaker_only => {
                if route_says.unwrap_or_else(|| snap.health.for_deployment(name).ignore_when_all_unhealthy) {
                    out.push(name.to_owned());
                }
            }
            _ => return Vec::new(),
        }
    }
    out
}
