//! Routing decisions without proxying: `POST /antirion/decide/<original path>` and the gateway-wide
//! dry run. The plan is built as the request would build it (pins, affinity, classifier and scorer
//! calls, eligibility with live health), but selection reads its state without advancing it,
//! affinity is looked up and never written, and nothing goes upstream.

use std::collections::HashMap;
use std::sync::Arc;

use antirion_core::headers as h;
use antirion_error::GatewayError;
use antirion_providers::DeploymentRt;
use antirion_router::{AttemptPlan, Captures, CompiledRoute, NodeKind, RouteRt, SelectCx, binding_key};
use antirion_runtime::cluster::Lookup;
use http::Uri;
use serde_json::{Value, json};

use crate::controls::PIN_DEPLOYMENT;
use crate::ctx::Ctx;
use crate::infer::Request;
use crate::route::{Filters, Prices, exhausted, leaf_by_path, pinned_leaf, session_value};
use crate::routing::{block_error, facts, slot, tagged};

/// The path prefix of a decide request.
const PREFIX: &str = "/antirion/decide/";

/// The most plan positions a decision lists.
const POSITIONS: usize = 8;

/// Rewrites a decide request's URI to the original path; whether it was a decide request.
pub(crate) fn strip(uri: &mut Uri) -> bool {
    let Some(rest) = uri.path().strip_prefix(PREFIX) else {
        return false;
    };
    let original = match uri.query() {
        Some(q) => format!("/{rest}?{q}"),
        None => format!("/{rest}"),
    };
    match original.parse::<Uri>() {
        Ok(u) => {
            *uri = u;
            true
        }
        Err(_) => false,
    }
}

/// What the request carries for routing beyond its facts.
pub(crate) struct Carried<'a> {
    pub(crate) turn: Option<&'a crate::respstate::Turn>,
    pub(crate) xlate: Option<&'a crate::xlate::Xlate>,
    pub(crate) client_keys: Option<&'a crate::byok::ClientKeys>,
}

/// What the decision is about besides the request.
pub(crate) struct Subject<'a> {
    pub(crate) route: &'a Arc<RouteRt>,
    pub(crate) captures: Option<&'a Captures>,
    pub(crate) direct: Option<Arc<DeploymentRt>>,
    /// `exact`, `alias`, `wildcard`, `provider` or `default`.
    pub(crate) via: &'static str,
}

/// The arm of the experiment above `node`, if any.
fn experiment_of(tree: &CompiledRoute, node: usize) -> Value {
    let chain = tree.ancestry(node);
    for pair in chain.windows(2) {
        let [parent, child] = pair else { continue };
        if let Some(NodeKind::Weighted {
            experiment: Some(e), ..
        }) = tree.node(*parent).map(|n| &n.kind)
            && let Some(arm) = tree.node(*child).and_then(|n| n.segment.clone())
        {
            return json!({"name": e.name, "arm": arm});
        }
    }
    Value::Null
}

/// The routing decision body (what a dry run answers), or the error the request would get.
pub(crate) async fn decide(
    cx: &Ctx,
    plan: Carried<'_>,
    req: &Request<'_>,
    subject: Subject<'_>,
) -> Result<Value, GatewayError> {
    let snap = Arc::clone(&cx.snap);
    let rt = Arc::clone(&cx.rt);
    let route = subject.route;
    let key = &req.auth.key;
    let controls = &req.auth.controls;
    let levels = snap.identity.access_levels(key);
    let mut facts = facts(req, (cx.request_id.as_str(), snap.routing.settings.hash_seed));
    if route.reads_guards {
        let results = Arc::new(crate::guardsel::GuardResults::default());
        facts.guards = Some(Arc::clone(&results));
        Box::pin(crate::guardsel::run(cx, route, &facts, &results)).await;
    }
    let test_pin = rt
        .test
        .as_ref()
        .and_then(|_| req.parts.headers.get(h::TEST_DEPLOYMENT))
        .and_then(|v| v.to_str().ok())
        .map(|p| (p, h::TEST_DEPLOYMENT));
    let continuation = plan.turn.and_then(|t| t.pin.clone());
    let pin = continuation
        .as_deref()
        .map(|d| (d, "previous_response_id"))
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
        xlate: plan.xlate,
        verdicts: req.verdicts.clone(),
        needs: req.needs,
        sizes: req.sizes,
        levels,
        model: req.model_name.to_owned(),
        tried: Vec::new(),
        synthetic: HashMap::new(),
        ignore_health: Vec::new(),
        client_keys: plan.client_keys,
        tagged: tagged(route, req.tags),
    };
    if let Some(d) = &subject.direct {
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
        captures: subject.captures,
        now: rt.now(),
        dry: true,
    };
    let aff = &route.affinity;
    if pin.is_none() && route.decides_ahead {
        let session = session_value(&aff.sources, &facts, controls.session.as_deref());
        for picker in crate::decide::take(
            cx,
            req,
            route,
            &mut attempt_plan,
            &sel,
            session.as_ref().map(|(_, v)| v.as_str()),
        )
        .await?
        {
            picker.finish(None).await;
        }
    }
    let mut affinity = Value::Null;
    let mut bound: Option<String> = None;
    let affinity_on = (aff.session || aff.prompt_prefix) && subject.direct.is_none();
    if pin.is_some() {
        affinity = json!({"source": null, "outcome": "pinned"});
    } else if affinity_on {
        if controls.affinity_off {
            affinity = json!({"source": null, "outcome": "off"});
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
            let scope: Vec<&str> = match aff.scope.as_str() {
                "key" => vec![key.name.as_str()],
                "organization" => vec![key.organization.as_str()],
                _ => vec![key.organization.as_str(), key.project.as_str()],
            };
            let k = binding_key(&scope, &route.tree.name, &source, &value);
            if let Lookup::Bound(b) = rt
                .affinity_lookup(&k, rt.now(), settings.affinity_max_entries)
                .await
            {
                if let Some(node) = leaf_by_path(route, &b.route_path) {
                    attempt_plan.prefer(node, &sel, &mut filters);
                }
                bound = Some(b.route_path);
            }
            affinity = json!({"source": source, "outcome": Value::Null});
        } else {
            affinity = json!({"source": null, "outcome": "none"});
        }
    }
    let mut positions: Vec<antirion_router::Step> = Vec::new();
    while positions.len() < POSITIONS {
        let Some(step) = attempt_plan.next(&sel, &mut filters) else {
            break;
        };
        if let Some(b) = &step.block {
            return Err(block_error(b, &step.route_path));
        }
        positions.push(step);
    }
    let Some(first) = positions.first() else {
        return Err(exhausted(
            attempt_plan.exhaustion(),
            None,
            &filters,
            continuation.is_some(),
        ));
    };
    if let Some(o) = affinity.as_object_mut()
        && o.get("outcome") == Some(&Value::Null)
    {
        let outcome = match &bound {
            Some(b) if *b == first.route_path => "hit",
            Some(_) => "moved",
            None => "new",
        };
        o.insert("outcome".into(), json!(outcome));
    }
    let describe = |s: &antirion_router::Step| {
        let d = snap
            .providers
            .deployments
            .get(&s.deployment)
            .or(subject.direct.as_ref());
        let provider = s.provider.clone().or_else(|| d.map(|d| d.provider.name.clone()));
        let upstream = s.upstream_model.clone().or_else(|| d.map(|d| d.model.clone()));
        (provider, upstream)
    };
    let mut plan_json = Vec::new();
    let mut prev: Option<usize> = None;
    for s in &positions {
        let transition = prev.and_then(|p| attempt_plan.label(p, s.node));
        plan_json
            .push(json!({"route_path": s.route_path, "deployment": s.deployment, "transition": transition}));
        prev = Some(s.node);
    }
    let (provider, upstream) = describe(first);
    let cost = antirion_router::CostOracle::price(&prices, &first.deployment);
    let debug = controls
        .route_debug
        .then(|| json!({"plan": positions.iter().map(|s| s.route_path.clone()).collect::<Vec<_>>()}));
    Ok(json!({
        "object": "antirion.route_decision",
        "model": req.model_name,
        "resolved_via": subject.via,
        "route": route.tree.name,
        "route_path": first.route_path,
        "deployment": first.deployment,
        "provider": provider,
        "upstream_model": upstream,
        "plan": plan_json,
        "affinity": affinity,
        "experiment": experiment_of(&route.tree, first.node),
        "estimated_cost_usd": cost.map(|c| format!("{c:.8}")),
        "debug": debug,
    }))
}
