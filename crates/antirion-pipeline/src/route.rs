//! Stage `route`: the request's facts for conditions, the eligibility filters, the attempt plan of
//! the resolved route read as tiers by the attempt loop, pins, session affinity, and the error when
//! no position is eligible.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use antirion_core::Stage;
use antirion_dialects::Endpoint;
use antirion_dialects::xlate::warn::Refusal;
use antirion_error::{Code, GatewayError, Message};
use antirion_providers::DeploymentRt;
use antirion_resilience::{KeyInfo, KeyMode, KeyPick, Planner, Policy, Selected};
use antirion_router::body_edit::BodyEdit;
use antirion_router::plan::PoolDispatch;
use antirion_router::{
    AttemptPlan, CostOracle, Eligibility, Exclusion, Facts, Leaf, NodeKind, Params, RouteRt, SelectCx, Step,
};
use antirion_runtime::Snapshot;
use antirion_runtime::identity::Access;
use http::HeaderMap;
use serde_json::{Map, Value};

use crate::authorize;
use crate::caps::{self, Need, Sizes};
use crate::cell::{Cell, cell};
use crate::ctx::Ctx;
use crate::fallbacks::{Fallbacks, Segment};
use crate::parse::Parsed;
use crate::reasons;

/// A plan position the loop selected: what the attempt needs to build its upstream request.
#[derive(Clone, Debug)]
pub(crate) struct Position {
    /// The plan segment: 0 for the route, then each fallback Model entered.
    pub(crate) segment: usize,
    pub(crate) node: usize,
    pub(crate) route_path: String,
    pub(crate) deployment: Arc<DeploymentRt>,
    /// The alias's params, then each node's from the root to the leaf.
    pub(crate) params: Vec<Params>,
    /// The leaf's edits of the final upstream body.
    pub(crate) body_edit: Option<Arc<BodyEdit>>,
    /// The pool the leaf belongs to, for the attempt's assumed load and prefix.
    pub(crate) pool: Option<PoolDispatch>,
}

/// Positions by deployment, shared by the planner and the sender (a deployment is selected once).
pub(crate) type Positions = Arc<Mutex<HashMap<String, Position>>>;

/// The facts conditions read.
#[derive(Debug)]
pub(crate) struct RequestFacts<'a> {
    pub(crate) headers: &'a HeaderMap,
    pub(crate) body: Option<&'a Value>,
    pub(crate) metadata: Option<Value>,
    pub(crate) request: Map<String, Value>,
    pub(crate) identity: Map<String, Value>,
    pub(crate) session: Option<String>,
    /// The request id and hash seed `sample` predicates draw on.
    pub(crate) sample: Option<(&'a str, u64)>,
    /// The results of the guardrails conditions read.
    pub(crate) guards: Option<Arc<crate::guardsel::GuardResults>>,
}

impl Facts for RequestFacts<'_> {
    fn guard(&self, name: &str, field: &str) -> Option<Value> {
        self.guards.as_ref()?.field(name, field)
    }

    fn sample_point(&self) -> Option<f64> {
        self.sample
            .map(|(id, seed)| antirion_router::predicate::sample_point(seed, id))
    }

    fn header(&self, name: &str) -> Option<String> {
        let values: Vec<&str> = self
            .headers
            .get_all(name)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect();
        (!values.is_empty()).then(|| values.join(", "))
    }

    fn body(&self) -> Option<&Value> {
        self.body
    }

    fn metadata(&self) -> Option<&Value> {
        self.metadata.as_ref()
    }

    fn request(&self, field: &str) -> Option<Value> {
        self.request.get(field).cloned()
    }

    fn identity(&self, field: &str) -> Option<Value> {
        self.identity.get(field).cloned()
    }
}

impl RequestFacts<'_> {
    /// The value a `sticky_by` source reads.
    pub(crate) fn sticky(&self, source: &str) -> Option<String> {
        let text = |v: Value| match v {
            Value::String(s) => Some(s),
            Value::Null => None,
            other => Some(other.to_string()),
        };
        match source {
            "session" => self.session.clone(),
            "end_user" => self.identity("end_user").and_then(text),
            "key" => self.identity("key").and_then(text),
            _ => match source.split_once(':') {
                Some(("header", name)) => self.header(&name.to_ascii_lowercase()),
                Some(("metadata", k)) => self
                    .metadata
                    .as_ref()
                    .and_then(|m| m.get(k))
                    .cloned()
                    .and_then(text),
                _ => None,
            },
        }
    }
}

/// The eligibility filters, in order: key lists, capability, health.
pub(crate) struct Filters<'a> {
    pub(crate) cx: &'a Ctx,
    pub(crate) snap: &'a Snapshot,
    pub(crate) endpoint: &'a Endpoint,
    pub(crate) parsed: &'a Parsed,
    /// Only a deployment that serves the client's dialect natively qualifies.
    pub(crate) native_only: bool,
    /// The decoded request, when some candidate translates it.
    pub(crate) xlate: Option<&'a crate::xlate::Xlate>,
    /// Translation verdicts by deployment (`None` when feasible), from `resolve` and then cached.
    pub(crate) verdicts: HashMap<String, Option<Refusal>>,
    pub(crate) needs: &'a [Need],
    pub(crate) sizes: Sizes,
    pub(crate) levels: Vec<&'a Access>,
    /// The resolved public Model.
    pub(crate) model: String,
    pub(crate) tried: Vec<String>,
    /// Deployments of `provider_model` leaves and direct addressing, by leaf name.
    pub(crate) synthetic: HashMap<String, Arc<DeploymentRt>>,
    /// Deployments whose breaker and cooldown are ignored: every candidate was unhealthy, and the
    /// configuration says to route as if they were healthy then.
    pub(crate) ignore_health: Vec<String>,
    /// The provider credential the client brought.
    pub(crate) client_keys: Option<&'a crate::byok::ClientKeys>,
    /// With tag filtering, the leaves of the current route the request's tags allow.
    pub(crate) tagged: Option<HashSet<usize>>,
}

impl Filters<'_> {
    /// The deployment of a leaf.
    pub(crate) fn deployment(&mut self, name: &str) -> Option<Arc<DeploymentRt>> {
        if let Some(d) = self.snap.providers.deployments.get(name) {
            return Some(Arc::clone(d));
        }
        if let Some(d) = self.synthetic.get(name) {
            return Some(Arc::clone(d));
        }
        let (provider, model) = name.split_once('/')?;
        let p = self.snap.providers.providers.get(provider)?;
        let caps = self
            .snap
            .config
            .doc
            .pointer(&format!("/providers/{provider}/capabilities"));
        let d = Arc::new(DeploymentRt::direct(p, name, model, caps));
        self.synthetic.insert(name.to_owned(), Arc::clone(&d));
        Some(d)
    }

    /// Why the request cannot be translated for `d`, if it cannot: a dry-run encode, once per
    /// deployment.
    fn translation(&mut self, d: &DeploymentRt, cell: Cell) -> Option<&'static str> {
        let Cell::Translate { .. } = cell else {
            return None;
        };
        let Some(x) = self.xlate else {
            return Some("dialects");
        };
        let doc = &self.snap.config.doc;
        self.verdicts
            .entry(d.name.clone())
            .or_insert_with(|| crate::xlate::feasible(doc, x, d, cell).err())
            .as_ref()
            .map(Refusal::reason)
    }

    /// The refusal of the first candidate the translation excluded.
    pub(crate) fn first_refusal(&self, reason: &str) -> Option<&Refusal> {
        self.verdicts
            .iter()
            .filter_map(|(name, v)| v.as_ref().map(|r| (name, r)))
            .filter(|(_, r)| r.reason() == reason)
            .min_by(|a, b| a.0.cmp(b.0))
            .map(|(_, r)| r)
    }

    fn keys(d: &DeploymentRt) -> Vec<KeyInfo> {
        d.provider
            .keys
            .iter()
            .map(|k| KeyInfo {
                name: k.name.clone(),
                weight: k.weight,
            })
            .collect()
    }
}

impl Filters<'_> {
    /// Filters 1 to 4: whether the key may use the deployment and it can serve the request.
    pub(crate) fn serves(&mut self, name: &str) -> Result<Arc<DeploymentRt>, Exclusion> {
        let Some(d) = self.deployment(name) else {
            return Err(Exclusion::Forbidden);
        };
        if !authorize::deployment_allowed(&self.levels, &d) {
            return Err(Exclusion::Forbidden);
        }
        match crate::byok::eligible(self.client_keys, &d) {
            Ok(()) => {}
            Err(crate::byok::Ineligible::NotAccepted) => return Err(Exclusion::Forbidden),
            Err(crate::byok::Ineligible::KeyRequired) => {
                return Err(Exclusion::Capability(crate::byok::KEY_REQUIRED.to_owned()));
            }
        }
        if self
            .parsed
            .pinned_provider()
            .is_some_and(|p| p != d.provider.name)
        {
            return Err(Exclusion::Capability("resource_pinned".to_owned()));
        }
        let cell = match cell(self.endpoint, self.parsed, &d) {
            Some(c) if !self.native_only || c == Cell::Identity => c,
            _ => return Err(Exclusion::Capability("dialects".to_owned())),
        };
        if let Some(n) = caps::missing(&d, self.needs).first() {
            return Err(Exclusion::Capability(n.flag.to_owned()));
        }
        if let Some(flag) = caps::size_unmet(&d, self.sizes) {
            return Err(Exclusion::Capability(flag.to_owned()));
        }
        if let Some(r) = self.translation(&d, cell) {
            return Err(Exclusion::Capability(r.to_owned()));
        }
        Ok(d)
    }

    /// Filter 5: the deployment's health.
    pub(crate) fn health(&self, d: &DeploymentRt) -> Result<(), antirion_resilience::Unhealthy> {
        if !d.usable() {
            return Err(antirion_resilience::Unhealthy {
                recover_at: None,
                cooling_only: false,
                disabled: true,
                breaker_only: false,
            });
        }
        let settings = self.snap.health.for_deployment(&d.name);
        self.cx.rt.state.health.status(
            &d.name,
            &d.provider.name,
            &Filters::keys(d),
            settings,
            self.cx.rt.now(),
        )
    }
}

impl Eligibility for Filters<'_> {
    fn check(&mut self, leaf: Leaf<'_>) -> Result<(), Exclusion> {
        if self.tried.iter().any(|t| t == leaf.deployment) {
            return Err(Exclusion::Tried);
        }
        let d = self.serves(leaf.deployment)?;
        if self.tagged.as_ref().is_some_and(|t| !t.contains(&leaf.node)) {
            return Err(Exclusion::TagFilter);
        }
        match self.health(&d) {
            Err(u) if u.breaker_only && self.ignore_health.iter().any(|n| *n == d.name) => Ok(()),
            Err(u) => Err(Exclusion::Health {
                recover_at: u.recover_at,
                cooling_only: u.cooling_only,
            }),
            Ok(()) => Ok(()),
        }
    }
}

/// Prices for `least_cost`: the deployment's configured price per million tokens.
pub(crate) struct Prices<'a> {
    pub(crate) snap: &'a Snapshot,
    pub(crate) input: f64,
    pub(crate) output: f64,
}

impl CostOracle for Prices<'_> {
    fn price(&self, deployment: &str) -> Option<f64> {
        let price = self
            .snap
            .config
            .doc
            .pointer(&format!("/deployments/{deployment}/price"))?;
        let rate = |k: &str| {
            price
                .get(k)
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<f64>().ok())
        };
        Some(rate("input")? * self.input + rate("output").unwrap_or(0.0) * self.output)
    }
}

/// What the planner reads per request besides the plan.
pub(crate) struct Tiers<'a> {
    pub(crate) plan: AttemptPlan<'a>,
    pub(crate) sel: SelectCx<'a>,
    pub(crate) filters: Filters<'a>,
    pub(crate) positions: Positions,
    pub(crate) route: &'a RouteRt,
    pub(crate) alias: Option<Params>,
    /// The merged resilience policy above the route's nodes (gateway down to the Model).
    pub(crate) base: Value,
    pub(crate) max_attempts: Option<u32>,
    /// `max_attempts` raises the configured budget (a key with `override:relax`).
    pub(crate) attempts_relaxed: bool,
    pub(crate) stash: Option<Step>,
    pub(crate) tier_first: bool,
    /// The last attempted position: (segment, node).
    pub(crate) last: Option<(usize, usize)>,
    /// Policies by position: (segment, node).
    pub(crate) policies: HashMap<(usize, usize), Arc<Policy>>,
    pub(crate) skipped: Vec<(String, &'static str)>,
    pub(crate) limit_refused: Option<Duration>,
    pub(crate) visited: Vec<String>,
    /// The session value a `sample_by: session` mirror draws on.
    pub(crate) session: Option<String>,
    /// The copies planned with the first position, for the first attempt to start.
    pub(crate) mirrors: crate::mirror::Jobs,
    pub(crate) mirrored: bool,
    /// The Models the plan may move to beyond the route.
    pub(crate) fallbacks: &'a Fallbacks,
    /// The request-supplied fallback Models not yet entered, in order.
    pub(crate) pending: VecDeque<&'a Segment>,
    /// The segment the plan walks: 0 for the route, then one more for each Model entered.
    pub(crate) segment: usize,
    /// The transition label of the move into the current segment, for its first position.
    pub(crate) entering: Option<String>,
    /// Seeds the plans of the segments entered.
    pub(crate) seed: u64,
    /// The request's tags, for the tag filters of the routes entered.
    pub(crate) tags: Vec<String>,
    /// The experiment assignment was recorded.
    pub(crate) assigned: bool,
    /// Degraded causes of the plans already reported.
    pub(crate) causes: Vec<&'static str>,
    /// The first position was a `block` node, with its path.
    pub(crate) blocked: Option<(Arc<antirion_router::tree::Block>, String)>,
}

fn merge(into: &mut Value, from: &Value) {
    match (into, from) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                match a.get_mut(k) {
                    Some(slot) if slot.is_object() && v.is_object() => merge(slot, v),
                    _ => {
                        a.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (slot, v) => *slot = v.clone(),
    }
}

impl<'a> Tiers<'a> {
    /// The resilience policy at a position: the base, then each node's from the root down.
    fn policy(&mut self, node: usize) -> Arc<Policy> {
        if let Some(p) = self.policies.get(&(self.segment, node)) {
            return Arc::clone(p);
        }
        let mut v = self.base.clone();
        for i in self.route.tree.ancestry(node) {
            if let Some(r) = self
                .route
                .tree
                .node(i)
                .and_then(|n| n.policies.as_ref())
                .and_then(|p| p.get("resilience"))
            {
                merge(&mut v, r);
            }
        }
        let mut p = Policy::from_value(&v);
        if let Some(n) = self.max_attempts {
            p.retry.max_attempts = if self.attempts_relaxed {
                n
            } else {
                p.retry.max_attempts.min(n)
            };
            p.retry.tier_retries = p.retry.tier_retries.min(n.saturating_sub(1));
        }
        let p = Arc::new(p);
        self.policies.insert((self.segment, node), Arc::clone(&p));
        p
    }

    /// Moves the plan to the tiers of another Model.
    fn enter(&mut self, seg: &'a Segment) {
        self.plan = AttemptPlan::new(&seg.route.tree, self.seed);
        self.route = &seg.route;
        self.alias.clone_from(&seg.alias);
        self.sel.captures = seg.captures.as_ref();
        self.base = seg.base.clone();
        self.filters.model.clone_from(&seg.model);
        self.filters.tagged = crate::routing::tagged(&seg.route, &self.tags);
        self.segment += 1;
        self.entering = Some(seg.label.clone());
        self.stash = None;
        self.last = None;
        self.tier_first = true;
    }

    /// Enters the next request-supplied fallback Model that has an eligible position.
    fn next_segment(&mut self) -> bool {
        while let Some(seg) = self.pending.pop_front() {
            self.enter(seg);
            if let Some(step) = self.plan.next(&self.sel, &mut self.filters) {
                self.stash = Some(step);
                return true;
            }
        }
        false
    }

    fn selected(&mut self, step: Step) -> Option<Selected> {
        let deployment = self.filters.deployment(&step.deployment)?;
        let mut params: Vec<Params> = self.alias.iter().cloned().collect();
        for i in self.route.tree.ancestry(step.node) {
            if let Some(p) = self.route.tree.node(i).and_then(|n| n.params.clone()) {
                params.push(p);
            }
        }
        let leaf = self.route.tree.node(step.node);
        let body_edit = leaf.and_then(|n| n.body_edit.clone());
        let pool = self.plan.pool_dispatch(step.node);
        if !self.assigned {
            self.assigned = true;
            crate::routing::experiment(self.filters.cx, &self.route.tree, step.node);
        }
        for cause in &self.plan.causes {
            if !self.causes.contains(cause) {
                self.causes.push(cause);
                crate::routing::degraded(self.filters.cx, cause);
            }
        }
        let via = self.entering.take().or_else(|| {
            self.last
                .filter(|(segment, _)| *segment == self.segment)
                .and_then(|(_, l)| self.plan.label(l, step.node))
        });
        let policy = self.policy(step.node);
        self.visited.push(step.route_path.clone());
        crate::observe::decision(
            self.filters.cx,
            &self.filters.model,
            &self.route.tree,
            &step.deployment,
        );
        if let Ok(mut map) = self.positions.lock() {
            map.insert(
                step.deployment.clone(),
                Position {
                    segment: self.segment,
                    node: step.node,
                    route_path: step.route_path.clone(),
                    deployment: Arc::clone(&deployment),
                    params,
                    body_edit,
                    pool,
                },
            );
        }
        if !self.mirrored {
            self.mirrored = true;
            crate::mirror::plan(self, step.node);
        }
        Some(Selected {
            deployment: step.deployment,
            provider: deployment.provider.name.clone(),
            via,
            policy,
        })
    }

    fn node_of(&self, deployment: &str) -> Option<(usize, usize)> {
        self.positions
            .lock()
            .ok()?
            .get(deployment)
            .map(|p| (p.segment, p.node))
    }

    /// The route path of the last attempted position.
    pub(crate) fn last_path(&self) -> Option<String> {
        let last = self.last?;
        let map = self.positions.lock().ok()?;
        map.values()
            .find(|p| (p.segment, p.node) == last)
            .map(|p| p.route_path.clone())
    }

    /// The error when no position was eligible: the most recoverable reason.
    pub(crate) fn no_candidate(&self, continuation: bool) -> GatewayError {
        exhausted(
            self.plan.exhaustion(),
            self.limit_refused,
            &self.filters,
            continuation,
        )
    }
}

/// The error when no position of a plan was eligible: the most recoverable reason.
pub(crate) fn exhausted(
    x: &antirion_router::Exhaustion,
    limit_refused: Option<Duration>,
    filters: &Filters<'_>,
    continuation: bool,
) -> GatewayError {
    let now = filters.cx.rt.now();
    let wait = |at: Option<antirion_core::UtcMicros>| at.filter(|a| *a > now).map(|a| a.since(now));
    if continuation && !x.limit && limit_refused.is_none() {
        let mut e = crate::respstate::affinity_unavailable();
        e.retry_after = Some(wait(x.recover_at).unwrap_or(Duration::from_secs(1)));
        return e;
    }
    if x.health {
        if x.cooling_only && !x.limit && x.capability.is_none() && limit_refused.is_none() {
            let mut e = GatewayError::with_reason(
                antirion_resilience::reasons::UPSTREAM_COOLING_DOWN,
                Stage::Route,
                Message::Fixed("Every upstream credential of this model is cooling down after rate limits."),
            );
            e.retry_after = Some(wait(x.recover_at).unwrap_or(Duration::from_secs(1)));
            return e;
        }
        let mut e = GatewayError::new(
            Code::NoHealthyDeployment,
            Stage::Route,
            Message::Fixed("No deployment of this model is healthy now."),
        );
        e.retry_after = Some(wait(x.recover_at).unwrap_or(Duration::from_secs(1)));
        return e;
    }
    if x.limit || limit_refused.is_some() {
        return antirion_limits::refusal::deployment_limits_error(
            x.limit_wait.or(limit_refused).unwrap_or(Duration::from_secs(1)),
        );
    }
    if let Some(flag) = &x.capability {
        if flag == crate::byok::KEY_REQUIRED {
            return crate::byok::key_required();
        }
        if let Some(r) = filters.first_refusal(flag) {
            let mut e = crate::xlate::refusal_error(r);
            e.stage = Stage::Route;
            return e;
        }
        return caps::unsupported_flag(flag);
    }
    if x.no_match {
        return GatewayError::new(
            Code::NoRouteMatched,
            Stage::Route,
            Message::Fixed("No routing rule matched this request and the route has no default."),
        );
    }
    if x.forbidden {
        return authorize::no_allowed_deployment();
    }
    if x.tag_filtered {
        return GatewayError::with_reason(
            reasons::TAG_FILTER_EXCLUDED,
            Stage::Route,
            Message::Fixed("No deployment of this model carries the tags of this request."),
        );
    }
    let mut e = GatewayError::new(
        Code::NoHealthyDeployment,
        Stage::Route,
        Message::Fixed("No deployment of this model is available."),
    );
    e.retry_after = Some(Duration::from_secs(1));
    e
}

impl Planner for Tiers<'_> {
    fn select(&mut self, tried: &[String]) -> Option<Selected> {
        self.filters.tried = tried.to_vec();
        if let Some(step) = self.stash.take() {
            if !self.tier_first {
                self.stash = Some(step);
                return None;
            }
            if !tried.contains(&step.deployment) {
                self.tier_first = false;
                return self.selected(step);
            }
        }
        let step = self.plan.next(&self.sel, &mut self.filters)?;
        if let Some(b) = &step.block {
            self.blocked = Some((Arc::clone(b), step.route_path.clone()));
            return None;
        }
        if step.new_tier && !self.tier_first {
            self.stash = Some(step);
            return None;
        }
        self.tier_first = false;
        self.selected(step)
    }

    fn next_tier(&mut self) -> bool {
        if self.stash.is_none() {
            self.plan.skip_tier();
            match self.plan.next(&self.sel, &mut self.filters) {
                Some(s) => self.stash = Some(s),
                None => return self.next_segment(),
            }
        }
        self.tier_first = true;
        true
    }

    fn class_fallback(&mut self, model: &str) -> bool {
        let fallbacks = self.fallbacks;
        let Some(seg) = fallbacks.class(model) else {
            return false;
        };
        fallbacks.take();
        self.pending.clear();
        self.enter(seg);
        true
    }

    fn attempted(&mut self, s: &Selected) {
        self.last = self.node_of(&s.deployment);
    }

    fn select_key(&mut self, s: &Selected, tried: &[String], last: Option<&str>, mode: KeyMode) -> KeyPick {
        let Some(d) = self.filters.deployment(&s.deployment) else {
            return KeyPick::None;
        };
        pick_key(self.filters.cx, &d, tried, last, mode)
    }

    fn skip(&mut self, s: &Selected, reason: &'static str) {
        self.skipped.push((s.deployment.clone(), reason));
    }

    async fn gate(&mut self, s: &Selected) -> Result<(), &'static str> {
        let path = self
            .positions
            .lock()
            .ok()
            .and_then(|m| m.get(&s.deployment).map(|p| p.route_path.clone()))
            .unwrap_or_default();
        let nodes = node_paths(&path);
        match crate::account::admit_attempt(&self.filters.cx.account, &nodes, &s.deployment).await {
            Ok(()) => Ok(()),
            Err(wait) => {
                let wait = wait.unwrap_or(Duration::from_secs(1));
                self.limit_refused = Some(self.limit_refused.map_or(wait, |w| w.min(wait)));
                Err("limit")
            }
        }
    }
}

/// The route paths of the tree nodes on a position's path, from the root down.
fn node_paths(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut acc = String::new();
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(seg);
        out.push(acc.clone());
    }
    out
}

/// A key of `d` for the attempt loop, from the health registry's key state.
pub(crate) fn pick_key(
    cx: &Ctx,
    d: &DeploymentRt,
    tried: &[String],
    last: Option<&str>,
    mode: KeyMode,
) -> KeyPick {
    let keys = Filters::keys(d);
    if keys.is_empty() {
        return KeyPick::Keyless;
    }
    let settings = cx.snap.health.for_deployment(&d.name);
    cx.rt
        .state
        .health
        .select_key(
            &d.name,
            &d.provider.name,
            &keys,
            tried,
            last,
            mode,
            settings,
            cx.rt.now(),
        )
        .map_or(KeyPick::None, KeyPick::Key)
}

/// The leaf of a pinned deployment in a route, or the pin error.
pub(crate) fn pinned_leaf(route: &RouteRt, pin: &str, param: &'static str) -> Result<usize, GatewayError> {
    AttemptPlan::leaf_of(&route.tree, pin).ok_or_else(|| {
        GatewayError::with_reason(
            reasons::PIN_NOT_IN_ROUTE,
            Stage::Route,
            Message::built(format!(
                "The pinned deployment `{pin}` is not part of this model's route."
            )),
        )
        .param(param)
    })
}

/// The leaf whose route path a binding names.
pub(crate) fn leaf_by_path(route: &RouteRt, path: &str) -> Option<usize> {
    route.tree.nodes.iter().position(|n| match &n.kind {
        NodeKind::Deployment(d) => format!("{}/{d}", n.path) == path,
        _ => false,
    })
}

/// The session value of a request from the route's sources, trimmed; invalid values are skipped.
pub(crate) fn session_value(
    sources: &[String],
    facts: &RequestFacts<'_>,
    header: Option<&str>,
) -> Option<(String, String)> {
    for source in sources {
        let raw = match source.split_once(':') {
            Some(("header", name)) if name.eq_ignore_ascii_case("x-antirion-session-id") => {
                header.map(str::to_owned)
            }
            Some(("header", name)) => facts.header(&name.to_ascii_lowercase()),
            Some(("body", path)) => facts
                .body
                .and_then(|b| {
                    antirion_router::predicate::parse_path(path)
                        .and_then(|p| antirion_router::predicate::lookup(b, &p).cloned())
                })
                .and_then(|v| v.as_str().map(str::to_owned)),
            _ => None,
        };
        let Some(v) = raw else { continue };
        let v = v.trim();
        if (1..=256).contains(&v.len()) && v.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
            return Some((source.clone(), v.to_owned()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn facts<'a>(headers: &'a HeaderMap, body: &'a Value) -> RequestFacts<'a> {
        RequestFacts {
            headers,
            body: Some(body),
            metadata: Some(json!({"tier": "pro"})),
            request: Map::new(),
            identity: Map::new(),
            session: None,
            sample: None,
            guards: None,
        }
    }

    #[test]
    fn node_paths_run_from_the_root() {
        assert_eq!(node_paths("r/a/b"), ["r", "r/a", "r/a/b"]);
        assert!(node_paths("").is_empty());
    }

    #[test]
    fn session_sources_are_tried_in_order_and_trimmed() {
        let mut h = HeaderMap::new();
        h.insert("x-claude-code-session-id", "cc-1".parse().unwrap());
        let body = json!({"prompt_cache_key": " pk-1 ", "metadata": {"user_id": "u"}});
        let f = facts(&h, &body);
        let sources: Vec<String> = [
            "header:x-antirion-session-id",
            "body:prompt_cache_key",
            "header:x-claude-code-session-id",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        assert_eq!(
            session_value(&sources, &f, None),
            Some(("body:prompt_cache_key".to_owned(), "pk-1".to_owned()))
        );
        assert_eq!(
            session_value(&sources, &f, Some("s1")).map(|(_, v)| v).as_deref(),
            Some("s1")
        );
        let bad_body = json!({"prompt_cache_key": "has space"});
        let bad = facts(&h, &bad_body);
        assert_eq!(
            session_value(&sources, &bad, None).map(|(_, v)| v).as_deref(),
            Some("cc-1")
        );
        assert_eq!(f.sticky("metadata:tier").as_deref(), Some("pro"));
        assert_eq!(
            f.sticky("header:x-claude-code-session-id").as_deref(),
            Some("cc-1")
        );
    }
}
