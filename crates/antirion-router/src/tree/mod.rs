//! Route trees compiled into an arena: node types, nesting, `route` references inlined, node ids
//! and route paths, per-node params and policies, and the config-time validation of trees.

use std::sync::Arc;
use std::time::Duration;

use antirion_core::UtcMicros;
use serde_json::Value;

use crate::body_edit::BodyEdit;
use crate::content::{ClassifierSpec, CostQualitySpec};
use crate::params::Params;
use crate::pool::PoolSpec;
use crate::predicate::Predicate;

mod build;

pub(crate) use self::build::{Ctx, compile_route};

/// The deepest tree, counted in nodes from the root to a leaf, through references.
pub const MAX_DEPTH: usize = 16;

/// How a `weighted` node splits.
#[derive(Clone, Debug, PartialEq)]
pub enum Algorithm {
    SmoothWrr,
    Random,
    /// A bandit experiment.
    Thompson(Arc<crate::bandit::BanditSpec>),
}

/// An A/B test on a `weighted` node: its children's ids are the arms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Experiment {
    pub name: String,
    /// The assignment is returned to the client.
    pub expose: bool,
}

/// One rule of a `conditional` node.
#[derive(Clone, Debug)]
pub struct Rule {
    /// `None` for a last rule without `when`, which always holds.
    pub when: Option<Predicate>,
    pub name: Option<String>,
    /// A disabled rule is skipped at evaluation, as if absent.
    pub enabled: bool,
}

/// A conditional rule a position was reached through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleHit {
    /// The conditional node's path.
    pub node: String,
    pub index: usize,
    pub name: Option<String>,
}

/// The answer of a `block` node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub status: u16,
    pub message: String,
    pub category: String,
}

/// A `tier` node: the enabled Models of a tier, each standing for its route tree, selected as the
/// node of `strategy` would.
#[derive(Clone, Debug)]
pub struct TierSpec {
    pub tier: String,
    /// `least_cost`, `least_latency`, `weighted` or `fallback`.
    pub strategy: &'static str,
    /// The node kind that orders the members.
    pub selects: Box<NodeKind>,
}

/// What a node is.
#[derive(Clone, Debug)]
pub enum NodeKind {
    /// A deployment, by name.
    Deployment(String),
    /// A provider and an upstream model id, templated under a wildcard Model.
    ProviderModel {
        provider: String,
        template: String,
    },
    /// An inlined reference to another route (or a Model's anonymous route); one child.
    Route(String),
    Fallback,
    Weighted {
        algorithm: Algorithm,
        sticky: Option<String>,
        experiment: Option<Arc<Experiment>>,
        canary: Option<Arc<Canary>>,
    },
    LeastLatency {
        decay: Duration,
    },
    /// `cluster`: in-flight attempts of every node count, through the counter store.
    LeastBusy {
        cluster: bool,
    },
    LeastCost,
    /// Children are the rule targets in order, then the default when there is one.
    Conditional {
        rules: Vec<Rule>,
        has_default: bool,
    },
    /// Children are the class targets in order, then the default.
    Classifier(Arc<ClassifierSpec>),
    /// Children are `strong` then `weak`.
    CostQuality(Arc<CostQualitySpec>),
    /// Children are the endpoints, all leaves.
    Pool(Arc<PoolSpec>),
    /// Ends the request as a guardrail block, with no upstream call.
    Block(Arc<Block>),
    /// Children are the tier's member Models, in name order.
    Tier(Arc<TierSpec>),
    /// A tier member: one child, the Model's route tree, ineligible from its end of life.
    Member {
        model: String,
        eol_at: Option<UtcMicros>,
    },
    /// A node type this build does not route through; it never yields a leaf.
    Unsupported(&'static str),
}

impl NodeKind {
    /// The `type` name, as transition labels show it.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            NodeKind::Deployment(_) => "deployment",
            NodeKind::ProviderModel { .. } => "provider_model",
            NodeKind::Route(_) => "route",
            NodeKind::Fallback => "fallback",
            NodeKind::Weighted { .. } => "weighted",
            NodeKind::LeastLatency { .. } => "least_latency",
            NodeKind::LeastBusy { .. } => "least_busy",
            NodeKind::LeastCost => "least_cost",
            NodeKind::Conditional { .. } => "conditional",
            NodeKind::Classifier(_) => "classifier",
            NodeKind::CostQuality(_) => "cost_quality",
            NodeKind::Pool(_) => "pool",
            NodeKind::Block(_) => "block",
            NodeKind::Tier(_) => "tier",
            NodeKind::Member { .. } => "model",
            NodeKind::Unsupported(t) => t,
        }
    }

    /// The plan stops here: a leaf, or a block.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.is_leaf() || matches!(self, NodeKind::Block(_))
    }

    /// Crossing the node between positions starts a new tier of the plan.
    #[must_use]
    pub fn is_fallback(&self) -> bool {
        match self {
            NodeKind::Fallback => true,
            NodeKind::Tier(t) => matches!(*t.selects, NodeKind::Fallback),
            _ => false,
        }
    }

    #[must_use]
    pub fn is_leaf(&self) -> bool {
        matches!(self, NodeKind::Deployment(_) | NodeKind::ProviderModel { .. })
    }

    /// The node chooses one branch from the request (its decision is taken once per request).
    #[must_use]
    pub fn decides(&self) -> bool {
        matches!(
            self,
            NodeKind::Conditional { .. } | NodeKind::Classifier(_) | NodeKind::CostQuality(_)
        )
    }

    /// The node's decision needs a call outside the route (a classifier or a scorer).
    #[must_use]
    pub fn calls(&self) -> bool {
        match self {
            NodeKind::Classifier(c) => c.calls(),
            NodeKind::CostQuality(q) => q.calls(),
            NodeKind::Pool(p) => p.picker.is_some(),
            _ => false,
        }
    }
}

/// A canary on a `weighted` node: one child's share follows a ramp of steps, each a percentage of
/// the node's traffic from some time after the configuration was applied, and an error budget can
/// stop or reverse the ramp.
#[derive(Clone, Debug, PartialEq)]
pub struct Canary {
    /// Identifies the canary's state: the node's path and the canary's configuration.
    pub key: String,
    /// The candidate's position among the node's children.
    pub child: usize,
    /// `(after, percent)`, in ascending `after`.
    pub steps: Vec<(Duration, u32)>,
    pub budget: Option<ErrorBudget>,
}

/// When a canary's candidate fails too often.
#[derive(Clone, Debug, PartialEq)]
pub struct ErrorBudget {
    pub max_error_rate: f64,
    pub window: Duration,
    /// `rollback` (else `hold`).
    pub rollback: bool,
}

impl Canary {
    /// The candidate's ramp percentage `elapsed` after the configuration was applied.
    #[must_use]
    pub fn ramp(&self, elapsed: Duration) -> u32 {
        self.steps
            .iter()
            .take_while(|(after, _)| *after <= elapsed)
            .last()
            .map_or(0, |(_, w)| *w)
    }

    /// The ramp has reached its last step.
    #[must_use]
    pub fn complete(&self, elapsed: Duration) -> bool {
        self.steps.last().is_some_and(|(after, _)| *after <= elapsed)
    }
}

/// A node's shadow traffic: a copy of a share of its requests goes to the first eligible position
/// of `target`, whose answer is read and discarded.
#[derive(Clone, Debug)]
pub struct Mirror {
    /// The target node, compiled as a tree of its own.
    pub target: CompiledRoute,
    pub fraction: f64,
    /// The share is drawn per session value instead of per request.
    pub by_session: bool,
    /// Copies in flight per node on this replica; more are dropped.
    pub max_in_flight: u32,
}

/// One node of a compiled route.
#[derive(Clone, Debug)]
pub struct Node {
    pub kind: NodeKind,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub weight: u32,
    /// The segment this node adds to route paths (its id, index, rule or class label), if any.
    pub segment: Option<String>,
    /// The node's path: the route name and the segments down to this node.
    pub path: String,
    pub params: Option<Params>,
    /// The node's `policies` block.
    pub policies: Option<Value>,
    pub mirror: Option<Arc<Mirror>>,
    /// Identifies the node's selection state across reloads: path, type, children and weights.
    pub state_key: String,
    /// A leaf's tags, for tag filtering.
    pub tags: Vec<String>,
    /// A leaf's edits of the final upstream body.
    pub body_edit: Option<Arc<BodyEdit>>,
}

/// A route compiled into an arena; node 0 is the root.
#[derive(Clone, Debug)]
pub struct CompiledRoute {
    pub name: String,
    pub nodes: Vec<Node>,
}

impl CompiledRoute {
    #[must_use]
    pub fn node(&self, i: usize) -> Option<&Node> {
        self.nodes.get(i)
    }

    /// Every leaf index, in declaration order.
    #[must_use]
    pub fn leaves(&self) -> Vec<usize> {
        let mut out = Vec::new();
        self.collect_leaves(0, &mut out);
        out
    }

    fn collect_leaves(&self, i: usize, out: &mut Vec<usize>) {
        let Some(n) = self.nodes.get(i) else { return };
        if n.kind.is_leaf() {
            out.push(i);
        }
        for c in &n.children {
            self.collect_leaves(*c, out);
        }
    }

    /// The nodes from the root down to `i`, root first.
    #[must_use]
    pub fn ancestry(&self, i: usize) -> Vec<usize> {
        let mut chain = vec![i];
        let mut cur = i;
        while let Some(p) = self.nodes.get(cur).and_then(|n| n.parent) {
            chain.push(p);
            cur = p;
        }
        chain.reverse();
        chain
    }

    /// The lowest common ancestor of two nodes, as its transition label `type@path`.
    #[must_use]
    pub fn label(&self, a: usize, b: usize) -> Option<String> {
        let (x, y) = (self.ancestry(a), self.ancestry(b));
        let lca = x
            .iter()
            .zip(y.iter())
            .take_while(|(p, q)| p == q)
            .last()
            .map(|(p, _)| *p)?;
        let n = self.nodes.get(lca)?;
        Some(format!("{}@{}", n.kind.type_name(), n.path))
    }

    /// The node type of the lowest common ancestor of two nodes.
    #[must_use]
    pub fn lca_is_fallback(&self, a: usize, b: usize) -> bool {
        let (x, y) = (self.ancestry(a), self.ancestry(b));
        x.iter()
            .zip(y.iter())
            .take_while(|(p, q)| p == q)
            .last()
            .and_then(|(p, _)| self.nodes.get(*p))
            .is_some_and(|n| n.kind.is_fallback())
    }

    /// The conditional rules on the way from the root to node `i`: each rule's node path, index
    /// and name. A default branch is not a rule.
    #[must_use]
    pub fn rules_to(&self, i: usize) -> Vec<RuleHit> {
        let chain = self.ancestry(i);
        let mut out = Vec::new();
        for pair in chain.windows(2) {
            let [parent, child] = pair else { continue };
            let Some(p) = self.nodes.get(*parent) else {
                continue;
            };
            let NodeKind::Conditional { rules, .. } = &p.kind else {
                continue;
            };
            if let Some(index) = p.children.iter().position(|c| c == child)
                && let Some(rule) = rules.get(index)
            {
                out.push(RuleHit {
                    node: p.path.clone(),
                    index,
                    name: rule.name.clone(),
                });
            }
        }
        out
    }

    /// Whether a deployment is one of the route's leaves.
    #[must_use]
    pub fn has_deployment(&self, name: &str) -> bool {
        self.nodes
            .iter()
            .any(|n| matches!(&n.kind, NodeKind::Deployment(d) if d == name))
    }
}

#[cfg(test)]
mod tests;
