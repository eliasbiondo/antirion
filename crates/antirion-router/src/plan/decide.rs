//! The decisions of conditional, classifier and cost-quality nodes: taken once per request. Those
//! that need a call (or replay an earlier decision) are taken by the caller before the plan is
//! walked and handed in with [`AttemptPlan::set_decision`]; the plan takes the others itself.

use crate::classifier::routing_text;
use crate::content::Classify;
use crate::plan::{AttemptPlan, SelectCx};
use crate::tree::NodeKind;

impl AttemptPlan<'_> {
    /// The decision of a deciding node: the chosen child.
    pub(super) fn decide(&mut self, i: usize, cx: &SelectCx<'_>) -> Option<usize> {
        if let Some(d) = self.decisions.get(&i) {
            return *d;
        }
        let n = self.route.node(i)?;
        let choice = match &n.kind {
            NodeKind::Conditional { rules, has_default } => {
                let hit = rules
                    .iter()
                    .position(|r| r.enabled && r.when.as_ref().is_none_or(|p| p.eval(cx.facts)));
                match hit {
                    Some(k) => n.children.get(k).copied(),
                    None if *has_default => n.children.last().copied(),
                    None => {
                        self.exhaustion.no_match = true;
                        None
                    }
                }
            }
            NodeKind::Classifier(spec) => {
                let class = match &spec.how {
                    Classify::Local(matcher) => cx
                        .facts
                        .body()
                        .and_then(|b| routing_text(b, &spec.input))
                        .and_then(|t| matcher.classify(&t)),
                    _ => None,
                };
                let name = class
                    .and_then(|k| spec.classes.get(k))
                    .map_or("default", String::as_str);
                self.classes.push((n.path.clone(), name.to_owned()));
                match class {
                    Some(k) => n.children.get(k).copied(),
                    None => n.children.last().copied(),
                }
            }
            NodeKind::CostQuality(_) => n.children.first().copied(),
            _ => None,
        };
        self.decisions.insert(i, choice);
        choice
    }

    /// Takes the decision of node `i` now, as the plan would, and returns the chosen child.
    pub fn decide_now(&mut self, i: usize, cx: &SelectCx<'_>) -> Option<usize> {
        self.decide(i, cx)
    }

    /// Hands in the decision of node `i`: its child at position `child` (for a classifier, the
    /// class index, or the class count for `default`; for a cost-quality node, 0 strong, 1 weak).
    pub fn set_decision(&mut self, i: usize, child: usize) {
        let chosen = self.route.node(i).and_then(|n| n.children.get(child).copied());
        self.decisions.insert(i, chosen);
    }

    /// Hands in what pool `i`'s external picker chose: its children at positions `children`, in
    /// order (none when it chose no endpoint); `None` leaves the pool to the built-in scheduler.
    pub fn set_picked(&mut self, i: usize, children: Option<&[usize]>) {
        self.decisions.insert(i, None);
        if let (Some(n), Some(children)) = (self.route.node(i), children) {
            let chosen = children
                .iter()
                .filter_map(|c| n.children.get(*c).copied())
                .collect();
            self.picked.insert(i, chosen);
        }
    }

    /// The nodes whose decision is taken ahead (a call or a replay) and not taken yet, reachable
    /// through the decisions already taken.
    pub fn pending(&mut self, cx: &SelectCx<'_>) -> Vec<usize> {
        let route = self.route;
        let mut out = Vec::new();
        let mut stack = vec![0usize];
        while let Some(i) = stack.pop() {
            let Some(n) = route.node(i) else { continue };
            let ahead = n.kind.calls()
                || matches!(&n.kind, NodeKind::Classifier(c) if c.replay)
                || matches!(&n.kind, NodeKind::CostQuality(_));
            if ahead && !self.decisions.contains_key(&i) {
                out.push(i);
                continue;
            }
            if n.kind.decides() {
                stack.extend(self.decide(i, cx));
                continue;
            }
            if matches!(&n.kind, NodeKind::Pool(_)) {
                continue;
            }
            stack.extend(n.children.iter().rev().copied());
        }
        out
    }
}
