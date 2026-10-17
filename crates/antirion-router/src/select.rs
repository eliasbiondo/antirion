//! The order of a node's children as the plan reads it: lazily, so choosing the first position
//! costs at most one pass over the children, and each further position at most one more. Children
//! are not filtered here: eligibility is checked when the plan reaches a leaf.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::time::{Duration, Instant};

use antirion_core::SplitMix64;

use crate::plan::SelectCx;
use crate::state::Slot;
use crate::tree::{CompiledRoute, NodeKind};

/// A child with its sort key: lower first, ties by `tie`.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Ranked {
    pub(crate) key: f64,
    pub(crate) tie: u64,
    pub(crate) child: usize,
}

fn before(a: &Ranked, b: &Ranked) -> bool {
    match a.key.partial_cmp(&b.key).unwrap_or(Ordering::Equal) {
        Ordering::Less => true,
        Ordering::Greater => false,
        Ordering::Equal => a.tie < b.tie,
    }
}

#[derive(Debug)]
pub(crate) enum Order {
    /// Positions in a fixed order.
    List { items: Vec<usize>, pos: usize },
    /// The children of `node` in declaration order.
    Children { node: usize, pos: usize },
    /// `head` first, then weighted sampling without replacement from `pool`.
    Sample {
        head: Option<usize>,
        pool: Vec<(usize, u32)>,
        total: u64,
    },
    /// Lowest key first.
    Ranked(Vec<Ranked>),
    /// The power-of-two-choices winner, then the other children of `node` by latency cost,
    /// costed when the plan first goes past the winner.
    Latency {
        node: usize,
        decay: Duration,
        winner: usize,
        given: bool,
        rest: Option<(Vec<usize>, usize)>,
    },
}

impl Order {
    pub(crate) const EMPTY: Order = Order::List {
        items: Vec::new(),
        pos: 0,
    };

    /// Whether a position is left.
    pub(crate) fn has_more(&self, route: &CompiledRoute) -> bool {
        match self {
            Order::List { items, pos } => *pos < items.len(),
            Order::Children { node, pos } => route.node(*node).is_some_and(|n| *pos < n.children.len()),
            Order::Sample { head, pool, .. } => head.is_some() || !pool.is_empty(),
            Order::Ranked(items) => !items.is_empty(),
            Order::Latency { given, rest, .. } => {
                !*given || rest.as_ref().is_none_or(|(items, pos)| *pos < items.len())
            }
        }
    }

    /// The next position.
    pub(crate) fn pop(
        &mut self,
        route: &CompiledRoute,
        cx: &SelectCx<'_>,
        rng: &mut SplitMix64,
    ) -> Option<usize> {
        match self {
            Order::List { items, pos } => {
                let next = items.get(*pos).copied();
                *pos += 1;
                next
            }
            Order::Children { node, pos } => {
                let next = route.node(*node)?.children.get(*pos).copied();
                *pos += 1;
                next
            }
            Order::Sample { head, pool, total } => {
                if let Some(h) = head.take() {
                    return Some(h);
                }
                if pool.is_empty() {
                    return None;
                }
                let mut u = rng.below((*total).max(1));
                let mut pick = 0;
                for (k, (_, w)) in pool.iter().enumerate() {
                    if u < u64::from(*w) {
                        pick = k;
                        break;
                    }
                    u -= u64::from(*w);
                }
                let (child, w) = pool.swap_remove(pick);
                *total = total.saturating_sub(u64::from(w));
                Some(child)
            }
            Order::Ranked(items) => {
                let mut best: Option<(usize, Ranked)> = None;
                for (k, r) in items.iter().enumerate() {
                    if best.is_none_or(|(_, b)| before(r, &b)) {
                        best = Some((k, *r));
                    }
                }
                let (k, r) = best?;
                items.swap_remove(k);
                Some(r.child)
            }
            Order::Latency {
                node,
                decay,
                winner,
                given,
                rest,
            } => {
                if !*given {
                    *given = true;
                    return Some(*winner);
                }
                let (items, pos) =
                    rest.get_or_insert_with(|| (latency_rest(route, *node, *winner, cx, *decay), 0));
                let next = items.get(*pos).copied();
                *pos += 1;
                next
            }
        }
    }
}

/// The other children of a `least_latency` node, by ascending cost (declaration order on ties).
fn latency_rest(
    route: &CompiledRoute,
    node: usize,
    winner: usize,
    cx: &SelectCx<'_>,
    decay: Duration,
) -> Vec<usize> {
    let Some(n) = route.node(node) else {
        return Vec::new();
    };
    let now = Instant::now();
    let mut costed: Vec<(usize, f64)> = n
        .children
        .iter()
        .copied()
        .filter(|c| *c != winner)
        .map(|c| (c, latency_cost(route, c, cx, decay, now)))
        .collect();
    costed.sort_by(|x, y| x.1.partial_cmp(&y.1).unwrap_or(Ordering::Equal));
    costed.into_iter().map(|(c, _)| c).collect()
}

/// Calls `f` on every leaf at or under node `i`.
pub(crate) fn for_leaves(route: &CompiledRoute, i: usize, f: &mut impl FnMut(usize)) {
    let Some(n) = route.node(i) else { return };
    if n.kind.is_leaf() {
        f(i);
    }
    for c in &n.children {
        for_leaves(route, *c, f);
    }
}

/// The deployment name of leaf `i` (`provider/upstream_model` for a `provider_model` leaf).
pub(crate) fn leaf_ref<'r>(route: &'r CompiledRoute, i: usize, cx: &SelectCx<'_>) -> Option<Cow<'r, str>> {
    match &route.node(i)?.kind {
        NodeKind::Deployment(d) => Some(Cow::Borrowed(d.as_str())),
        NodeKind::ProviderModel { provider, template } => {
            let upstream = cx
                .captures
                .map_or_else(|| template.clone(), |c| c.render(template));
            Some(Cow::Owned(format!("{provider}/{upstream}")))
        }
        _ => None,
    }
}

pub(crate) fn slot_ix(s: Slot) -> usize {
    match s {
        Slot::Ttft => 0,
        Slot::PerToken => 1,
        Slot::E2e => 2,
    }
}

/// The smallest latency cost among the leaves under a child.
pub(crate) fn latency_cost(
    route: &CompiledRoute,
    c: usize,
    cx: &SelectCx<'_>,
    tau: Duration,
    now: Instant,
) -> f64 {
    let default = cx
        .default_estimate
        .get(slot_ix(cx.slot))
        .copied()
        .unwrap_or(500.0);
    let mut best = f64::INFINITY;
    for_leaves(route, c, &mut |l| {
        if let Some(d) = leaf_ref(route, l, cx) {
            let est = cx.state.estimate(&d, cx.slot, tau, default, now);
            let pending = f64::from(cx.state.inflight(&d));
            best = best.min(est * (pending + 1.0) / cx.state.success(&d).max(0.05));
        }
    });
    best
}

/// `least_busy`: by in-flight attempts over weight, ties at random. With `cluster`, the other
/// nodes' attempts count too; the flag is false when their counts were not fresh (this node's
/// alone were used).
pub(crate) fn least_busy(
    route: &CompiledRoute,
    node: usize,
    cx: &SelectCx<'_>,
    rng: &mut SplitMix64,
    cluster: bool,
) -> (Order, bool) {
    let Some(n) = route.node(node) else {
        return (Order::EMPTY, true);
    };
    let now = Instant::now();
    let mut fresh = true;
    let remote = |d: &str| -> u32 {
        if !cluster {
            return 0;
        }
        cx.state.remote_inflight(d, now).unwrap_or(0)
    };
    if cluster && !n.children.is_empty() {
        fresh = cx.state.remote_inflight("", now).is_some();
    }
    let items = cx.state.with_inflight(|inflight| {
        n.children
            .iter()
            .map(|&c| {
                let mut least: Option<u32> = None;
                for_leaves(route, c, &mut |l| {
                    if let Some(d) = leaf_ref(route, l, cx) {
                        let v = inflight(&d).saturating_add(remote(&d));
                        least = Some(least.map_or(v, |x| x.min(v)));
                    }
                });
                let w = route.node(c).map_or(1, |x| x.weight.max(1));
                Ranked {
                    key: f64::from(least.unwrap_or(0)) / f64::from(w),
                    tie: rng.next_u64(),
                    child: c,
                }
            })
            .collect()
    });
    (Order::Ranked(items), fresh)
}

/// `least_cost`: by the cheapest estimated price under each child, unpriced last, ties in
/// declaration order.
pub(crate) fn least_cost(route: &CompiledRoute, node: usize, cx: &SelectCx<'_>) -> Order {
    let Some(n) = route.node(node) else {
        return Order::EMPTY;
    };
    let items = n
        .children
        .iter()
        .enumerate()
        .map(|(k, &c)| {
            let mut price: Option<f64> = None;
            for_leaves(route, c, &mut |l| {
                if let Some(p) = leaf_ref(route, l, cx).and_then(|d| cx.cost.price(&d)) {
                    price = Some(price.map_or(p, |a| a.min(p)));
                }
            });
            Ranked {
                key: price.unwrap_or(f64::INFINITY),
                tie: (u64::from(price.is_none()) << 32) | k as u64,
                child: c,
            }
        })
        .collect();
    Order::Ranked(items)
}

/// A canary node's children weighted by the candidate's ramp: the candidate gets `percent` of the
/// traffic and the others share the rest in their configured proportions, in thousandths.
pub(crate) fn canary_weights(
    children: &[(usize, usize, u32)],
    canary: &crate::tree::Canary,
    percent: u32,
) -> Vec<(usize, usize, u32)> {
    let others: u64 = children
        .iter()
        .filter(|(k, ..)| *k != canary.child)
        .map(|(.., w)| u64::from(*w))
        .sum();
    let rest = u64::from(100u32.saturating_sub(percent)) * 1000;
    children
        .iter()
        .map(|&(k, c, w)| {
            let share = if k == canary.child {
                u64::from(percent) * 1000
            } else {
                (rest * u64::from(w)).checked_div(others).unwrap_or(0)
            };
            (k, c, u32::try_from(share).unwrap_or(u32::MAX))
        })
        .collect()
}

/// Weighted rendezvous hashing of a sticky value: by descending score, ties by index.
pub(crate) fn sticky(route: &CompiledRoute, node: usize, value: &str, seed: u64) -> Order {
    let Some(n) = route.node(node) else {
        return Order::EMPTY;
    };
    fn part(buf: &mut Vec<u8>, p: &[u8]) {
        buf.extend_from_slice(&(p.len() as u64).to_be_bytes());
        buf.extend_from_slice(p);
    }
    let mut buf = Vec::with_capacity(64);
    part(&mut buf, &seed.to_be_bytes());
    part(&mut buf, n.path.as_bytes());
    let prefix = buf.len();
    let items = n
        .children
        .iter()
        .enumerate()
        .filter_map(|(k, &c)| {
            let child = route.node(c)?;
            if child.weight == 0 {
                return None;
            }
            buf.truncate(prefix);
            part(&mut buf, child.segment.as_deref().unwrap_or_default().as_bytes());
            part(&mut buf, value.as_bytes());
            let h = xxhash_rust::xxh3::xxh3_64(&buf);
            let u = (h as f64 + 0.5) / 18_446_744_073_709_551_616.0;
            Some(Ranked {
                key: -(f64::from(child.weight) / -u.ln()),
                tie: k as u64,
                child: c,
            })
        })
        .collect();
    Order::Ranked(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranked_orders_by_key_then_tie() {
        let mut o = Order::Ranked(vec![
            Ranked {
                key: 2.0,
                tie: 0,
                child: 10,
            },
            Ranked {
                key: 1.0,
                tie: 5,
                child: 11,
            },
            Ranked {
                key: 1.0,
                tie: 1,
                child: 12,
            },
        ]);
        let route = CompiledRoute {
            name: "r".into(),
            nodes: Vec::new(),
        };
        let state = crate::state::RoutingState::default();
        let none = |_: &str| None;
        let cx = test_cx(&state, &none);
        let mut rng = SplitMix64::new(1);
        let got: Vec<usize> = std::iter::from_fn(|| o.pop(&route, &cx, &mut rng)).collect();
        assert_eq!(got, [12, 11, 10]);
    }

    #[test]
    fn sampling_without_replacement_yields_every_child_once() {
        let route = CompiledRoute {
            name: "r".into(),
            nodes: Vec::new(),
        };
        let state = crate::state::RoutingState::default();
        let none = |_: &str| None;
        let cx = test_cx(&state, &none);
        let mut rng = SplitMix64::new(7);
        let mut firsts = [0u32; 2];
        for _ in 0..4000 {
            let mut o = Order::Sample {
                head: None,
                pool: vec![(0, 3), (1, 1)],
                total: 4,
            };
            let a = o.pop(&route, &cx, &mut rng).unwrap();
            let b = o.pop(&route, &cx, &mut rng).unwrap();
            assert_ne!(a, b);
            assert!(o.pop(&route, &cx, &mut rng).is_none());
            firsts[a] += 1;
        }
        let share = f64::from(firsts[0]) / 4000.0;
        assert!((share - 0.75).abs() < 0.04, "{share}");
    }

    struct NoFacts;

    impl crate::predicate::Facts for NoFacts {
        fn header(&self, _: &str) -> Option<String> {
            None
        }
        fn body(&self) -> Option<&serde_json::Value> {
            None
        }
        fn metadata(&self) -> Option<&serde_json::Value> {
            None
        }
        fn request(&self, _: &str) -> Option<serde_json::Value> {
            None
        }
        fn identity(&self, _: &str) -> Option<serde_json::Value> {
            None
        }
    }

    struct NoCost;

    impl crate::plan::CostOracle for NoCost {
        fn price(&self, _: &str) -> Option<f64> {
            None
        }
    }

    fn test_cx<'a>(
        state: &'a crate::state::RoutingState,
        sticky: &'a (dyn Fn(&str) -> Option<String> + Sync),
    ) -> SelectCx<'a> {
        SelectCx {
            state,
            facts: &NoFacts,
            cost: &NoCost,
            slot: Slot::E2e,
            default_estimate: [500.0, 50.0, 500.0],
            hash_seed: 1,
            sticky,
            captures: None,
            now: antirion_core::UtcMicros(0),
            dry: false,
        }
    }
}
