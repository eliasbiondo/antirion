//! Routing: public Models, aliases and wildcards resolved to routes; route trees compiled once per
//! generation and evaluated per request into an attempt plan; the selection algorithms and their
//! state; conditions and classifiers; node parameters; session affinity bindings.

#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unreachable
)]

pub mod affinity;
pub mod bandit;
pub mod body_edit;
pub mod canary;
pub mod canon;
pub mod classifier;
pub mod content;
pub mod lru;
pub mod model;
pub mod params;
pub mod pickers;
pub mod plan;
pub mod pool;
pub mod predicate;
pub mod prefix_key;
mod select;
pub mod shadow;
pub mod simulate;
pub mod state;
pub mod tags;
pub mod tree;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use antirion_config::scalar::{parse_duration, parse_size};
use antirion_config::{ConfigError, DetailCode, DocPath};
use serde_json::Value;

pub use affinity::{AffinityStore, Binding, binding_key};
pub use model::{BUILTIN_TIERS, Caller, Lifecycle, LifecycleState, ModelAccess, ModelRt, Resolved};
pub use params::Params;
pub use plan::{AttemptPlan, Captures, CostOracle, Eligibility, Exclusion, Exhaustion, Leaf, SelectCx, Step};
pub use predicate::Facts;
pub use state::{InFlight, MirrorSlot, RoutingState, Slot};
pub use tree::{CompiledRoute, Mirror, Node, NodeKind};

use crate::model::Wildcard;

/// A route's session-affinity settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AffinitySettings {
    pub session: bool,
    pub sources: Vec<String>,
    /// `project`, `key` or `organization`.
    pub scope: String,
    pub ttl: Duration,
    /// `mode: strict`: a bound session is served only by its position.
    pub strict: bool,
    /// `on_all_unhealthy`: `Some(true)` for `ignore_health`, `Some(false)` for `reject`, `None`
    /// when the route follows the gateway's `resilience.health.all_unhealthy`.
    pub ignore_when_all_unhealthy: Option<bool>,
    /// Without a session value, the prompt prefix stands for one (`prefer` only).
    pub prompt_prefix: bool,
}

/// A compiled route with its route-level settings.
#[derive(Clone, Debug)]
pub struct RouteRt {
    pub tree: CompiledRoute,
    /// The route's `policies` block (none for a Model's anonymous route).
    pub policies: Option<Value>,
    pub affinity: AffinitySettings,
    /// With tag filtering, the tag of the leaves requests without a matching tag go to.
    pub tag_filter: Option<String>,
    /// Some decision of the tree needs a call, or replays an earlier one: decisions are taken
    /// before the plan is walked.
    pub decides_ahead: bool,
    /// Some weighted node has a canary: attempts are counted against its error budget.
    pub has_canary: bool,
    /// Some rule reads a guardrail's result: those guardrails run before the plan is walked.
    pub reads_guards: bool,
}

impl RouteRt {
    fn new(
        tree: CompiledRoute,
        policies: Option<Value>,
        affinity: AffinitySettings,
        tag_filter: Option<String>,
    ) -> RouteRt {
        let decides_ahead = tree.nodes.iter().any(|n| {
            n.kind.calls()
                || matches!(&n.kind, NodeKind::Classifier(c) if c.replay)
                || matches!(&n.kind, NodeKind::CostQuality(_))
        });
        let has_canary = tree
            .nodes
            .iter()
            .any(|n| matches!(&n.kind, NodeKind::Weighted { canary: Some(_), .. }));
        let reads_guards = tree.nodes.iter().any(|n| match &n.kind {
            NodeKind::Conditional { rules, .. } => rules.iter().any(|r| {
                r.when.as_ref().is_some_and(|p| {
                    p.selectors()
                        .iter()
                        .any(|s| matches!(s, predicate::Selector::Guard { .. }))
                })
            }),
            _ => false,
        });
        RouteRt {
            tree,
            policies,
            affinity,
            tag_filter,
            decides_ahead,
            has_canary,
            reads_guards,
        }
    }
}

impl RouteRt {
    /// The implicit route of a `provider/model` request: one `provider_model` leaf, no policies.
    #[must_use]
    pub fn direct(name: &str, provider: &str, upstream_model: &str) -> RouteRt {
        RouteRt {
            tree: CompiledRoute {
                name: name.to_owned(),
                nodes: vec![Node {
                    kind: NodeKind::ProviderModel {
                        provider: provider.to_owned(),
                        template: upstream_model.to_owned(),
                    },
                    parent: None,
                    children: Vec::new(),
                    weight: 1,
                    segment: None,
                    path: name.to_owned(),
                    params: None,
                    policies: None,
                    mirror: None,
                    state_key: format!("{name}|provider_model"),
                    tags: Vec::new(),
                    body_edit: None,
                }],
            },
            policies: None,
            affinity: AffinitySettings {
                session: false,
                sources: Vec::new(),
                scope: "project".to_owned(),
                ttl: Duration::from_secs(3600),
                strict: false,
                ignore_when_all_unhealthy: None,
                prompt_prefix: false,
            },
            tag_filter: None,
            decides_ahead: false,
            has_canary: false,
            reads_guards: false,
        }
    }
}

/// The `routing` settings.
#[derive(Clone, Debug)]
pub struct RoutingSettings {
    pub default_models: HashMap<String, String>,
    pub hash_seed: u64,
    pub cost_output_estimate: u64,
    pub default_estimate: [f64; 3],
    pub affinity_max_entries: usize,
    pub metadata_max_size: usize,
    pub expose_route_path: bool,
    /// Every generation request is answered with its routing decision, nothing sent upstream.
    pub dry_run_all: bool,
}

impl Default for RoutingSettings {
    fn default() -> RoutingSettings {
        RoutingSettings {
            default_models: HashMap::new(),
            hash_seed: 0,
            cost_output_estimate: 512,
            default_estimate: [500.0, 50.0, 500.0],
            affinity_max_entries: 100_000,
            metadata_max_size: 8 << 10,
            expose_route_path: true,
            dry_run_all: false,
        }
    }
}

/// Every Model and route of one generation.
#[derive(Clone, Debug, Default)]
pub struct RoutingSnapshot {
    pub models: BTreeMap<String, Arc<ModelRt>>,
    aliases: HashMap<String, (String, Option<Params>)>,
    wildcards: Vec<Wildcard>,
    /// Named routes.
    pub routes: HashMap<String, Arc<RouteRt>>,
    /// Models' anonymous routes, by Model name.
    pub model_routes: HashMap<String, Arc<RouteRt>>,
    pub settings: RoutingSettings,
}

impl RoutingSnapshot {
    /// Resolves a requested name: an exact public Model, then an alias, then the most specific
    /// matching wildcard.
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<Resolved> {
        let usable = |m: &&Arc<ModelRt>| m.enabled && m.public;
        if let Some(m) = self.models.get(name).filter(usable).filter(|m| !m.wildcard) {
            return Some(Resolved {
                model: Arc::clone(m),
                alias_params: None,
                captures: None,
                via: "exact",
            });
        }
        if let Some((model, params)) = self.aliases.get(name)
            && let Some(m) = self.models.get(model).filter(usable)
        {
            return Some(Resolved {
                model: Arc::clone(m),
                alias_params: params.clone(),
                captures: None,
                via: "alias",
            });
        }
        self.wildcards.iter().find_map(|w| {
            let m = self.models.get(&w.model).filter(usable)?;
            Some(Resolved {
                model: Arc::clone(m),
                alias_params: None,
                captures: Some(w.captures(name)?),
                via: "wildcard",
            })
        })
    }

    /// The route that serves a Model.
    #[must_use]
    pub fn route_of(&self, model: &ModelRt) -> Option<&Arc<RouteRt>> {
        if model.named_route {
            self.routes.get(&model.route)
        } else {
            self.model_routes.get(&model.name)
        }
    }

    /// A route by name: a named route, else a Model's anonymous route.
    #[must_use]
    pub fn route(&self, name: &str) -> Option<&Arc<RouteRt>> {
        self.routes.get(name).or_else(|| self.model_routes.get(name))
    }

    /// The names a client may list: every public, enabled, non-wildcard Model and its aliases.
    #[must_use]
    pub fn listed(&self) -> Vec<(&str, &Arc<ModelRt>)> {
        let mut out = Vec::new();
        for m in self
            .models
            .values()
            .filter(|m| m.enabled && m.public && !m.wildcard)
        {
            out.push((m.name.as_str(), m));
            for a in &m.aliases {
                out.push((a.as_str(), m));
            }
        }
        out
    }
}

impl RoutingSnapshot {
    /// The key of every canary of every route.
    #[must_use]
    pub fn canary_keys(&self) -> Vec<&str> {
        self.routes
            .values()
            .chain(self.model_routes.values())
            .filter(|r| r.has_canary)
            .flat_map(|r| r.tree.nodes.iter())
            .filter_map(|n| match &n.kind {
                NodeKind::Weighted { canary: Some(c), .. } => Some(c.key.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// Every deployment a Model's route can reach.
#[must_use]
pub fn deployments_of(route: &CompiledRoute) -> Vec<&str> {
    route
        .nodes
        .iter()
        .filter_map(|n| match &n.kind {
            NodeKind::Deployment(d) => Some(d.as_str()),
            _ => None,
        })
        .collect()
}

fn settings(doc: &Value) -> RoutingSettings {
    let r = doc.get("routing").unwrap_or(&Value::Null);
    let d = RoutingSettings::default();
    let est = |slot: &str, default: f64| {
        r.pointer(&format!("/latency/default_estimate/{slot}"))
            .and_then(Value::as_str)
            .and_then(parse_duration)
            .map_or(default, |d| d.as_secs_f64() * 1000.0)
    };
    RoutingSettings {
        default_models: r
            .get("default_models")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| v.as_str().map(|m| (k.clone(), m.to_owned())))
            .collect(),
        hash_seed: r.get("hash_seed").and_then(Value::as_u64).unwrap_or(0x5eed_a17e),
        cost_output_estimate: r
            .get("cost_output_estimate")
            .and_then(Value::as_u64)
            .unwrap_or(512),
        default_estimate: [est("ttft", 500.0), est("per_token", 50.0), est("e2e", 500.0)],
        affinity_max_entries: r
            .pointer("/affinity/max_entries")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(d.affinity_max_entries),
        metadata_max_size: r
            .get("metadata_max_size")
            .and_then(Value::as_str)
            .and_then(parse_size)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(d.metadata_max_size),
        expose_route_path: r
            .get("expose_route_path")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        dry_run_all: r.get("dry_run_all").and_then(Value::as_bool).unwrap_or(false),
    }
}

const DEFAULT_SOURCES: [&str; 4] = [
    "header:x-antirion-session-id",
    "body:prompt_cache_key",
    "header:x-claude-code-session-id",
    "body:metadata.user_id",
];

fn affinity(doc: &Value, spec: Option<&Value>) -> AffinitySettings {
    let gw = doc.pointer("/routing/affinity").unwrap_or(&Value::Null);
    let a = spec.and_then(|s| s.get("affinity")).unwrap_or(&Value::Null);
    let sources = |v: &Value| -> Option<Vec<String>> {
        v.get("sources")
            .and_then(Value::as_array)
            .map(|l| l.iter().filter_map(Value::as_str).map(str::to_owned).collect())
    };
    let ttl = |v: &Value| v.get("ttl").and_then(Value::as_str).and_then(parse_duration);
    AffinitySettings {
        session: a.get("session").and_then(Value::as_bool).unwrap_or(true),
        sources: sources(a)
            .or_else(|| sources(gw))
            .unwrap_or_else(|| DEFAULT_SOURCES.iter().map(|s| (*s).to_owned()).collect()),
        scope: a
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or("project")
            .to_owned(),
        ttl: ttl(a).or_else(|| ttl(gw)).unwrap_or(Duration::from_secs(3600)),
        strict: a.get("mode").and_then(Value::as_str) == Some("strict"),
        ignore_when_all_unhealthy: spec
            .and_then(|s| s.get("on_all_unhealthy"))
            .and_then(Value::as_str)
            .map(|v| v == "ignore_health"),
        prompt_prefix: a.get("prompt_prefix").and_then(Value::as_bool).unwrap_or(false),
    }
}

fn tag_filter(spec: &Value) -> Option<String> {
    let t = spec.get("tag_filter")?;
    (t.get("enabled").and_then(Value::as_bool) == Some(true)).then(|| {
        t.get("default_tag")
            .and_then(Value::as_str)
            .unwrap_or("default")
            .to_owned()
    })
}

fn names(doc: &Value, key: &str) -> HashSet<String> {
    doc.get(key)
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// Whether `tier` is built in or declared in `routing.tiers`.
#[must_use]
pub fn tier_known(doc: &Value, tier: &str) -> bool {
    BUILTIN_TIERS.contains(&tier) || doc.pointer("/routing/tiers").and_then(|t| t.get(tier)).is_some()
}

/// The references of a Model's catalog fields: its tier, its successor and its aliases' owners.
fn catalog_refs(doc: &Value, at: &DocPath, m: &Value, errors: &mut Vec<ConfigError>) {
    if let Some(t) = m.get("tier").and_then(Value::as_str)
        && !tier_known(doc, t)
    {
        errors.push(ConfigError::new(
            &at.key("tier"),
            DetailCode::UnresolvedReference,
            "the tier is neither built in nor declared in routing.tiers",
        ));
    }
    if let Some(r) = m.pointer("/lifecycle/replacement").and_then(Value::as_str)
        && doc.pointer("/models").and_then(|ms| ms.get(r)).is_none()
    {
        errors.push(ConfigError::new(
            &at.key("lifecycle").key("replacement"),
            DetailCode::UnresolvedReference,
            "the replacement Model does not exist",
        ));
    }
    for (alias, spec) in m.get("aliases").and_then(Value::as_object).into_iter().flatten() {
        if let Some(owner) = spec.get("owner").and_then(Value::as_str)
            && owner != "default"
            && doc.pointer("/teams").and_then(|t| t.get(owner)).is_none()
        {
            errors.push(ConfigError::new(
                &at.key("aliases").key(alias).key("owner"),
                DetailCode::UnresolvedReference,
                "the owner is not a team",
            ));
        }
    }
}

/// Compiles Models and routes from a validated, secret-resolved document.
pub fn compile(doc: &Value) -> Result<RoutingSnapshot, Vec<ConfigError>> {
    let root = DocPath::root();
    let deployments = names(doc, "deployments");
    let providers = names(doc, "providers");
    let model_names = names(doc, "models");
    let empty = serde_json::Map::new();
    let route_specs = doc.get("routes").and_then(Value::as_object).unwrap_or(&empty);
    let model_specs = doc.get("models").and_then(Value::as_object).unwrap_or(&empty);
    let mut trees: HashMap<String, (&Value, DocPath)> = HashMap::new();
    for (name, m) in model_specs {
        if let Some(t) = m.get("target") {
            trees.insert(name.clone(), (t, root.key("models").key(name).key("target")));
        }
    }
    for (name, r) in route_specs {
        if let Some(t) = r.get("tree") {
            trees.insert(name.clone(), (t, root.key("routes").key(name).key("tree")));
        }
    }
    let mut errors = Vec::new();
    let mut out = RoutingSnapshot {
        settings: settings(doc),
        ..RoutingSnapshot::default()
    };
    let default_decay = doc
        .pointer("/routing/latency/decay")
        .and_then(Value::as_str)
        .and_then(parse_duration)
        .unwrap_or(Duration::from_secs(10));
    for (name, r) in route_specs {
        let Some(t) = r.get("tree") else { continue };
        let at = root.key("routes").key(name).key("tree");
        let mut cx = tree::Ctx {
            trees: &trees,
            deployments: &deployments,
            providers: &providers,
            models: &model_names,
            stars: None,
            default_decay,
            doc,
            errors: &mut errors,
        };
        let compiled = tree::compile_route(name, t, &at, &mut cx);
        out.routes.insert(
            name.clone(),
            Arc::new(RouteRt::new(
                compiled,
                r.get("policies").cloned(),
                affinity(doc, Some(r)),
                tag_filter(r),
            )),
        );
    }
    let mut alias_owner: HashMap<String, String> = HashMap::new();
    // Name order: which Model keeps a contested alias does not depend on the document's member order.
    let mut models: Vec<(&String, &Value)> = model_specs.iter().collect();
    models.sort_unstable_by(|a, b| a.0.cmp(b.0));
    for (name, m) in models {
        let at = root.key("models").key(name);
        let wildcard = name.contains('*');
        let route_name = m.get("route").and_then(Value::as_str);
        if let Some(t) = m.get("target") {
            let mut cx = tree::Ctx {
                trees: &trees,
                deployments: &deployments,
                providers: &providers,
                models: &model_names,
                stars: wildcard.then(|| name.matches('*').count()),
                default_decay,
                doc,
                errors: &mut errors,
            };
            let compiled = tree::compile_route(name, t, &at.key("target"), &mut cx);
            out.model_routes.insert(
                name.clone(),
                Arc::new(RouteRt::new(compiled, None, affinity(doc, None), None)),
            );
        }
        let mut aliases = Vec::new();
        for (alias, spec) in m.get("aliases").and_then(Value::as_object).into_iter().flatten() {
            let aat = at.key("aliases").key(alias);
            if alias.contains('*') {
                errors.push(ConfigError::new(
                    &aat,
                    DetailCode::InvalidFormat,
                    "an alias cannot contain `*`",
                ));
                continue;
            }
            if model_specs.contains_key(alias) {
                errors.push(ConfigError::new(
                    &aat,
                    DetailCode::Conflict,
                    "an alias cannot equal a Model name",
                ));
                continue;
            }
            if let Some(other) = alias_owner.insert(alias.clone(), name.clone()) {
                let _ = other;
                errors.push(ConfigError::new(
                    &aat,
                    DetailCode::Conflict,
                    "the alias is already used",
                ));
                continue;
            }
            out.aliases.insert(
                alias.clone(),
                (name.clone(), Params::from_value(spec.get("params"))),
            );
            aliases.push(alias.clone());
        }
        if wildcard && let Some(w) = Wildcard::new(name, name) {
            out.wildcards.push(w);
        }
        let rt = ModelRt {
            name: name.clone(),
            enabled: m.get("enabled").and_then(Value::as_bool).unwrap_or(true),
            public: m.get("visibility").and_then(Value::as_str) != Some("internal"),
            owned_by: m
                .get("owned_by")
                .and_then(Value::as_str)
                .unwrap_or("antirion")
                .to_owned(),
            route: route_name.unwrap_or(name).to_owned(),
            named_route: route_name.is_some(),
            aliases,
            wildcard,
            tier: m.get("tier").and_then(Value::as_str).map(str::to_owned),
            labels: m
                .get("labels")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            lifecycle: model::Lifecycle::from_value(m.get("lifecycle")),
            access: model::ModelAccess::from_value(m.get("access")),
        };
        catalog_refs(doc, &at, m, &mut errors);
        out.models.insert(name.clone(), Arc::new(rt));
    }
    out.wildcards.sort_by(Wildcard::specificity);
    if errors.is_empty() { Ok(out) } else { Err(errors) }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn resolution_order_and_visibility() {
        let doc = json!({
            "deployments": {"a": {}, "b": {}},
            "routes": {"r": {"tree": {"type": "fallback", "children": ["a", "b"]}}},
            "models": {
                "m": {"target": {"deployment": "a"}, "aliases": {"m-alias": {"params": {"set": {"x": 1}}}}},
                "via": {"route": "r"},
                "off": {"target": "a", "enabled": false},
                "hidden": {"target": "a", "visibility": "internal"},
                "*": {"target": "b"}
            }
        });
        let s = compile(&doc).unwrap();
        assert_eq!(s.resolve("m").map(|r| r.via), Some("exact"));
        let alias = s.resolve("m-alias").unwrap();
        assert_eq!((alias.model.name.as_str(), alias.via), ("m", "alias"));
        assert!(alias.alias_params.is_some());
        assert_eq!(s.resolve("hidden").map(|r| r.via), Some("wildcard"));
        assert_eq!(
            s.resolve("nope").map(|r| r.model.name.clone()).as_deref(),
            Some("*")
        );
        let via = s.resolve("via").unwrap();
        assert_eq!(s.route_of(&via.model).map(|r| r.tree.name.as_str()), Some("r"));
        assert_eq!(s.listed().len(), 3);
    }

    #[test]
    fn alias_collisions_are_rejected() {
        let doc = json!({
            "deployments": {"a": {}},
            "models": {"one": {"target": "a", "aliases": {"one-a": {}}}, "two": {"target": "a", "aliases": {"one": {}, "one-a": {}, "t*": {}}}}
        });
        let errors = compile(&doc).unwrap_err();
        let paths: Vec<String> = errors.iter().map(|e| e.path.clone()).collect();
        assert!(paths.contains(&"models.two.aliases.one".to_owned()));
        assert!(paths.contains(&"models.two.aliases.t*".to_owned()));
        assert_eq!(paths.len(), 3);
    }
}
