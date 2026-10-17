//! The guardrails a request runs: attachments accumulated from the broadest policy level to the
//! narrowest, narrower `exclude`s removing broader ones, then sorted by level, `order` and name.

use std::sync::Arc;

use serde_json::Value;

use crate::def::{Def, Guardrails};

/// The effective guardrails of one request, in execution order.
#[derive(Debug, Default, Clone)]
pub struct Attached {
    pub input: Vec<Arc<Def>>,
    pub output: Vec<Arc<Def>>,
}

impl Attached {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.input.is_empty() && self.output.is_empty()
    }
}

fn names<'a>(block: &'a Value, list: &str) -> impl Iterator<Item = &'a str> {
    block
        .get(list)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}

fn stage(g: &Guardrails, levels: &[Option<&Value>], list: &str) -> Vec<Arc<Def>> {
    let mut entries: Vec<(usize, &str)> = Vec::new();
    for (level, block) in levels.iter().enumerate() {
        let Some(block) = block else { continue };
        for x in names(block, "exclude") {
            entries.retain(|(l, n)| *n != x || *l == level);
        }
        for n in names(block, list) {
            if !entries.iter().any(|(_, e)| *e == n) {
                entries.push((level, n));
            }
        }
    }
    let mut defs: Vec<(usize, Arc<Def>)> = entries
        .into_iter()
        .filter_map(|(l, n)| g.get(n).map(|d| (l, Arc::clone(d))))
        .collect();
    defs.sort_by(|(la, a), (lb, b)| la.cmp(lb).then(a.order.cmp(&b.order)).then(a.name.cmp(&b.name)));
    defs.into_iter().map(|(_, d)| d).collect()
}

/// The guardrails attached by `levels`: the `guardrails` member of each applicable `policies`
/// block, broadest level first.
#[must_use]
pub fn resolve(g: &Guardrails, levels: &[Option<&Value>]) -> Attached {
    Attached {
        input: stage(g, levels, "input"),
        output: stage(g, levels, "output"),
    }
}

/// The guardrails `levels` attach to MCP calls: `mcp_input` as the input stage and `mcp_output`
/// as the output stage.
#[must_use]
pub fn resolve_mcp(g: &Guardrails, levels: &[Option<&Value>]) -> Attached {
    Attached {
        input: stage(g, levels, "mcp_input"),
        output: stage(g, levels, "mcp_output"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::compile::compile;

    #[test]
    fn broadest_position_order_and_exclude() {
        let g = compile(&json!({"guardrails": {"definitions": {
            "twice": {"type": "regex", "regex": {"deny": ["z"]}},
            "other": {"type": "regex", "regex": {"deny": ["y"]}},
            "late": {"type": "regex", "order": 100, "regex": {"deny": ["x"]}},
            "early": {"type": "regex", "order": -5, "regex": {"deny": ["w"]}}
        }}}))
        .unwrap();
        let gateway = json!({"input": ["twice", "late"]});
        let route = json!({"input": ["other", "twice", "early"]});
        let a = resolve(&g, &[Some(&gateway), None, Some(&route)]);
        let names: Vec<&str> = a.input.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["twice", "late", "early", "other"]);
        let team = json!({"exclude": ["twice"]});
        let a = resolve(&g, &[Some(&gateway), Some(&team)]);
        let names: Vec<&str> = a.input.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["late"]);
    }
}
