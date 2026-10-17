//! A route evaluated against recorded request facts: which rule each record would match, which
//! position it would be served by, which it would block, with every deployment taken as healthy and
//! selection state kept apart from the live one. Rules that read content a record does not carry
//! (the body, or a guardrail's result) cannot be decided and are counted as unknown.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use antirion_core::UtcMicros;
use serde_json::Value;

use crate::plan::{AttemptPlan, CostOracle, Eligibility, Exclusion, Leaf, SelectCx};
use crate::predicate::Facts;
use crate::state::{RoutingState, Slot};
use crate::tree::{CompiledRoute, NodeKind};

/// One rule's tally.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleTally {
    /// The rule target's route path.
    pub path: String,
    pub name: Option<String>,
    pub count: u64,
    pub unknown: u64,
}

/// The outcome of evaluating records against one route.
#[derive(Debug)]
pub struct Simulation<'r> {
    route: &'r CompiledRoute,
    state: RoutingState,
    pub evaluated: u64,
    /// Per conditional node, its rules in order.
    rules: BTreeMap<usize, Vec<RuleTally>>,
    /// `(route path, deployment)` of each first position, with its count.
    pub leaves: BTreeMap<(String, String), u64>,
    pub blocked: u64,
    pub no_route: u64,
}

/// Facts of a record with what it lacks noted: content selectors read without content, and
/// guardrail results, which are never computed here.
struct Probe<'a> {
    inner: &'a (dyn Facts + Sync),
    has_content: bool,
    unknowable: AtomicBool,
}

impl Facts for Probe<'_> {
    fn header(&self, name: &str) -> Option<String> {
        self.inner.header(name)
    }
    fn body(&self) -> Option<&Value> {
        if !self.has_content {
            self.unknowable.store(true, Ordering::Relaxed);
        }
        self.inner.body()
    }
    fn metadata(&self) -> Option<&Value> {
        self.inner.metadata()
    }
    fn request(&self, field: &str) -> Option<Value> {
        self.inner.request(field)
    }
    fn identity(&self, field: &str) -> Option<Value> {
        self.inner.identity(field)
    }
    fn sample_point(&self) -> Option<f64> {
        self.inner.sample_point()
    }
    fn guard(&self, _name: &str, _field: &str) -> Option<Value> {
        self.unknowable.store(true, Ordering::Relaxed);
        None
    }
}

/// Every leaf is healthy and allowed.
struct AllHealthy;

impl Eligibility for AllHealthy {
    fn check(&mut self, _leaf: Leaf<'_>) -> Result<(), Exclusion> {
        Ok(())
    }
}

struct Unpriced;

impl CostOracle for Unpriced {
    fn price(&self, _deployment: &str) -> Option<f64> {
        None
    }
}

impl<'r> Simulation<'r> {
    #[must_use]
    pub fn new(route: &'r CompiledRoute) -> Simulation<'r> {
        let mut rules = BTreeMap::new();
        for (i, n) in route.nodes.iter().enumerate() {
            if let NodeKind::Conditional { rules: list, .. } = &n.kind {
                let tallies = list
                    .iter()
                    .zip(n.children.iter())
                    .map(|(r, c)| RuleTally {
                        path: route.node(*c).map(|x| x.path.clone()).unwrap_or_default(),
                        name: r.name.clone(),
                        count: 0,
                        unknown: 0,
                    })
                    .collect();
                rules.insert(i, tallies);
            }
        }
        Simulation {
            route,
            state: RoutingState::default(),
            evaluated: 0,
            rules,
            leaves: BTreeMap::new(),
            blocked: 0,
            no_route: 0,
        }
    }

    /// Every rule, in tree order.
    #[must_use]
    pub fn rules(&self) -> Vec<&RuleTally> {
        self.rules.values().flatten().collect()
    }

    /// Evaluates one record; `has_content` when its body was stored.
    pub fn add(&mut self, facts: &(dyn Facts + Sync), has_content: bool, hash_seed: u64, now: UtcMicros) {
        self.evaluated += 1;
        let probe = Probe {
            inner: facts,
            has_content,
            unknowable: AtomicBool::new(false),
        };
        let route = self.route;
        let mut plan = AttemptPlan::new(route, self.evaluated);
        let mut stack = vec![0usize];
        while let Some(i) = stack.pop() {
            let Some(n) = route.node(i) else { continue };
            match &n.kind {
                NodeKind::Conditional { rules, has_default } => {
                    let mut chosen = None;
                    for (k, r) in rules.iter().enumerate() {
                        if !r.enabled {
                            continue;
                        }
                        probe.unknowable.store(false, Ordering::Relaxed);
                        let holds = r.when.as_ref().is_none_or(|p| p.eval(&probe));
                        let tally = self.rules.get_mut(&i).and_then(|t| t.get_mut(k));
                        if probe.unknowable.load(Ordering::Relaxed) {
                            if let Some(t) = tally {
                                t.unknown += 1;
                            }
                            continue;
                        }
                        if holds {
                            if let Some(t) = tally {
                                t.count += 1;
                            }
                            chosen = Some(k);
                            break;
                        }
                    }
                    let child = match chosen {
                        Some(k) => Some(k),
                        None if *has_default => Some(n.children.len().saturating_sub(1)),
                        None => None,
                    };
                    match child {
                        Some(k) => {
                            plan.set_decision(i, k);
                            stack.extend(n.children.get(k).copied());
                        }
                        None => plan.set_decision(i, usize::MAX),
                    }
                }
                _ => stack.extend(n.children.iter().copied()),
            }
        }
        let sticky = |_: &str| None;
        let cx = SelectCx {
            state: &self.state,
            facts: &probe,
            cost: &Unpriced,
            slot: Slot::E2e,
            default_estimate: [500.0, 50.0, 500.0],
            hash_seed,
            sticky: &sticky,
            captures: None,
            now,
            dry: false,
        };
        match plan.next(&cx, &mut AllHealthy) {
            Some(step) if step.block.is_some() => self.blocked += 1,
            Some(step) => {
                *self.leaves.entry((step.route_path, step.deployment)).or_default() += 1;
            }
            None => self.no_route += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    struct Rec {
        tags: Value,
        body: Option<Value>,
    }

    impl Facts for Rec {
        fn header(&self, _: &str) -> Option<String> {
            None
        }
        fn body(&self) -> Option<&Value> {
            self.body.as_ref()
        }
        fn metadata(&self) -> Option<&Value> {
            None
        }
        fn request(&self, _: &str) -> Option<Value> {
            None
        }
        fn identity(&self, field: &str) -> Option<Value> {
            (field == "tags").then(|| self.tags.clone())
        }
    }

    #[test]
    fn rules_leaves_blocks_and_unknowns_are_counted() {
        let doc = json!({
            "deployments": {"a": {}, "b": {}},
            "routes": {"r": {"tree": {"type": "conditional", "rules": [
                {"name": "batch", "when": {"field": "identity.tags", "contains": "batch"}, "target": "a"},
                {"name": "secret", "when": {"field": "body.secret", "eq": true}, "target": {"type": "block"}}
            ], "default": "b"}}}
        });
        let snap = crate::compile(&doc).unwrap();
        let tree = &snap.routes["r"].tree;
        let mut sim = Simulation::new(tree);
        for i in 0..10 {
            let tags = if i < 3 { json!(["batch"]) } else { json!([]) };
            sim.add(&Rec { tags, body: None }, false, 1, UtcMicros(0));
        }
        sim.add(
            &Rec {
                tags: json!([]),
                body: Some(json!({"secret": true})),
            },
            true,
            1,
            UtcMicros(0),
        );
        let rules = sim.rules();
        assert_eq!((rules[0].count, rules[0].unknown), (3, 0));
        assert_eq!((rules[1].count, rules[1].unknown), (1, 7));
        assert_eq!(sim.evaluated, 11);
        assert_eq!(sim.blocked, 1);
        assert_eq!(sim.leaves.get(&("r/r0/a".to_owned(), "a".to_owned())), Some(&3));
        assert_eq!(
            sim.leaves.get(&("r/default/b".to_owned(), "b".to_owned())),
            Some(&7)
        );
    }
}
