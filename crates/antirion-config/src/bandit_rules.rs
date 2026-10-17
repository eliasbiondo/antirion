//! The configuration rules of bandit experiments (`algorithm: thompson`): an experiment node, a
//! feedback metric whose values are rewards in [0, 1] (a boolean metric, or a number metric with a
//! `min` and a `max`), feedback enabled, arms that are children of the node (at least two), and
//! the bandit members only with the algorithm.

use serde_json::Value;

use crate::tree_walk::each_node;
use crate::{ConfigError, DetailCode, DocPath};

const BANDIT_MEMBERS: [&str; 4] = ["metric", "min_samples_per_arm", "stop_after", "arms"];

fn reward_metric(doc: &Value, name: &str) -> Result<(), &'static str> {
    let Some(m) = doc
        .pointer("/observability/feedback/metrics")
        .and_then(|ms| ms.get(name))
    else {
        return Err("names no feedback metric");
    };
    match m.get("type").and_then(Value::as_str) {
        Some("boolean") => Ok(()),
        Some("number")
            if m.get("min").is_some_and(Value::is_number) && m.get("max").is_some_and(Value::is_number) =>
        {
            Ok(())
        }
        _ => Err("must name a boolean metric, or a number metric with min and max"),
    }
}

fn node(doc: &Value, n: &serde_json::Map<String, Value>, at: &DocPath, errors: &mut Vec<ConfigError>) {
    let thompson = n.get("algorithm").and_then(Value::as_str) == Some("thompson");
    if !thompson {
        for member in BANDIT_MEMBERS {
            if n.contains_key(member) {
                errors.push(ConfigError::new(
                    &at.key(member),
                    DetailCode::Conflict,
                    "applies only to algorithm thompson",
                ));
            }
        }
        return;
    }
    if !n.contains_key("experiment") {
        errors.push(ConfigError::new(
            &at.key("algorithm"),
            DetailCode::Conflict,
            "algorithm thompson needs an experiment",
        ));
    }
    match n.get("metric").and_then(Value::as_str) {
        None => errors.push(ConfigError::new(
            &at.key("metric"),
            DetailCode::Required,
            "algorithm thompson needs a feedback metric",
        )),
        Some(m) => {
            if let Err(why) = reward_metric(doc, m) {
                errors.push(ConfigError::new(
                    &at.key("metric"),
                    DetailCode::UnresolvedReference,
                    why,
                ));
            } else if doc
                .pointer("/observability/feedback/enabled")
                .and_then(Value::as_bool)
                != Some(true)
            {
                errors.push(ConfigError::new(
                    &at.key("metric"),
                    DetailCode::Conflict,
                    "a bandit learns from feedback, which is not enabled",
                ));
            }
        }
    }
    let ids: Vec<&str> = n
        .get("children")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| c.get("id").and_then(Value::as_str))
        .collect();
    match n.get("arms").and_then(Value::as_array) {
        Some(arms) => {
            for (i, a) in arms.iter().enumerate() {
                if !a.as_str().is_some_and(|a| ids.contains(&a)) {
                    errors.push(ConfigError::new(
                        &at.key("arms").index(i),
                        DetailCode::UnresolvedReference,
                        "names no child of the experiment",
                    ));
                }
            }
            if arms.len() < 2 {
                errors.push(ConfigError::new(
                    &at.key("arms"),
                    DetailCode::OutOfRange,
                    "a bandit needs two arms",
                ));
            }
        }
        None if ids.len() < 2 => errors.push(ConfigError::new(
            &at.key("children"),
            DetailCode::OutOfRange,
            "a bandit needs two arms",
        )),
        None => {}
    }
}

pub(crate) fn check(doc: &Value, errors: &mut Vec<ConfigError>) {
    each_node(doc, &mut |n, at| {
        if n.get("type").and_then(Value::as_str) == Some("weighted") {
            node(doc, n, at, errors);
        }
    });
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn paths(doc: &Value) -> Vec<String> {
        let mut errors = Vec::new();
        check(doc, &mut errors);
        errors.into_iter().map(|e| e.path).collect()
    }

    fn doc(node: Value, metrics: Value) -> Value {
        json!({"observability": {"feedback": {"enabled": true, "metrics": metrics}},
               "routes": {"r": {"tree": node}}})
    }

    fn arms() -> Value {
        json!([{"id": "a", "deployment": "a"}, {"id": "b", "deployment": "b"}])
    }

    #[test]
    fn a_valid_bandit_passes() {
        let d = doc(
            json!({"type": "weighted", "algorithm": "thompson", "experiment": "e", "metric": "good",
                   "min_samples_per_arm": 3, "stop_after": {"delta": 0.05}, "arms": ["a", "b"], "children": arms()}),
            json!({"good": {"type": "boolean"}}),
        );
        assert!(paths(&d).is_empty(), "{:?}", paths(&d));
    }

    #[test]
    fn invalid_bandits_are_named_by_path() {
        let node = |extra: Value| {
            let mut n = json!({"type": "weighted", "algorithm": "thompson", "experiment": "e", "metric": "good",
                               "children": arms()});
            n.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            n
        };
        let metrics =
            json!({"good": {"type": "boolean"}, "stars": {"type": "number"}, "note": {"type": "text"}});
        let at = |p: &str| format!("routes.r.tree.{p}");
        assert_eq!(
            paths(&doc(node(json!({"metric": "nope"})), metrics.clone())),
            vec![at("metric")]
        );
        assert_eq!(
            paths(&doc(node(json!({"metric": "stars"})), metrics.clone())),
            vec![at("metric")]
        );
        assert_eq!(
            paths(&doc(node(json!({"metric": "note"})), metrics.clone())),
            vec![at("metric")]
        );
        assert_eq!(
            paths(&doc(node(json!({"arms": ["a", "z"]})), metrics.clone())),
            vec![at("arms[1]")]
        );
        let mut no_exp = node(json!({}));
        no_exp.as_object_mut().unwrap().remove("experiment");
        assert_eq!(paths(&doc(no_exp, metrics.clone())), vec![at("algorithm")]);
        let plain = json!({"type": "weighted", "metric": "good", "children": arms()});
        assert_eq!(paths(&doc(plain, metrics.clone())), vec![at("metric")]);
        let mut off = doc(node(json!({})), metrics);
        off["observability"]["feedback"]["enabled"] = json!(false);
        assert_eq!(paths(&off), vec![at("metric")]);
    }
}
