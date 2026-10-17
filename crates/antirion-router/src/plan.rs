//! The attempt plan: the lazy depth-first concatenation of node plans. Each node orders its
//! children when the plan first enters it (by its selection algorithm), lazily (`select`), and
//! eligibility is checked at every leaf, so a leaf that became unhealthy mid-request is skipped.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use antirion_core::{SplitMix64, UtcMicros};

use crate::predicate::Facts;
use crate::select::{self, Order, leaf_ref};
use crate::state::{RoutingState, Slot};
use crate::tree::{Algorithm, Block, CompiledRoute, NodeKind};

mod bandit;
mod decide;
mod pool;

pub use self::pool::PoolDispatch;

/// Why a leaf may not serve the request now (the filters, in order).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Exclusion {
    /// Already attempted in this request: skipped, not an exclusion reason.
    Tried,
    /// The key may not use the deployment.
    Forbidden,
    /// The request's tags exclude the leaf.
    TagFilter,
    /// The deployment cannot serve the request; the first unmet capability.
    Capability(String),
    /// The deployment's own limits refuse the request; the wait when known.
    Limit(Option<Duration>),
    /// Health: breaker, cooldown, keys or a manual disable.
    Health {
        recover_at: Option<UtcMicros>,
        cooling_only: bool,
    },
}

/// A leaf being checked.
#[derive(Copy, Clone, Debug)]
pub struct Leaf<'a> {
    pub node: usize,
    /// The deployment name (for a `provider_model` leaf, `provider/upstream_model`).
    pub deployment: &'a str,
}

/// The eligibility filters of the request.
pub trait Eligibility {
    /// Why the leaf cannot serve this request, if it cannot.
    fn check(&mut self, leaf: Leaf<'_>) -> Result<(), Exclusion>;
}

/// Estimated prices for `least_cost`.
pub trait CostOracle {
    /// The estimated price of this request on a deployment; `None` when it has no price.
    fn price(&self, deployment: &str) -> Option<f64>;
}

/// The text a wildcard Model captured, for `provider_model` templates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Captures {
    pub model: String,
    pub groups: Vec<String>,
}

impl Captures {
    /// `{model}` and `{1}`…`{9}` substituted.
    #[must_use]
    pub fn render(&self, template: &str) -> String {
        let mut out = template.replace("{model}", &self.model);
        for (i, g) in self.groups.iter().enumerate().take(9) {
            out = out.replace(&format!("{{{}}}", i + 1), g);
        }
        out
    }
}

/// What node selection reads besides the tree.
pub struct SelectCx<'a> {
    pub state: &'a RoutingState,
    pub facts: &'a (dyn Facts + Sync),
    pub cost: &'a (dyn CostOracle + Sync),
    pub slot: Slot,
    /// Default latency estimates in milliseconds, by slot.
    pub default_estimate: [f64; 3],
    pub hash_seed: u64,
    /// The value `sticky_by` sources read, by source.
    pub sticky: &'a (dyn Fn(&str) -> Option<String> + Sync),
    pub captures: Option<&'a Captures>,
    /// The gateway clock, for tier members' end of life.
    pub now: UtcMicros,
    /// Selection reads its state without advancing it (a decision without a request).
    pub dry: bool,
}

impl std::fmt::Debug for SelectCx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SelectCx")
            .field("slot", &self.slot)
            .finish_non_exhaustive()
    }
}

/// One position of the plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub node: usize,
    pub deployment: String,
    /// The upstream model of a `provider_model` leaf.
    pub upstream_model: Option<String>,
    pub provider: Option<String>,
    /// The route path of the position.
    pub route_path: String,
    /// The transition from the previous position crosses a `fallback` node: a new tier starts.
    pub new_tier: bool,
    /// The position is a `block` node: the request ends as a guardrail block.
    pub block: Option<Arc<Block>>,
}

/// The most recoverable reason the plan had no eligible position.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Exhaustion {
    pub health: bool,
    /// The earliest known recovery among health exclusions.
    pub recover_at: Option<UtcMicros>,
    /// Every health exclusion was only a pair rate-limit cooldown.
    pub cooling_only: bool,
    pub limit: bool,
    pub limit_wait: Option<Duration>,
    /// The first unmet capability of the first position excluded for capability.
    pub capability: Option<String>,
    pub forbidden: bool,
    /// Some leaf was excluded by tag filtering.
    pub tag_filtered: bool,
    /// A conditional node matched no rule and had no default.
    pub no_match: bool,
}

#[derive(Debug)]
struct Frame {
    node: Option<usize>,
    order: Order,
}

/// The attempt plan of one request over one route.
#[derive(Debug)]
pub struct AttemptPlan<'r> {
    route: &'r CompiledRoute,
    stack: Vec<Frame>,
    started: bool,
    rng: SplitMix64,
    decisions: HashMap<usize, Option<usize>>,
    /// The endpoints an external picker chose for a pool, in its order.
    picked: HashMap<usize, Vec<usize>>,
    /// Classifier choices as (node path, class), for debugging output.
    pub classes: Vec<(String, String)>,
    /// Causes of a degraded decision (`pool_metrics`), each once.
    pub causes: Vec<&'static str>,
    /// The prompt's chunk hashes under each pool node the plan ordered.
    pools: HashMap<usize, Vec<u64>>,
    exhaustion: Exhaustion,
    health_seen: bool,
    prev: Option<usize>,
    /// A position came before in plan order, eligible or not: a block after it is a fallback.
    passed: bool,
    front: Option<usize>,
    front_done: Option<usize>,
}

impl<'r> AttemptPlan<'r> {
    #[must_use]
    pub fn new(route: &'r CompiledRoute, seed: u64) -> AttemptPlan<'r> {
        AttemptPlan {
            route,
            stack: Vec::new(),
            started: false,
            rng: SplitMix64::new(seed),
            decisions: HashMap::new(),
            picked: HashMap::new(),
            classes: Vec::new(),
            causes: Vec::new(),
            pools: HashMap::new(),
            exhaustion: Exhaustion::default(),
            health_seen: false,
            prev: None,
            passed: false,
            front: None,
            front_done: None,
        }
    }

    /// A plan with the single position `node` (a pin).
    #[must_use]
    pub fn pinned(route: &'r CompiledRoute, node: usize) -> AttemptPlan<'r> {
        let mut p = AttemptPlan::new(route, 0);
        p.started = true;
        p.stack.push(Frame {
            node: None,
            order: Order::List {
                items: vec![node],
                pos: 0,
            },
        });
        p
    }

    #[must_use]
    pub fn route(&self) -> &'r CompiledRoute {
        self.route
    }

    /// The leaf node of the first position of deployment `d`.
    #[must_use]
    pub fn leaf_of(route: &CompiledRoute, d: &str) -> Option<usize> {
        route
            .nodes
            .iter()
            .position(|n| matches!(&n.kind, NodeKind::Deployment(x) if x == d))
    }

    fn leaf_name(
        &self,
        i: usize,
        cx: &SelectCx<'_>,
    ) -> Option<(String, Option<String>, Option<String>, String)> {
        let n = self.route.node(i)?;
        match &n.kind {
            NodeKind::Deployment(d) => Some((d.clone(), None, None, format!("{}/{d}", n.path))),
            NodeKind::ProviderModel { provider, template } => {
                let upstream = cx
                    .captures
                    .map_or_else(|| template.clone(), |c| c.render(template));
                Some((
                    format!("{provider}/{upstream}"),
                    Some(upstream.clone()),
                    Some(provider.clone()),
                    format!("{}/{provider}:{upstream}", n.path),
                ))
            }
            _ => None,
        }
    }

    fn record(&mut self, e: &Exclusion) {
        let x = &mut self.exhaustion;
        match e {
            Exclusion::Tried => {}
            Exclusion::Forbidden => x.forbidden = true,
            Exclusion::TagFilter => x.tag_filtered = true,
            Exclusion::Capability(c) => {
                if x.capability.is_none() {
                    x.capability = Some(c.clone());
                }
            }
            Exclusion::Limit(w) => {
                x.limit = true;
                x.limit_wait = match (x.limit_wait, *w) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
            }
            Exclusion::Health {
                recover_at,
                cooling_only,
            } => {
                x.cooling_only = if self.health_seen {
                    x.cooling_only && *cooling_only
                } else {
                    *cooling_only
                };
                self.health_seen = true;
                x.health = true;
                x.recover_at = match (x.recover_at, *recover_at) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
            }
        }
    }

    /// Whether some eligible leaf is reachable under node `i`.
    fn reachable(&mut self, i: usize, cx: &SelectCx<'_>, elig: &mut dyn Eligibility) -> bool {
        let route = self.route;
        let Some(n) = route.node(i) else { return false };
        match &n.kind {
            NodeKind::Deployment(_) | NodeKind::ProviderModel { .. } => {
                let Some(name) = leaf_ref(route, i, cx) else {
                    return false;
                };
                match elig.check(Leaf {
                    node: i,
                    deployment: &name,
                }) {
                    Ok(()) => true,
                    Err(e) => {
                        self.record(&e);
                        false
                    }
                }
            }
            NodeKind::Conditional { .. } | NodeKind::Classifier(_) | NodeKind::CostQuality(_) => {
                self.decide(i, cx).is_some_and(|c| self.reachable(c, cx, elig))
            }
            NodeKind::Weighted { .. } => {
                let mut any = false;
                for &c in &n.children {
                    if route.node(c).is_some_and(|x| x.weight > 0) {
                        any |= self.reachable(c, cx, elig);
                    }
                }
                any
            }
            NodeKind::Block(_) => true,
            NodeKind::Member { eol_at, .. } => {
                eol_at.is_none_or(|eol| cx.now < eol)
                    && n.children.iter().any(|&c| self.reachable(c, cx, elig))
            }
            NodeKind::Unsupported(_) => false,
            _ => {
                let mut any = false;
                for &c in &n.children {
                    any |= self.reachable(c, cx, elig);
                }
                any
            }
        }
    }

    /// A uniformly random child of `pool` with an eligible leaf, removed from it.
    fn take_reachable(
        &mut self,
        pool: &mut Vec<usize>,
        cx: &SelectCx<'_>,
        elig: &mut dyn Eligibility,
    ) -> Option<usize> {
        while !pool.is_empty() {
            let len = u64::try_from(pool.len()).unwrap_or(u64::MAX);
            let k = usize::try_from(self.rng.below(len)).unwrap_or(0);
            let c = pool.swap_remove(k.min(pool.len() - 1));
            if self.reachable(c, cx, elig) {
                return Some(c);
            }
        }
        None
    }

    fn order(&mut self, i: usize, cx: &SelectCx<'_>, elig: &mut dyn Eligibility) -> Order {
        let route = self.route;
        let Some(n) = route.node(i) else {
            return Order::EMPTY;
        };
        let kind = match &n.kind {
            NodeKind::Tier(t) => t.selects.as_ref(),
            k => k,
        };
        match kind {
            NodeKind::Route(_) => Order::Children { node: i, pos: 0 },
            NodeKind::Member { eol_at, .. } => {
                if eol_at.is_some_and(|eol| cx.now >= eol) {
                    Order::EMPTY
                } else {
                    Order::Children { node: i, pos: 0 }
                }
            }
            NodeKind::Tier(_) | NodeKind::Block(_) => Order::EMPTY,
            NodeKind::Conditional { .. } | NodeKind::Classifier(_) | NodeKind::CostQuality(_) => {
                Order::List {
                    items: self.decide(i, cx).into_iter().collect(),
                    pos: 0,
                }
            }
            NodeKind::Pool(spec) => self.pool_order(i, spec, cx, elig),
            NodeKind::Unsupported(_) | NodeKind::Deployment(_) | NodeKind::ProviderModel { .. } => {
                Order::EMPTY
            }
            NodeKind::Fallback => Order::Children { node: i, pos: 0 },
            NodeKind::Weighted {
                algorithm: Algorithm::Thompson(spec),
                experiment,
                ..
            } => self.bandit_order(i, spec, experiment.as_deref(), cx, elig),
            NodeKind::Weighted {
                algorithm,
                sticky,
                canary,
                ..
            } => {
                if let Some(v) = sticky.as_deref().and_then(|s| (cx.sticky)(s)) {
                    return select::sticky(route, i, &v, cx.hash_seed);
                }
                let configured: Vec<(usize, usize, u32)> = n
                    .children
                    .iter()
                    .enumerate()
                    .filter_map(|(k, &c)| route.node(c).map(|x| (k, c, x.weight)))
                    .collect();
                let weights = match canary {
                    Some(c) => select::canary_weights(&configured, c, cx.state.canaries.weight(c, cx.now)),
                    None => configured,
                };
                let positive = weights.into_iter().filter(|(.., w)| *w > 0);
                match algorithm {
                    Algorithm::Random => {
                        let pool: Vec<(usize, u32)> = positive.map(|(_, c, w)| (c, w)).collect();
                        let total = pool.iter().map(|(_, w)| u64::from(*w)).sum();
                        Order::Sample {
                            head: None,
                            pool,
                            total,
                        }
                    }
                    Algorithm::SmoothWrr | Algorithm::Thompson(_) => {
                        let mut eligible: Vec<(usize, usize, u32)> = Vec::with_capacity(n.children.len());
                        for (k, c, w) in positive {
                            if self.reachable(c, cx, elig) {
                                eligible.push((k, c, w));
                            }
                        }
                        let cands: Vec<(usize, u32)> = eligible.iter().map(|(k, _, w)| (*k, *w)).collect();
                        let chosen = if cx.dry {
                            cx.state.swrr_peek(&n.state_key, &cands)
                        } else {
                            cx.state.swrr(&n.state_key, n.children.len(), &cands)
                        };
                        let Some(first) = chosen else {
                            return Order::EMPTY;
                        };
                        let head = eligible.get(first).map(|(_, c, _)| *c);
                        let pool: Vec<(usize, u32)> = eligible
                            .iter()
                            .enumerate()
                            .filter(|(j, _)| *j != first)
                            .map(|(_, (_, c, w))| (*c, *w))
                            .collect();
                        let total = pool.iter().map(|(_, w)| u64::from(*w)).sum();
                        Order::Sample { head, pool, total }
                    }
                }
            }
            NodeKind::LeastLatency { decay } => {
                if !cx.dry && !cx.state.primed(&n.state_key, cx.slot) {
                    let default = cx
                        .default_estimate
                        .get(select::slot_ix(cx.slot))
                        .copied()
                        .unwrap_or(500.0);
                    let mut names = Vec::new();
                    select::for_leaves(route, i, &mut |l| {
                        if let Some(d) = leaf_ref(route, l, cx) {
                            names.push(d);
                        }
                    });
                    cx.state.prime(
                        &n.state_key,
                        cx.slot,
                        default,
                        Instant::now(),
                        names.iter().map(|d| d.as_ref()),
                    );
                }
                let mut pool = n.children.clone();
                let a = self.take_reachable(&mut pool, cx, elig);
                let b = a.and_then(|_| self.take_reachable(&mut pool, cx, elig));
                let winner = match (a, b) {
                    (Some(x), Some(y)) => {
                        let now = Instant::now();
                        let ca = select::latency_cost(route, x, cx, *decay, now);
                        let cy = select::latency_cost(route, y, cx, *decay, now);
                        if ca < cy {
                            x
                        } else if cy < ca {
                            y
                        } else if self.rng.below(2) == 0 {
                            x
                        } else {
                            y
                        }
                    }
                    (Some(x), None) => x,
                    _ => return Order::EMPTY,
                };
                Order::Latency {
                    node: i,
                    decay: *decay,
                    winner,
                    given: false,
                    rest: None,
                }
            }
            NodeKind::LeastBusy { cluster } => {
                let (order, fresh) = select::least_busy(route, i, cx, &mut self.rng, *cluster);
                if *cluster && !fresh && !self.causes.contains(&"cluster_counters") {
                    self.causes.push("cluster_counters");
                }
                order
            }
            NodeKind::LeastCost => select::least_cost(route, i, cx),
        }
    }

    fn start(&mut self) {
        if !self.started {
            self.started = true;
            self.stack.push(Frame {
                node: None,
                order: Order::List {
                    items: vec![0],
                    pos: 0,
                },
            });
        }
    }

    /// Puts a bound position first when it is in the plan (inside the chosen branches) and
    /// eligible. Returns whether it was placed.
    pub fn prefer(&mut self, node: usize, cx: &SelectCx<'_>, elig: &mut dyn Eligibility) -> bool {
        let chain = self.route.ancestry(node);
        for pair in chain.windows(2) {
            let [parent, child] = pair else { continue };
            let decides = self.route.node(*parent).is_some_and(|n| n.kind.decides());
            if decides && self.decide(*parent, cx) != Some(*child) {
                return false;
            }
            if self
                .route
                .node(*parent)
                .is_some_and(|n| matches!(n.kind, NodeKind::Unsupported(_)))
            {
                return false;
            }
        }
        let Some(name) = leaf_ref(self.route, node, cx) else {
            return false;
        };
        if elig
            .check(Leaf {
                node,
                deployment: &name,
            })
            .is_err()
        {
            return false;
        }
        self.front = Some(node);
        true
    }

    /// The next eligible position.
    pub fn next(&mut self, cx: &SelectCx<'_>, elig: &mut dyn Eligibility) -> Option<Step> {
        if let Some(f) = self.front.take() {
            self.front_done = Some(f);
            if let Some(step) = self.visit(f, cx, elig) {
                return Some(step);
            }
        }
        self.start();
        loop {
            let frame = self.stack.last_mut()?;
            let Some(i) = frame.order.pop(self.route, cx, &mut self.rng) else {
                self.stack.pop();
                continue;
            };
            if Some(i) == self.front_done {
                continue;
            }
            let leaf = self.route.node(i).is_some_and(|n| n.kind.is_terminal());
            if leaf {
                if let Some(step) = self.visit(i, cx, elig) {
                    return Some(step);
                }
                continue;
            }
            let order = self.order(i, cx, elig);
            self.stack.push(Frame { node: Some(i), order });
        }
    }

    fn visit(&mut self, i: usize, cx: &SelectCx<'_>, elig: &mut dyn Eligibility) -> Option<Step> {
        if let Some(NodeKind::Block(b)) = self.route.node(i).map(|n| &n.kind) {
            // A block ends the request only where it is selected first; it is never a fallback.
            if std::mem::replace(&mut self.passed, true) {
                return None;
            }
            self.prev = Some(i);
            return Some(Step {
                node: i,
                deployment: String::new(),
                upstream_model: None,
                provider: None,
                route_path: self.route.node(i).map(|n| n.path.clone()).unwrap_or_default(),
                new_tier: false,
                block: Some(Arc::clone(b)),
            });
        }
        self.passed = true;
        let (deployment, upstream_model, provider, route_path) = self.leaf_name(i, cx)?;
        match elig.check(Leaf {
            node: i,
            deployment: &deployment,
        }) {
            Ok(()) => {
                let new_tier = self.prev.is_some_and(|p| self.route.lca_is_fallback(p, i));
                self.prev = Some(i);
                Some(Step {
                    node: i,
                    deployment,
                    upstream_model,
                    provider,
                    route_path,
                    new_tier,
                    block: None,
                })
            }
            Err(Exclusion::Tried) => {
                self.prev = Some(i);
                None
            }
            Err(e) => {
                self.record(&e);
                None
            }
        }
    }

    /// Skips what is left of the current tier: up to the next child of a `fallback` node.
    pub fn skip_tier(&mut self) {
        while let Some(top) = self.stack.last() {
            let fallback_with_more = top
                .node
                .is_some_and(|n| self.route.node(n).is_some_and(|x| x.kind.is_fallback()))
                && top.order.has_more(self.route);
            if fallback_with_more {
                return;
            }
            self.stack.pop();
        }
    }

    /// The transition label between two positions.
    #[must_use]
    pub fn label(&self, from: usize, to: usize) -> Option<String> {
        self.route.label(from, to)
    }

    /// Why nothing was eligible.
    #[must_use]
    pub fn exhaustion(&self) -> &Exhaustion {
        &self.exhaustion
    }
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
