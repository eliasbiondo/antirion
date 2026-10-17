//! The node plan of a pool: the eligible endpoints that pass the queue and KV filters by
//! descending score (ties at random), then those the filters removed, by score; and what the data
//! plane needs to count an attempt dispatched to one.

use std::time::Instant;

use crate::plan::{AttemptPlan, Eligibility, SelectCx};
use crate::pool::{Candidate, PoolSpec, prefix, rank};
use crate::select::{Order, Ranked, leaf_ref};
use crate::tree::NodeKind;

/// An attempt to an endpoint of a pool: the pool's state key, and the prompt's chunk hashes the
/// endpoint learns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolDispatch {
    pub pool: String,
    pub hashes: Vec<u64>,
    pub entries: usize,
}

impl AttemptPlan<'_> {
    pub(super) fn pool_order(
        &mut self,
        i: usize,
        spec: &PoolSpec,
        cx: &SelectCx<'_>,
        elig: &mut dyn Eligibility,
    ) -> Order {
        let route = self.route;
        let Some(n) = route.node(i) else {
            return Order::EMPTY;
        };
        if let Some(picked) = self.picked.get(&i).cloned() {
            let items = picked
                .into_iter()
                .filter(|c| leaf_ref(route, *c, cx).is_some() && self.reachable(*c, cx, elig))
                .collect();
            return Order::List { items, pos: 0 };
        }
        let hashes = cx
            .facts
            .body()
            .map(|b| prefix::chain(&n.state_key, b, spec.chunk_chars, spec.max_chunks))
            .unwrap_or_default();
        let now = Instant::now();
        let pools = cx.state.pools();
        let mut eligible = Vec::new();
        let mut names = Vec::new();
        let mut rest = Vec::new();
        for &c in &n.children {
            match leaf_ref(route, c, cx).filter(|_| self.reachable(c, cx, elig)) {
                Some(name) => {
                    eligible.push(c);
                    names.push(name);
                }
                None => rest.push(c),
            }
        }
        let refs: Vec<&str> = names.iter().map(|n| n.as_ref()).collect();
        let views = pools.views(&n.state_key, &refs, &hashes, spec.stale_after(), now);
        let mut stale_seen = false;
        let cands: Vec<Candidate> = views
            .iter()
            .zip(&refs)
            .map(|(view, name)| match &view.fresh {
                Some((r, assumed)) => Candidate {
                    queue: r.waiting + f64::from(*assumed),
                    kv: r.kv.unwrap_or(0.5),
                    stale: r.kv.is_none(),
                    prefix: view.prefix,
                    lora: if spec.lora {
                        r.lora_fit()
                    } else {
                        crate::pool::LoraFit::Unknown
                    },
                },
                None => {
                    stale_seen = true;
                    Candidate {
                        queue: f64::from(cx.state.inflight(name)),
                        kv: 0.5,
                        stale: true,
                        prefix: view.prefix,
                        lora: crate::pool::LoraFit::Unknown,
                    }
                }
            })
            .collect();
        if stale_seen && !self.causes.contains(&"pool_metrics") {
            self.causes.push("pool_metrics");
        }
        self.pools.insert(i, hashes);
        let mut items: Vec<Ranked> = rank(spec, &cands)
            .into_iter()
            .zip(eligible)
            .map(|((kept, score), child)| Ranked {
                key: if kept { -score } else { 1e9 - score },
                tie: self.rng.next_u64(),
                child,
            })
            .collect();
        items.extend(rest.into_iter().map(|child| Ranked {
            key: f64::INFINITY,
            tie: self.rng.next_u64(),
            child,
        }));
        Order::Ranked(items)
    }

    /// The pool dispatch of a leaf, when its parent is a pool the plan ordered.
    #[must_use]
    pub fn pool_dispatch(&self, leaf: usize) -> Option<PoolDispatch> {
        let parent = self.route.node(leaf)?.parent?;
        let p = self.route.node(parent)?;
        let NodeKind::Pool(spec) = &p.kind else {
            return None;
        };
        Some(PoolDispatch {
            pool: p.state_key.clone(),
            hashes: self.pools.get(&parent).cloned().unwrap_or_default(),
            entries: spec.entries,
        })
    }
}
