use std::collections::{HashMap, HashSet};

use antirion_config::DocPath;
use serde_json::json;

use super::*;

fn build(tree: Value) -> (CompiledRoute, Vec<String>) {
    let deployments: HashSet<String> = ["a", "a2", "b"].iter().map(|s| (*s).to_owned()).collect();
    let providers = HashSet::new();
    let inner = json!({"type": "fallback", "children": ["b"]});
    let mut trees = HashMap::new();
    trees.insert("inner".to_owned(), (&inner, DocPath::root()));
    let mut errors = Vec::new();
    let mut cx = Ctx {
        trees: &trees,
        deployments: &deployments,
        providers: &providers,
        models: &HashSet::new(),
        stars: None,
        default_decay: Duration::from_secs(10),
        doc: &serde_json::Value::Null,
        errors: &mut errors,
    };
    let r = compile_route(
        "r",
        &tree,
        &DocPath::root().key("routes").key("r").key("tree"),
        &mut cx,
    );
    (r, errors.into_iter().map(|e| e.path).collect())
}

#[test]
fn paths_and_labels() {
    let (r, errors) = build(json!({"type": "weighted", "children": [
        {"type": "fallback", "children": ["a", "a2"]},
        {"type": "fallback", "id": "top", "children": [{"route": "inner"}]}]}));
    assert!(errors.is_empty(), "{errors:?}");
    let paths: Vec<String> = r
        .leaves()
        .iter()
        .filter_map(|l| r.node(*l))
        .map(|n| match &n.kind {
            NodeKind::Deployment(d) => format!("{}/{d}", n.path),
            _ => String::new(),
        })
        .collect();
    assert_eq!(paths, vec!["r/0/0/a", "r/0/1/a2", "r/top/0/inner/0/b"]);
    let leaves = r.leaves();
    assert_eq!(r.label(leaves[0], leaves[1]).as_deref(), Some("fallback@r/0"));
    assert_eq!(r.label(leaves[1], leaves[2]).as_deref(), Some("weighted@r"));
    assert!(r.lca_is_fallback(leaves[0], leaves[1]));
}

#[test]
fn invalid_trees_name_their_paths() {
    let mut deep = json!("a");
    for _ in 0..16 {
        deep = json!({"type": "fallback", "children": [deep]});
    }
    assert!(!build(deep).1.is_empty());
    let (_, e) = build(
        json!({"type": "fallback", "children": [{"deployment": "a", "id": "x"}, {"deployment": "b", "id": "x"}]}),
    );
    assert_eq!(e, vec!["routes.r.tree.children[1].id"]);
    let (_, e) = build(
        json!({"type": "conditional", "rules": [{"target": "a"}, {"when": {"field": "metadata.t", "eq": "x"}, "target": "b"}]}),
    );
    assert_eq!(e, vec!["routes.r.tree.rules[0]"]);
    let (_, e) = build(json!({"type": "fallback", "children": ["ghost", {"route": "nowhere"}]}));
    assert_eq!(e.len(), 2);
    let (_, e) = build(json!({"type": "weighted", "children": [{"deployment": "a", "weight": 0}]}));
    assert_eq!(e, vec!["routes.r.tree.children"]);
}

#[test]
fn mirrors_compile_their_target_apart_from_the_tree() {
    let (r, errors) = build(json!({"deployment": "a",
        "mirror": {"target": {"type": "fallback", "children": ["b", "missing"]}, "fraction": 0.25,
                   "sample_by": "session"}}));
    assert_eq!(errors, ["routes.r.tree.mirror.target.children[1]"]);
    assert_eq!(
        r.leaves().len(),
        1,
        "a mirror's leaves are not positions of the tree"
    );
    assert!(!r.has_deployment("b"));
    let m = r.node(0).and_then(|n| n.mirror.clone()).unwrap();
    assert_eq!(m.target.name, "r/mirror");
    assert_eq!(m.target.leaves().len(), 2);
    assert!((m.fraction - 0.25).abs() < f64::EPSILON);
    assert!(m.by_session);
    assert_eq!(m.max_in_flight, 64);
}
