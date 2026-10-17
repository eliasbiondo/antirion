use std::collections::{HashMap, HashSet};

use antirion_config::DocPath;
use serde_json::{Value, json};

use super::*;
use crate::tree::{Ctx, compile_route};

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

struct NoCost;

impl CostOracle for NoCost {
    fn price(&self, _: &str) -> Option<f64> {
        None
    }
}

struct Down(Vec<&'static str>);

impl Eligibility for Down {
    fn check(&mut self, leaf: Leaf<'_>) -> Result<(), Exclusion> {
        if self.0.contains(&leaf.deployment) {
            Err(Exclusion::Health {
                recover_at: None,
                cooling_only: false,
            })
        } else {
            Ok(())
        }
    }
}

fn route(tree: Value) -> CompiledRoute {
    let deployments: HashSet<String> = ["a", "b", "c", "d"].iter().map(|s| (*s).to_owned()).collect();
    let providers = HashSet::new();
    let trees = HashMap::new();
    let mut errors = Vec::new();
    let mut cx = Ctx {
        trees: &trees,
        deployments: &deployments,
        providers: &providers,
        models: &HashSet::new(),
        stars: None,
        default_decay: std::time::Duration::from_secs(10),
        doc: &Value::Null,
        errors: &mut errors,
    };
    let r = compile_route("r", &tree, &DocPath::root(), &mut cx);
    assert!(errors.is_empty());
    r
}

fn walk(r: &CompiledRoute, state: &RoutingState, down: Vec<&'static str>) -> Vec<(String, bool)> {
    let sticky = |_: &str| None;
    let cx = SelectCx {
        state,
        facts: &NoFacts,
        cost: &NoCost,
        slot: Slot::E2e,
        default_estimate: [500.0, 50.0, 500.0],
        hash_seed: 1,
        sticky: &sticky,
        captures: None,
        now: antirion_core::UtcMicros(0),
        dry: false,
    };
    let mut plan = AttemptPlan::new(r, 3);
    let mut elig = Down(down);
    let mut out = Vec::new();
    while let Some(s) = plan.next(&cx, &mut elig) {
        out.push((s.deployment, s.new_tier));
    }
    out
}

#[test]
fn depth_first_with_tiers() {
    let r = route(
        json!({"type": "fallback", "children": ["a", {"type": "weighted", "algorithm": "smooth_wrr", "children": ["b", "c"]}, "d"]}),
    );
    let state = RoutingState::default();
    let plan = walk(&r, &state, vec![]);
    assert_eq!(
        plan,
        vec![
            ("a".to_owned(), false),
            ("b".to_owned(), true),
            ("c".to_owned(), false),
            ("d".to_owned(), true)
        ]
    );
    let second = walk(&r, &state, vec![]);
    assert_eq!(
        second.get(1).map(|s| s.0.as_str()),
        Some("c"),
        "smooth WRR advanced"
    );
}

#[test]
fn ineligible_leaves_are_skipped_and_explained() {
    let r = route(json!({"type": "weighted", "children": ["a", "b"]}));
    let state = RoutingState::default();
    assert_eq!(walk(&r, &state, vec!["a"]), vec![("b".to_owned(), false)]);
    let sticky = |_: &str| None;
    let cx = SelectCx {
        state: &state,
        facts: &NoFacts,
        cost: &NoCost,
        slot: Slot::E2e,
        default_estimate: [500.0, 50.0, 500.0],
        hash_seed: 1,
        sticky: &sticky,
        captures: None,
        now: antirion_core::UtcMicros(0),
        dry: false,
    };
    let mut plan = AttemptPlan::new(&r, 1);
    assert!(plan.next(&cx, &mut Down(vec!["a", "b"])).is_none());
    assert!(plan.exhaustion().health);
}

#[test]
fn skip_tier_moves_to_the_next_fallback_child() {
    let r =
        route(json!({"type": "fallback", "children": [{"type": "weighted", "children": ["a", "b"]}, "c"]}));
    let state = RoutingState::default();
    let sticky = |_: &str| None;
    let cx = SelectCx {
        state: &state,
        facts: &NoFacts,
        cost: &NoCost,
        slot: Slot::E2e,
        default_estimate: [500.0, 50.0, 500.0],
        hash_seed: 1,
        sticky: &sticky,
        captures: None,
        now: antirion_core::UtcMicros(0),
        dry: false,
    };
    let mut plan = AttemptPlan::new(&r, 1);
    let mut elig = Down(vec![]);
    assert_eq!(
        plan.next(&cx, &mut elig).map(|s| s.deployment).as_deref(),
        Some("a")
    );
    plan.skip_tier();
    let next = plan.next(&cx, &mut elig).unwrap();
    assert_eq!((next.deployment.as_str(), next.new_tier), ("c", true));
}

fn walk_with(
    r: &CompiledRoute,
    state: &RoutingState,
    seed: u64,
    sticky: Option<&str>,
    down: Vec<&'static str>,
) -> Vec<String> {
    let value = sticky.map(str::to_owned);
    let sticky = move |_: &str| value.clone();
    let cx = SelectCx {
        state,
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
    let mut plan = AttemptPlan::new(r, seed);
    let mut elig = Down(down);
    std::iter::from_fn(|| plan.next(&cx, &mut elig).map(|s| s.deployment)).collect()
}

struct Prices;

impl CostOracle for Prices {
    fn price(&self, d: &str) -> Option<f64> {
        match d {
            "a" => Some(3.0),
            "b" => Some(1.0),
            "c" => Some(2.0),
            _ => None,
        }
    }
}

#[test]
fn every_algorithm_yields_each_eligible_leaf_once() {
    let kinds = [
        json!({"type": "weighted", "algorithm": "random", "children": ["a", "b", "c", "d"]}),
        json!({"type": "weighted", "algorithm": "smooth_wrr", "children": ["a", "b", "c", "d"]}),
        json!({"type": "weighted", "sticky_by": "session", "children": ["a", "b", "c", "d"]}),
        json!({"type": "least_latency", "children": ["a", "b", "c", "d"]}),
        json!({"type": "least_busy", "children": ["a", "b", "c", "d"]}),
        json!({"type": "least_cost", "children": ["a", "b", "c", "d"]}),
        json!({"type": "fallback", "children": ["a", "b", "c", "d"]}),
    ];
    for tree in kinds {
        let r = route(tree.clone());
        let state = RoutingState::default();
        for seed in 0..20 {
            let mut got = walk_with(&r, &state, seed, Some("s"), vec!["c"]);
            got.sort();
            assert_eq!(got, ["a", "b", "d"], "{tree}");
        }
    }
}

#[test]
fn least_cost_is_exact_and_skips_ineligible_children() {
    let r = route(json!({"type": "least_cost", "children": ["a", "b", "c", "d"]}));
    let state = RoutingState::default();
    assert_eq!(walk_with(&r, &state, 1, None, vec![]), ["b", "c", "a", "d"]);
    assert_eq!(walk_with(&r, &state, 1, None, vec!["b"]), ["c", "a", "d"]);
}

#[test]
fn random_first_positions_follow_the_weights_of_eligible_children() {
    let r = route(json!({"type": "weighted", "algorithm": "random", "children": [
        {"deployment": "a", "weight": 6}, {"deployment": "b", "weight": 3},
        {"deployment": "c", "weight": 1}, {"deployment": "d", "weight": 50}]}));
    let state = RoutingState::default();
    let n = 6000;
    let mut a = 0;
    for seed in 0..n {
        if walk_with(&r, &state, seed, None, vec!["d"])
            .first()
            .map(String::as_str)
            == Some("a")
        {
            a += 1;
        }
    }
    let share = f64::from(a) / f64::from(u32::try_from(n).unwrap());
    assert!((share - 0.6).abs() < 0.03, "{share}");
}

#[test]
fn sticky_values_keep_their_child_and_only_the_ejected_ones_move() {
    let r = route(json!({"type": "weighted", "sticky_by": "session", "children": ["a", "b", "c"]}));
    let state = RoutingState::default();
    let mut moved_from_c = 0;
    for i in 0..300 {
        let v = format!("v{i}");
        let before = walk_with(&r, &state, i, Some(&v), vec![]);
        assert_eq!(
            before,
            walk_with(&r, &state, i + 1, Some(&v), vec![]),
            "deterministic"
        );
        let after = walk_with(&r, &state, i, Some(&v), vec!["c"]);
        if before.first().map(String::as_str) == Some("c") {
            moved_from_c += 1;
        } else {
            assert_eq!(before.first(), after.first());
        }
    }
    assert!(moved_from_c > 50, "{moved_from_c}");
}

#[test]
fn least_latency_prefers_the_cheaper_of_two_and_orders_the_rest_by_cost() {
    let r = route(json!({"type": "least_latency", "children": ["a", "b", "c"]}));
    let state = RoutingState::default();
    let now = std::time::Instant::now();
    let tau = std::time::Duration::from_secs(10);
    state.observe("a", Slot::E2e, 10.0, tau, now);
    state.observe("b", Slot::E2e, 20.0, tau, now);
    state.observe("c", Slot::E2e, 1000.0, tau, now);
    for seed in 0..50 {
        let plan = walk_with(&r, &state, seed, None, vec![]);
        assert_ne!(plan.first().map(String::as_str), Some("c"));
        assert_eq!(plan.last().map(String::as_str), Some("c"));
    }
}

#[test]
fn cold_least_latency_children_start_their_estimates_together() {
    let r = route(json!({"type": "least_latency", "children": ["a", "b", "c"]}));
    let state = RoutingState::default();
    let mut firsts = std::collections::HashSet::new();
    for seed in 0..60 {
        let plan = walk_with(&r, &state, seed, None, vec![]);
        if let Some(f) = plan.first() {
            let now = std::time::Instant::now();
            state.observe(f, Slot::E2e, 5.0, std::time::Duration::from_secs(10), now);
            firsts.insert(f.clone());
        }
    }
    assert_eq!(
        firsts.len(),
        3,
        "every cold child gets a first position: {firsts:?}"
    );
}

fn walk_steps(r: &CompiledRoute, now: i64, down: Vec<&'static str>) -> Vec<Step> {
    let sticky = |_: &str| None;
    let state = RoutingState::default();
    let cx = SelectCx {
        state: &state,
        facts: &NoFacts,
        cost: &NoCost,
        slot: Slot::E2e,
        default_estimate: [500.0, 50.0, 500.0],
        hash_seed: 1,
        sticky: &sticky,
        captures: None,
        now: antirion_core::UtcMicros(now),
        dry: false,
    };
    let mut plan = AttemptPlan::new(r, 3);
    let mut elig = Down(down);
    let mut out = Vec::new();
    while let Some(s) = plan.next(&cx, &mut elig) {
        out.push(s);
    }
    out
}

#[test]
fn disabled_rules_are_skipped_as_if_absent() {
    let absent = json!({"field": "request.nothing", "exists": false});
    let r = route(json!({"type": "conditional", "rules": [
        {"name": "first", "enabled": false, "when": absent, "target": "a"},
        {"when": absent, "target": "b"}
    ]}));
    let steps = walk_steps(&r, 0, vec![]);
    assert_eq!(steps.first().map(|s| s.deployment.as_str()), Some("b"));
    let hits = r.rules_to(steps.first().map_or(0, |s| s.node));
    assert_eq!(hits.first().map(|h| (h.index, h.name.clone())), Some((1, None)));
}

#[test]
fn a_block_ends_the_plan_only_as_its_first_position() {
    let r = route(json!({"type": "conditional", "rules": [
        {"name": "deny", "when": {"field": "request.nothing", "exists": false},
         "target": {"type": "block", "status": 451, "category": "custom"}}
    ]}));
    let steps = walk_steps(&r, 0, vec![]);
    let block = steps.first().and_then(|s| s.block.clone()).expect("a block step");
    assert_eq!((block.status, block.category.as_str()), (451, "custom"));
    assert_eq!(r.rules_to(steps[0].node)[0].name.as_deref(), Some("deny"));
    let behind = route(json!({"type": "fallback", "children": ["a", {"type": "block"}, "b"]}));
    let steps = walk_steps(&behind, 0, vec!["a"]);
    let names: Vec<&str> = steps.iter().map(|s| s.deployment.as_str()).collect();
    assert_eq!(names, ["b"], "a block is never a fallback candidate");
}

#[test]
fn tier_members_follow_the_document() {
    let doc = json!({
        "deployments": {"a": {}, "b": {}, "c": {}},
        "models": {
            "fast": {"target": {"type": "tier", "tier": "fast", "strategy": "fallback"}},
            "x": {"tier": "fast", "target": "a", "lifecycle": {"eol_at": "1970-01-01T00:00:10Z"}},
            "y": {"tier": "fast", "target": "b"},
            "p": {"tier": "fast", "target": "c", "lifecycle": {"state": "preview"}},
            "off": {"tier": "fast", "target": "c", "enabled": false}
        }
    });
    let snap = crate::compile(&doc).expect("compiles");
    let r = &snap.model_routes["fast"].tree;
    let before: Vec<String> = walk_steps(r, 0, vec![])
        .into_iter()
        .map(|s| s.deployment)
        .collect();
    assert_eq!(
        before,
        ["a", "b"],
        "members in name order, preview and disabled left out"
    );
    let after: Vec<String> = walk_steps(r, 11_000_000, vec![])
        .into_iter()
        .map(|s| s.deployment)
        .collect();
    assert_eq!(after, ["b"], "a member past its end of life is skipped");
    let unknown =
        json!({"deployments": {"a": {}}, "models": {"m": {"target": {"type": "tier", "tier": "huge"}}}});
    let errors = crate::compile(&unknown).expect_err("unknown tier");
    assert!(errors.iter().any(|e| e.path == "models.m.target.tier"));
}

#[test]
fn sample_points_are_stable_and_uniform() {
    use crate::predicate::sample_point;
    assert_eq!(sample_point(7, "req-1"), sample_point(7, "req-1"));
    assert_ne!(sample_point(7, "req-1"), sample_point(8, "req-1"));
    let n = 20_000;
    let below = (0..n).filter(|i| sample_point(1, &format!("r{i}")) < 0.1).count();
    assert!((1700..=2300).contains(&below), "{below}");
}
