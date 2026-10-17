//! Conditional nodes: ordered rules, each a predicate and a target, and an optional default.

use antirion_config::{DetailCode, DocPath};
use serde_json::Value;

use super::{Builder, error};
use crate::predicate::Predicate;
use crate::tree::{NodeKind, Rule};

impl Builder<'_, '_> {
    pub(super) fn conditional(
        &mut self,
        index: usize,
        obj: &serde_json::Map<String, Value>,
        at: &DocPath,
        path: &str,
        depth: usize,
    ) -> usize {
        let rules = obj
            .get("rules")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if rules.is_empty() {
            error(
                self.cx.errors,
                &at.key("rules"),
                DetailCode::OutOfRange,
                "a conditional node needs rules",
            );
        }
        let mut compiled = Vec::new();
        let mut children = Vec::new();
        let count = rules.len();
        for (i, rule) in rules.iter().enumerate() {
            let rat = at.key("rules").index(i);
            let predicate = match rule.get("when") {
                Some(w) => match Predicate::compile(w) {
                    Ok(p) => {
                        for sel in p.selectors() {
                            if let crate::predicate::Selector::Guard { name, .. } = sel
                                && self
                                    .cx
                                    .doc
                                    .pointer("/guardrails/definitions")
                                    .and_then(|d| d.get(name))
                                    .is_none()
                                && !self.cx.doc.is_null()
                            {
                                error(
                                    self.cx.errors,
                                    &rat.key("when"),
                                    DetailCode::UnresolvedReference,
                                    "the condition reads a guardrail that is not defined",
                                );
                            }
                        }
                        Some(p)
                    }
                    Err(e) => {
                        let mut wat = rat.key("when");
                        for part in e.at.split('.').filter(|p| !p.is_empty()) {
                            wat = wat.key(part);
                        }
                        error(self.cx.errors, &wat, DetailCode::InvalidFormat, &e.message);
                        None
                    }
                },
                None => {
                    if i + 1 != count {
                        error(
                            self.cx.errors,
                            &rat,
                            DetailCode::Conflict,
                            "a rule without `when` must be the last rule",
                        );
                    }
                    None
                }
            };
            compiled.push(Rule {
                when: predicate.or_else(|| rule.get("when").map(|_| Predicate::never())),
                name: rule.get("name").and_then(Value::as_str).map(str::to_owned),
                enabled: rule.get("enabled").and_then(Value::as_bool).unwrap_or(true),
            });
            let target = rule.get("target").unwrap_or(&Value::Null);
            let seg = target
                .get("id")
                .and_then(Value::as_str)
                .map_or_else(|| format!("r{i}"), str::to_owned);
            children.push(self.node(
                target,
                &rat.key("target"),
                Some(index),
                Some(seg),
                path,
                depth + 1,
                false,
            ));
        }
        let has_default = obj.get("default").is_some();
        if let Some(d) = obj.get("default") {
            let seg = d
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("default")
                .to_owned();
            children.push(self.node(
                d,
                &at.key("default"),
                Some(index),
                Some(seg),
                path,
                depth + 1,
                false,
            ));
        }
        if let Some(n) = self.nodes.get_mut(index) {
            n.kind = NodeKind::Conditional {
                rules: compiled,
                has_default,
            };
            n.children = children;
        }
        self.finish(index);
        index
    }
}
