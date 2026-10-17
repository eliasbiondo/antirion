//! Conditional grants (`policies.mcp.when`): rules over the caller's identity attributes whose
//! grants apply only when every condition holds. A level with rules grants what its matching rules
//! grant together, and nothing when none matches; a condition on a missing attribute is false.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::caller::Caller;
use crate::compile::McpGrant;
use crate::glob;
use crate::select::ItemKind;

/// How a condition compares an attribute.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    Equals(String),
    In(Vec<String>),
    Glob(String),
    Present,
}

/// One condition: an attribute and an operator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Cond {
    pub(crate) attribute: String,
    pub(crate) op: Op,
}

/// One rule: its conditions and what it grants.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Rule {
    pub(crate) conds: Vec<Cond>,
    pub(crate) servers: Option<Vec<String>>,
    pub(crate) tools: BTreeMap<String, Vec<String>>,
    pub(crate) prompts: BTreeMap<String, Vec<String>>,
    pub(crate) resources: BTreeMap<String, Vec<String>>,
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn item_lists(v: Option<&Value>) -> BTreeMap<String, Vec<String>> {
    v.and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(k, list)| (k.clone(), strings(Some(list))))
        .collect()
}

fn cond(v: &Value) -> Option<Cond> {
    let attribute = v.get("attribute")?.as_str()?.to_owned();
    let value = v.get("value");
    let op = match v.get("operator")?.as_str()? {
        "equals" => Op::Equals(value?.as_str()?.to_owned()),
        "in" => Op::In(strings(value)),
        "glob" => Op::Glob(value?.as_str()?.to_owned()),
        "present" => Op::Present,
        _ => return None,
    };
    Some(Cond { attribute, op })
}

/// The rules of a validated `when` list.
pub(crate) fn rules(v: &Value) -> Vec<Rule> {
    v.as_array()
        .into_iter()
        .flatten()
        .map(|r| Rule {
            conds: r
                .get("match")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(cond)
                .collect(),
            servers: r.get("servers").map(|s| strings(Some(s))),
            tools: item_lists(r.get("tools")),
            prompts: item_lists(r.get("prompts")),
            resources: item_lists(r.get("resources")),
        })
        .collect()
}

impl Cond {
    fn holds(&self, caller: &Caller) -> bool {
        let values = caller.values(&self.attribute);
        if values.is_empty() {
            return false;
        }
        match &self.op {
            Op::Present => true,
            Op::Equals(want) => values.iter().any(|v| v == want),
            Op::In(set) => values.iter().any(|v| set.contains(v)),
            Op::Glob(pattern) => values.iter().any(|v| glob::matches(pattern, v)),
        }
    }
}

impl Rule {
    /// Whether every condition holds for `caller` (a rule without conditions never matches).
    pub(crate) fn matches(&self, caller: &Caller) -> bool {
        !self.conds.is_empty() && self.conds.iter().all(|c| c.holds(caller))
    }

    /// Whether the rule grants `server`; a rule without `servers` grants every server.
    pub(crate) fn grants_server(&self, server: &str) -> bool {
        self.servers
            .as_ref()
            .is_none_or(|l| l.iter().any(|s| s == "*" || s == server))
    }

    /// Whether the rule lets the caller see `item` of `server` (its list for the server, if any).
    pub(crate) fn grants_item(&self, server: &str, kind: ItemKind, item: &str) -> bool {
        let lists = match kind {
            ItemKind::Tool => &self.tools,
            ItemKind::Prompt => &self.prompts,
            ItemKind::Resource => &self.resources,
        };
        self.grants_server(server) && lists.get(server).is_none_or(|globs| glob::any(globs, item))
    }
}

/// The rules of `grants` that match `caller`, one list per level that has rules.
pub(crate) fn matching(grants: &[McpGrant], caller: &Caller) -> Vec<Vec<Rule>> {
    grants
        .iter()
        .filter_map(|g| g.when.as_ref())
        .map(|rules| rules.iter().filter(|r| r.matches(caller)).cloned().collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn caller(claims: Value) -> Caller {
        Caller {
            project: "p".into(),
            tags: vec!["blue".into()],
            jwt: claims.as_object().cloned(),
            ..Caller::default()
        }
    }

    #[test]
    fn matching_rules_grant_servers_and_items() {
        let r = rules(&json!([
            {"match": [{"attribute": "jwt.mcp_backends", "operator": "in", "value": ["a"]}], "servers": ["a"]},
            {"match": [{"attribute": "jwt.mcp_backends", "operator": "equals", "value": "b"}], "servers": ["b"],
             "tools": {"b": ["get_*"]}},
            {"match": [{"attribute": "tags", "operator": "glob", "value": "bl*"},
                       {"attribute": "project", "operator": "equals", "value": "q"}], "servers": ["c"]}
        ]));
        let c = caller(json!({"mcp_backends": ["a", "b"]}));
        let hits: Vec<&Rule> = r.iter().filter(|x| x.matches(&c)).collect();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().any(|x| x.grants_server("a")) && !hits.iter().any(|x| x.grants_server("c")));
        assert!(hits.iter().any(|x| x.grants_item("b", ItemKind::Tool, "get_x")));
        assert!(!hits.iter().any(|x| x.grants_item("b", ItemKind::Tool, "delete")));
        assert!(
            !r.iter().any(|x| x.matches(&caller(json!({})))),
            "a missing attribute matches nothing"
        );
    }

    #[test]
    fn present_needs_a_value() {
        let r = rules(&json!([{"match": [{"attribute": "end_user", "operator": "present"}]}]));
        let anon = caller(json!({}));
        assert!(!r[0].matches(&anon));
        let named = Caller {
            end_user: Some("u".into()),
            ..anon
        };
        assert!(r[0].matches(&named) && r[0].grants_server("anything"));
    }
}
