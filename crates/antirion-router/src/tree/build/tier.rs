//! `tier` nodes: the enabled Models carrying a tier become the node's members, in name order,
//! each inlined as its route tree under a member node that stops serving at the Model's end of life.

use std::sync::Arc;

use antirion_config::{DetailCode, DocPath};
use antirion_core::UtcMicros;
use serde_json::Value;

use super::{Builder, error};
use crate::tree::{Algorithm, NodeKind, TierSpec};

/// Whether a Model's own tree is a tier node: its inline target, or the root of its named route.
fn targets_a_tier(doc: &Value, m: &Value) -> bool {
    let is_tier = |t: Option<&Value>| t.and_then(|t| t.get("type")).and_then(Value::as_str) == Some("tier");
    if is_tier(m.get("target")) {
        return true;
    }
    m.get("route")
        .and_then(Value::as_str)
        .and_then(|r| doc.pointer("/routes")?.get(r)?.get("tree"))
        .is_some_and(|t| is_tier(Some(t)))
}

impl Builder<'_, '_> {
    pub(super) fn tier(
        &mut self,
        index: usize,
        obj: &serde_json::Map<String, Value>,
        at: &DocPath,
        path: &str,
        depth: usize,
    ) -> usize {
        let tier = obj
            .get("tier")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if !crate::tier_known(self.cx.doc, &tier) {
            error(
                self.cx.errors,
                &at.key("tier"),
                DetailCode::UnresolvedReference,
                "the tier is neither built in nor declared in routing.tiers",
            );
        }
        let include_preview = obj
            .get("include_preview")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let (strategy, selects) = match obj.get("strategy").and_then(Value::as_str) {
            Some("least_latency") => (
                "least_latency",
                NodeKind::LeastLatency {
                    decay: self.cx.default_decay,
                },
            ),
            Some("weighted") => (
                "weighted",
                NodeKind::Weighted {
                    algorithm: Algorithm::SmoothWrr,
                    sticky: None,
                    experiment: None,
                    canary: None,
                },
            ),
            Some("fallback") => ("fallback", NodeKind::Fallback),
            _ => ("least_cost", NodeKind::LeastCost),
        };
        let doc = self.cx.doc;
        let mut members: Vec<(&String, &Value)> = doc
            .get("models")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(_, m)| {
                let state = m.pointer("/lifecycle/state").and_then(Value::as_str);
                m.get("tier").and_then(Value::as_str) == Some(tier.as_str())
                    && m.get("enabled").and_then(Value::as_bool) != Some(false)
                    && (include_preview || state != Some("preview"))
                    && !targets_a_tier(doc, m)
            })
            .collect();
        members.sort_unstable_by(|a, b| a.0.cmp(b.0));
        let mut children = Vec::new();
        for (name, m) in members {
            if self.stack.contains(name) {
                continue;
            }
            let tree_name = m.get("route").and_then(Value::as_str).unwrap_or(name);
            let Some(tree) = self.cx.trees.get(tree_name).map(|(t, _)| *t) else {
                continue;
            };
            let member = self.nodes.len();
            let member_path = format!("{path}/{name}");
            self.nodes.push(crate::tree::Node {
                kind: NodeKind::Member {
                    model: name.clone(),
                    eol_at: m
                        .pointer("/lifecycle/eol_at")
                        .and_then(Value::as_str)
                        .and_then(|t| UtcMicros::parse_rfc3339(t).ok()),
                },
                parent: Some(index),
                children: Vec::new(),
                weight: 1,
                segment: Some(name.clone()),
                path: member_path.clone(),
                params: None,
                policies: None,
                mirror: None,
                state_key: String::new(),
                tags: Vec::new(),
                body_edit: None,
            });
            // A member's tree reports its own errors where it is declared.
            let reported = self.cx.errors.len();
            self.stack.push(name.clone());
            let root_id = tree.get("id").and_then(Value::as_str).map(str::to_owned);
            let child = self.node(tree, at, Some(member), root_id, &member_path, depth + 1, false);
            self.stack.pop();
            self.cx.errors.truncate(reported);
            if let Some(n) = self.nodes.get_mut(member) {
                n.children = vec![child];
            }
            self.finish(member);
            children.push(member);
        }
        if let Some(n) = self.nodes.get_mut(index) {
            n.kind = NodeKind::Tier(Arc::new(TierSpec {
                tier,
                strategy,
                selects: Box::new(selects),
            }));
            n.children = children;
        }
        self.finish(index);
        index
    }
}
