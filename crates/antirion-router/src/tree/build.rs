//! Compiling one route tree into its arena, with the config-time checks of every node.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use antirion_config::scalar::parse_duration;
use antirion_config::{ConfigError, DetailCode, DocPath};
use serde_json::Value;

use super::{
    Algorithm, Block, Canary, CompiledRoute, ErrorBudget, Experiment, MAX_DEPTH, Mirror, Node, NodeKind,
};
use crate::body_edit::{BadPointer, BodyEdit};
use crate::params::Params;
use crate::pool::PoolSpec;

mod branch;
mod content;
mod tier;

/// What tree compilation needs to know about the rest of the document.
pub(crate) struct Ctx<'a> {
    /// Route trees by name: named routes and Models' anonymous routes.
    pub(crate) trees: &'a HashMap<String, (&'a Value, DocPath)>,
    pub(crate) deployments: &'a HashSet<String>,
    pub(crate) providers: &'a HashSet<String>,
    /// Model names, for the Models classifiers and scorers call.
    pub(crate) models: &'a HashSet<String>,
    /// The number of `*` of the wildcard Model the tree belongs to, when it is one's inline target.
    pub(crate) stars: Option<usize>,
    pub(crate) default_decay: Duration,
    /// The whole document, for the Models a `tier` node selects among.
    pub(crate) doc: &'a Value,
    pub(crate) errors: &'a mut Vec<ConfigError>,
}

fn error(errors: &mut Vec<ConfigError>, at: &DocPath, code: DetailCode, message: &str) {
    errors.push(ConfigError::new(at, code, message));
}

fn valid_id(id: &str) -> bool {
    let b = id.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b.first()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

/// Placeholders a template uses: `{model}` and `{1}`…`{9}`; the highest index, 0 for none.
fn placeholders(template: &str) -> (bool, usize) {
    let mut any = false;
    let mut max = 0;
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let Some(len) = rest.get(start..).and_then(|s| s.find('}')) else {
            break;
        };
        let inner = rest.get(start + 1..start + len).unwrap_or_default();
        if inner == "model" {
            any = true;
        } else if let Ok(n) = inner.parse::<usize>()
            && (1..=9).contains(&n)
        {
            any = true;
            max = max.max(n);
        }
        rest = rest.get(start + len + 1..).unwrap_or_default();
    }
    (any, max)
}

struct Builder<'c, 'a> {
    cx: &'c mut Ctx<'a>,
    nodes: Vec<Node>,
    stack: Vec<String>,
}

impl Builder<'_, '_> {
    #[allow(clippy::too_many_arguments)]
    fn node(
        &mut self,
        v: &Value,
        at: &DocPath,
        parent: Option<usize>,
        segment: Option<String>,
        parent_path: &str,
        depth: usize,
        weight_allowed: bool,
    ) -> usize {
        let index = self.nodes.len();
        let path = match &segment {
            Some(s) => format!("{parent_path}/{s}"),
            None => parent_path.to_owned(),
        };
        self.nodes.push(Node {
            kind: NodeKind::Unsupported("invalid"),
            parent,
            children: Vec::new(),
            weight: 1,
            segment,
            path: path.clone(),
            params: None,
            policies: None,
            mirror: None,
            state_key: String::new(),
            tags: Vec::new(),
            body_edit: None,
        });
        if depth > MAX_DEPTH {
            error(
                self.cx.errors,
                at,
                DetailCode::OutOfRange,
                "the tree is deeper than 16 nodes",
            );
            return index;
        }
        let obj = match v {
            Value::String(d) => {
                self.leaf_deployment(index, d, at);
                return index;
            }
            Value::Object(o) => o,
            _ => {
                error(
                    self.cx.errors,
                    at,
                    DetailCode::TypeMismatch,
                    "a node is an object or a deployment name",
                );
                return index;
            }
        };
        if let Some(w) = obj.get("weight") {
            if !weight_allowed {
                error(
                    self.cx.errors,
                    &at.key("weight"),
                    DetailCode::Conflict,
                    "weight is only valid on children of weighted and least_busy nodes",
                );
            }
            if let Some(n) = self.nodes.get_mut(index) {
                n.weight = w.as_u64().and_then(|w| u32::try_from(w).ok()).unwrap_or(1);
            }
        }
        let mirror = obj
            .get("mirror")
            .and_then(|m| self.mirror(m, &at.key("mirror"), &path, depth));
        if let Some(n) = self.nodes.get_mut(index) {
            n.params = Params::from_value(obj.get("params"));
            n.policies = obj.get("policies").cloned();
            n.mirror = mirror;
        }
        let ty = obj.get("type").and_then(Value::as_str);
        let ty = ty.or_else(|| {
            if obj.contains_key("deployment") {
                Some("deployment")
            } else if obj.contains_key("route") {
                Some("route")
            } else {
                None
            }
        });
        if matches!(ty, Some("deployment" | "provider_model")) {
            self.leaf_extras(index, obj, at);
        }
        let kind = match ty {
            Some("deployment") => {
                let d = obj.get("deployment").and_then(Value::as_str).unwrap_or_default();
                self.leaf_deployment(index, d, &at.key("deployment"));
                return index;
            }
            Some("provider_model") => {
                let provider = obj.get("provider").and_then(Value::as_str).unwrap_or_default();
                let template = obj
                    .get("upstream_model")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !self.cx.providers.contains(provider) {
                    error(
                        self.cx.errors,
                        &at.key("provider"),
                        DetailCode::UnresolvedReference,
                        "provider does not exist",
                    );
                }
                let (any, max) = placeholders(template);
                match self.cx.stars {
                    None if any => error(
                        self.cx.errors,
                        &at.key("upstream_model"),
                        DetailCode::Conflict,
                        "placeholders are only valid under a wildcard Model",
                    ),
                    Some(stars) if max > stars => error(
                        self.cx.errors,
                        &at.key("upstream_model"),
                        DetailCode::OutOfRange,
                        "the placeholder index is beyond the pattern's `*` count",
                    ),
                    _ => {}
                }
                if template.is_empty() {
                    error(
                        self.cx.errors,
                        &at.key("upstream_model"),
                        DetailCode::OutOfRange,
                        "upstream_model is empty",
                    );
                }
                let leaf = format!("{provider}:{template}");
                if let Some(n) = self.nodes.get_mut(index) {
                    n.kind = NodeKind::ProviderModel {
                        provider: provider.to_owned(),
                        template: template.to_owned(),
                    };
                    n.state_key = format!("{path}|provider_model|{leaf}");
                }
                return index;
            }
            Some("route") => {
                let name = obj
                    .get("route")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if self.stack.contains(&name) {
                    error(
                        self.cx.errors,
                        &at.key("route"),
                        DetailCode::Conflict,
                        "route references form a cycle",
                    );
                    return index;
                }
                let Some(tree) = self.cx.trees.get(&name).map(|(t, _)| *t) else {
                    error(
                        self.cx.errors,
                        &at.key("route"),
                        DetailCode::UnresolvedReference,
                        "route does not exist",
                    );
                    return index;
                };
                let inner_path = format!("{path}/{name}");
                if let Some(n) = self.nodes.get_mut(index) {
                    n.path.clone_from(&inner_path);
                }
                self.stack.push(name.clone());
                let root_id = tree.get("id").and_then(Value::as_str).map(str::to_owned);
                let child = self.node(
                    tree,
                    &at.key("route"),
                    Some(index),
                    root_id,
                    &inner_path,
                    depth + 1,
                    false,
                );
                self.stack.pop();
                if let Some(n) = self.nodes.get_mut(index) {
                    n.children = vec![child];
                    n.kind = NodeKind::Route(name);
                }
                self.finish(index);
                return index;
            }
            Some("fallback") => NodeKind::Fallback,
            Some("weighted") => NodeKind::Weighted {
                algorithm: match obj.get("algorithm").and_then(Value::as_str) {
                    Some("random") => Algorithm::Random,
                    Some("thompson") => {
                        Algorithm::Thompson(Arc::new(crate::bandit::BanditSpec::from_node(obj)))
                    }
                    _ => Algorithm::SmoothWrr,
                },
                sticky: obj.get("sticky_by").and_then(Value::as_str).map(str::to_owned),
                experiment: obj.get("experiment").and_then(Value::as_str).map(|name| {
                    Arc::new(Experiment {
                        name: name.to_owned(),
                        expose: obj.get("expose").and_then(Value::as_bool).unwrap_or(false),
                    })
                }),
                canary: obj.get("canary").and_then(|c| self.canary(c, obj, at, &path)),
            },
            Some("least_latency") => NodeKind::LeastLatency {
                decay: obj
                    .get("decay")
                    .and_then(Value::as_str)
                    .and_then(parse_duration)
                    .unwrap_or(self.cx.default_decay),
            },
            Some("least_busy") => NodeKind::LeastBusy {
                cluster: obj.get("scope").and_then(Value::as_str) == Some("cluster"),
            },
            Some("least_cost") => NodeKind::LeastCost,
            Some("conditional") => return self.conditional(index, obj, at, &path, depth),
            Some("tier") => return self.tier(index, obj, at, &path, depth),
            Some("block") => {
                let block = Block {
                    status: obj
                        .get("status")
                        .and_then(Value::as_u64)
                        .and_then(|s| u16::try_from(s).ok())
                        .unwrap_or(400),
                    message: obj
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("Request blocked by routing policy.")
                        .to_owned(),
                    category: obj
                        .get("category")
                        .and_then(Value::as_str)
                        .unwrap_or("custom")
                        .to_owned(),
                };
                if let Some(n) = self.nodes.get_mut(index) {
                    n.kind = NodeKind::Block(Arc::new(block));
                }
                self.finish(index);
                return index;
            }
            Some("classifier") => return self.classifier(index, obj, at, &path, depth),
            Some("cost_quality") => return self.cost_quality(index, obj, at, &path, depth),
            Some("pool") => NodeKind::Pool(Arc::new(PoolSpec::from_value(obj, parse_duration))),
            _ => {
                error(
                    self.cx.errors,
                    &at.key("type"),
                    DetailCode::InvalidEnum,
                    "unknown node type",
                );
                return index;
            }
        };
        let weighted_children = matches!(kind, NodeKind::Weighted { .. } | NodeKind::LeastBusy { .. });
        if let NodeKind::Unsupported(_) = kind {
            if let Some(n) = self.nodes.get_mut(index) {
                n.kind = kind;
            }
            return index;
        }
        let children = obj
            .get("children")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if children.is_empty() {
            error(
                self.cx.errors,
                &at.key("children"),
                DetailCode::OutOfRange,
                "a composite node needs children",
            );
        }
        let mut ids = HashSet::new();
        let mut built = Vec::new();
        for (i, c) in children.iter().enumerate() {
            let cat = at.key("children").index(i);
            let id = c.get("id").and_then(Value::as_str);
            if let Some(id) = id {
                if !valid_id(id) {
                    error(
                        self.cx.errors,
                        &cat.key("id"),
                        DetailCode::InvalidFormat,
                        "not a valid node id",
                    );
                }
                if !ids.insert(id.to_owned()) {
                    error(
                        self.cx.errors,
                        &cat.key("id"),
                        DetailCode::Conflict,
                        "duplicate sibling id",
                    );
                }
            }
            let segment = id.map_or_else(|| i.to_string(), str::to_owned);
            built.push(self.node(
                c,
                &cat,
                Some(index),
                Some(segment),
                &path,
                depth + 1,
                weighted_children,
            ));
        }
        if let NodeKind::Weighted {
            experiment: Some(_), ..
        } = &kind
            && children
                .iter()
                .any(|c| c.get("id").and_then(Value::as_str).is_none())
        {
            error(
                self.cx.errors,
                &at.key("children"),
                DetailCode::Conflict,
                "every child of an experiment needs an id: the ids are the arms",
            );
        }
        if let NodeKind::Pool(_) = &kind {
            for (i, c) in built.iter().enumerate() {
                if self.nodes.get(*c).is_some_and(|n| !n.kind.is_leaf()) {
                    error(
                        self.cx.errors,
                        &at.key("children").index(i),
                        DetailCode::Conflict,
                        "the children of a pool are deployment or provider_model leaves",
                    );
                }
            }
        }
        if let NodeKind::Weighted { .. } = kind {
            let positive = built
                .iter()
                .any(|c| self.nodes.get(*c).is_some_and(|n| n.weight > 0));
            if !positive && !built.is_empty() {
                error(
                    self.cx.errors,
                    &at.key("children"),
                    DetailCode::OutOfRange,
                    "no child has a weight above 0",
                );
            }
        }
        if let Some(n) = self.nodes.get_mut(index) {
            n.kind = kind;
            n.children = built;
        }
        self.finish(index);
        index
    }

    /// A `weighted` node's `canary`: the candidate must be one of the node's children by id, and
    /// each step's weight a percentage.
    fn canary(
        &mut self,
        c: &Value,
        obj: &serde_json::Map<String, Value>,
        at: &DocPath,
        path: &str,
    ) -> Option<Arc<Canary>> {
        let at = at.key("canary");
        let id = c.get("child").and_then(Value::as_str).unwrap_or_default();
        let child = obj.get("children").and_then(Value::as_array).and_then(|kids| {
            kids.iter()
                .position(|k| k.get("id").and_then(Value::as_str) == Some(id))
        });
        let Some(child) = child else {
            error(
                self.cx.errors,
                &at.key("child"),
                DetailCode::UnresolvedReference,
                "the canary child is not the id of one of the node's children",
            );
            return None;
        };
        let mut steps = Vec::new();
        for (i, s) in c
            .get("steps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let weight = s.get("weight").and_then(Value::as_u64).unwrap_or(0);
            if weight > 100 {
                error(
                    self.cx.errors,
                    &at.key("steps").index(i).key("weight"),
                    DetailCode::OutOfRange,
                    "a canary step's weight is a percentage, at most 100",
                );
            }
            let after = s
                .get("after")
                .and_then(Value::as_str)
                .and_then(parse_duration)
                .unwrap_or_default();
            steps.push((after, u32::try_from(weight.min(100)).unwrap_or(100)));
        }
        steps.sort_by_key(|(after, _)| *after);
        let budget = c.get("error_budget").map(|b| ErrorBudget {
            max_error_rate: b.get("max_error_rate").and_then(Value::as_f64).unwrap_or(1.0),
            window: b
                .get("window")
                .and_then(Value::as_str)
                .and_then(parse_duration)
                .unwrap_or(Duration::from_secs(300)),
            rollback: b.get("action").and_then(Value::as_str) != Some("hold"),
        });
        Some(Arc::new(Canary {
            key: format!("{path}|{c}"),
            child,
            steps,
            budget,
        }))
    }

    /// A node's `mirror`, its target compiled as a tree whose root path is `<node path>/mirror`.
    fn mirror(&mut self, m: &Value, at: &DocPath, path: &str, depth: usize) -> Option<Arc<Mirror>> {
        let target = m.get("target")?;
        let root = format!("{path}/mirror");
        let mut b = Builder {
            cx: &mut *self.cx,
            nodes: Vec::new(),
            stack: self.stack.clone(),
        };
        b.node(target, &at.key("target"), None, None, &root, depth + 1, false);
        let nodes = b.nodes;
        Some(Arc::new(Mirror {
            target: CompiledRoute { name: root, nodes },
            fraction: m.get("fraction").and_then(Value::as_f64).unwrap_or(0.0),
            by_session: m.get("sample_by").and_then(Value::as_str) == Some("session"),
            max_in_flight: m
                .get("max_in_flight")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(64),
        }))
    }

    /// A leaf's tags and `upstream_body`.
    fn leaf_extras(&mut self, index: usize, obj: &serde_json::Map<String, Value>, at: &DocPath) {
        let tags: Vec<String> = obj
            .get("tags")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let edit = match BodyEdit::from_value(obj.get("upstream_body")) {
            Ok(e) => e.map(Arc::new),
            Err(bad) => {
                let at = at.key("upstream_body");
                let at = match bad {
                    BadPointer::Set(k) => at.key("set").key(&k),
                    BadPointer::Remove(i) => at.key("remove").index(i),
                };
                error(
                    self.cx.errors,
                    &at,
                    DetailCode::InvalidFormat,
                    "not a JSON Pointer",
                );
                None
            }
        };
        if let Some(n) = self.nodes.get_mut(index) {
            n.tags = tags;
            n.body_edit = edit;
        }
    }

    fn leaf_deployment(&mut self, index: usize, d: &str, at: &DocPath) {
        if !self.cx.deployments.contains(d) {
            error(
                self.cx.errors,
                at,
                DetailCode::UnresolvedReference,
                "deployment does not exist",
            );
        }
        if let Some(n) = self.nodes.get_mut(index) {
            n.state_key = format!("{}|deployment|{d}", n.path);
            n.kind = NodeKind::Deployment(d.to_owned());
        }
    }

    fn finish(&mut self, index: usize) {
        let Some(n) = self.nodes.get(index) else { return };
        let kids: Vec<String> = n
            .children
            .iter()
            .filter_map(|c| self.nodes.get(*c))
            .map(|c| format!("{}:{}", c.segment.as_deref().unwrap_or_default(), c.weight))
            .collect();
        let key = format!("{}|{}|{}", n.path, n.kind.type_name(), kids.join(","));
        if let Some(n) = self.nodes.get_mut(index) {
            n.state_key = key;
        }
    }
}

/// Compiles one route tree. Errors go to `cx.errors` with their document paths.
pub(crate) fn compile_route(name: &str, tree: &Value, at: &DocPath, cx: &mut Ctx<'_>) -> CompiledRoute {
    let root_id = tree.get("id").and_then(Value::as_str).map(str::to_owned);
    if let Some(id) = &root_id
        && !valid_id(id)
    {
        error(
            cx.errors,
            &at.key("id"),
            DetailCode::InvalidFormat,
            "not a valid node id",
        );
    }
    let mut b = Builder {
        cx,
        nodes: Vec::new(),
        stack: vec![name.to_owned()],
    };
    b.node(tree, at, None, root_id, name, 1, false);
    CompiledRoute {
        name: name.to_owned(),
        nodes: b.nodes,
    }
}
