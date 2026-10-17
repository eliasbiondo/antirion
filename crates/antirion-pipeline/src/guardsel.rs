//! Conditions on guardrail results: a `guard.<name>.<field>` selector runs the named input
//! guardrail when the evaluation first reaches it, once per request, reusing the verdict the input
//! stage already recorded for it. The conditions are evaluated ahead of the plan so each guardrail
//! a reached rule reads is run; the plan's own evaluation then reads the kept results.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use antirion_router::{NodeKind, RouteRt};
use antirion_runtime::guardtest::Sample;
use serde_json::{Value, json};

use crate::ctx::Ctx;
use crate::route::RequestFacts;

/// The results of the guardrails a request's conditions read, and the one last asked for that
/// has none yet.
#[derive(Debug, Default)]
pub(crate) struct GuardResults {
    results: Mutex<HashMap<String, Value>>,
    wanted: Mutex<Option<String>>,
}

impl GuardResults {
    /// A field of guardrail `name`'s result: `passed`, `verdict` or `category`. Without a result
    /// yet, the guardrail is noted as wanted and the field is absent.
    pub(crate) fn field(&self, name: &str, field: &str) -> Option<Value> {
        let results = self.results.lock().ok()?;
        let Some(r) = results.get(name) else {
            if let Ok(mut w) = self.wanted.lock() {
                w.get_or_insert_with(|| name.to_owned());
            }
            return None;
        };
        match field {
            "passed" => Some(json!(r.get("verdict").and_then(Value::as_str) == Some("pass"))),
            "verdict" | "category" => r.get(field).cloned().filter(|v| !v.is_null()),
            _ => None,
        }
    }

    fn take_wanted(&self) -> Option<String> {
        self.wanted.lock().ok()?.take()
    }

    fn keep(&self, name: String, result: Value) {
        if let Ok(mut r) = self.results.lock() {
            r.insert(name, result);
        }
    }
}

/// The verdict the input stage recorded for guardrail `name`, if it ran.
fn recorded(cx: &Ctx, name: &str) -> Option<Value> {
    cx.log
        .read(|r| {
            r.guardrails
                .iter()
                .find(|g| g["name"] == json!(name) && g["stage"] == json!("input"))
                .map(|g| json!({"verdict": g["verdict"], "category": g["category"]}))
        })
        .flatten()
}

/// Runs, once each, the guardrails the reached rules of `route` read.
pub(crate) async fn run(cx: &Ctx, route: &RouteRt, facts: &RequestFacts<'_>, results: &GuardResults) {
    let body = facts.body.cloned().unwrap_or_else(|| json!({}));
    let tree = &route.tree;
    let mut stack = vec![0usize];
    while let Some(i) = stack.pop() {
        let Some(n) = tree.node(i) else { continue };
        match &n.kind {
            NodeKind::Conditional { rules, has_default } => {
                let mut chosen = None;
                for (k, rule) in rules.iter().enumerate().filter(|(_, r)| r.enabled) {
                    let holds = loop {
                        let holds = rule.when.as_ref().is_none_or(|p| p.eval(facts));
                        let Some(name) = results.take_wanted() else {
                            break holds;
                        };
                        let result = match recorded(cx, &name) {
                            Some(r) => r,
                            None => crate::guardtest::test(
                                Arc::clone(&cx.rt),
                                Arc::clone(&cx.snap),
                                name.clone(),
                                false,
                                Sample::Body(body.clone()),
                            )
                            .await
                            .unwrap_or_else(|_| json!({"verdict": "error", "category": null})),
                        };
                        results.keep(name, result);
                    };
                    if holds {
                        chosen = Some(k);
                        break;
                    }
                }
                let child = chosen.or_else(|| has_default.then(|| n.children.len().saturating_sub(1)));
                stack.extend(child.and_then(|c| n.children.get(c).copied()));
            }
            NodeKind::Classifier(_) | NodeKind::CostQuality(_) => {}
            _ => stack.extend(n.children.iter().copied()),
        }
    }
}
