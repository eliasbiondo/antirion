//! The node plan of a bandit experiment: the arm the bandit chooses, then the other arms in
//! declaration order.

use crate::bandit::BanditSpec;
use crate::plan::{AttemptPlan, Eligibility, SelectCx};
use crate::select::Order;
use crate::tree::Experiment;

impl AttemptPlan<'_> {
    pub(super) fn bandit_order(
        &mut self,
        i: usize,
        spec: &BanditSpec,
        experiment: Option<&Experiment>,
        cx: &SelectCx<'_>,
        elig: &mut dyn Eligibility,
    ) -> Order {
        let route = self.route;
        let (Some(n), Some(e)) = (route.node(i), experiment) else {
            return Order::EMPTY;
        };
        let mut arms: Vec<(usize, &str)> = Vec::with_capacity(n.children.len());
        for &c in &n.children {
            let Some(child) = route.node(c) else { continue };
            let Some(arm) = child.segment.as_deref() else {
                continue;
            };
            let listed = spec
                .arms
                .as_ref()
                .map_or(child.weight > 0, |a| a.iter().any(|x| x == arm));
            if listed && self.reachable(c, cx, elig) {
                arms.push((c, arm));
            }
        }
        let names: Vec<&str> = arms.iter().map(|(_, a)| *a).collect();
        let Some(first) = cx
            .state
            .bandits
            .choose(&e.name, spec, &names, &mut self.rng, cx.dry)
        else {
            return Order::EMPTY;
        };
        let mut items: Vec<usize> = arms.get(first).map(|(c, _)| *c).into_iter().collect();
        items.extend(
            arms.iter()
                .enumerate()
                .filter(|(k, _)| *k != first)
                .map(|(_, (c, _))| *c),
        );
        Order::List { items, pos: 0 }
    }
}
