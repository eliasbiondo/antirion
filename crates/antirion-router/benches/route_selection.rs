//! Choosing the first position of a route among 1,000 deployments of one model, for every
//! selection strategy that needs no external call (a pool's metrics are read beforehand).

use std::hint::black_box;

use antirion_router::pool::Reading;
use antirion_router::{
    AttemptPlan, CostOracle, Eligibility, Exclusion, Facts, Leaf, RoutingState, SelectCx, Slot, compile,
};
use criterion::{Criterion, criterion_group, criterion_main};
use serde_json::{Map, Value, json};

const DEPLOYMENTS: usize = 1000;

struct NoFacts;

impl Facts for NoFacts {
    fn header(&self, _: &str) -> Option<String> {
        None
    }
    fn body(&self) -> Option<&Value> {
        None
    }
    fn metadata(&self) -> Option<&Value> {
        None
    }
    fn request(&self, _: &str) -> Option<Value> {
        None
    }
    fn identity(&self, _: &str) -> Option<Value> {
        None
    }
}

/// Prices spread over the deployments, so the cheapest is not the first.
struct Prices;

impl CostOracle for Prices {
    fn price(&self, deployment: &str) -> Option<f64> {
        let n: f64 = deployment.trim_start_matches('d').parse().ok()?;
        Some(1.0 + (n * 7.0) % 13.0)
    }
}

struct AllEligible;

impl Eligibility for AllEligible {
    fn check(&mut self, _: Leaf<'_>) -> Result<(), Exclusion> {
        Ok(())
    }
}

fn names() -> Vec<String> {
    (0..DEPLOYMENTS).map(|i| format!("d{i:04}")).collect()
}

fn document() -> Value {
    let mut deployments = Map::new();
    for d in names() {
        deployments.insert(d, json!({"provider": "p", "model": "m"}));
    }
    let weighted: Vec<Value> = names()
        .into_iter()
        .enumerate()
        .map(|(i, d)| json!({"deployment": d, "weight": 1 + i % 5}))
        .collect();
    let plain: Vec<Value> = names().into_iter().map(Value::String).collect();
    let routes = json!({
        "smooth_wrr": {"tree": {"type": "weighted", "algorithm": "smooth_wrr", "children": weighted}},
        "random": {"tree": {"type": "weighted", "algorithm": "random", "children": weighted}},
        "sticky": {"tree": {"type": "weighted", "sticky_by": "session", "children": weighted}},
        "least_latency": {"tree": {"type": "least_latency", "children": plain}},
        "least_busy": {"tree": {"type": "least_busy", "children": plain}},
        "least_cost": {"tree": {"type": "least_cost", "children": plain}},
        "fallback": {"tree": {"type": "fallback", "children": plain}},
        "pool": {"tree": {"type": "pool", "children": plain}},
    });
    json!({
        "providers": {"p": {"type": "openai", "base_url": "http://127.0.0.1:1/v1"}},
        "deployments": deployments,
        "routes": routes,
    })
}

fn route_selection(c: &mut Criterion) {
    let Ok(snapshot) = compile(&document()) else {
        return;
    };
    let state = RoutingState::default();
    if let Some(pool) = snapshot.route("pool").and_then(|r| r.tree.node(0)) {
        let now = std::time::Instant::now();
        for (i, d) in names().iter().enumerate() {
            let reading = Reading {
                waiting: (i % 7) as f64,
                running: 1.0,
                kv: Some((i % 10) as f64 / 10.0),
                base_model: None,
                lora: None,
                served: None,
            };
            state
                .pools()
                .scraped(&pool.state_key, d, Some(reading), now, 1000);
        }
    }
    let sticky = |_: &str| Some("session-42".to_owned());
    let cx = SelectCx {
        state: &state,
        facts: &NoFacts,
        cost: &Prices,
        slot: Slot::E2e,
        default_estimate: [500.0, 50.0, 500.0],
        hash_seed: 1,
        sticky: &sticky,
        captures: None,
        now: antirion_core::UtcMicros(0),
        dry: false,
    };
    let mut group = c.benchmark_group("route_selection");
    for name in [
        "smooth_wrr",
        "random",
        "sticky",
        "least_latency",
        "least_busy",
        "least_cost",
        "fallback",
        "pool",
    ] {
        let Some(route) = snapshot.route(name) else {
            continue;
        };
        let mut seed = 0u64;
        group.bench_function(name, |b| {
            b.iter(|| {
                seed = seed.wrapping_add(1);
                let mut plan = AttemptPlan::new(&route.tree, seed);
                black_box(plan.next(&cx, &mut AllEligible))
            });
        });
    }
    group.finish();
}

criterion_group!(benches, route_selection);
criterion_main!(benches);
