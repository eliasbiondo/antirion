//! Rules the schema cannot express: references between resources, route cycles, weighted nodes with
//! no positive weight, `locked` fields set at narrower levels, and cross-field security checks.

use std::collections::{BTreeMap, HashSet};

use serde_json::{Map, Value};

use crate::model::{self, DEFAULT_TENANT};
use crate::{ConfigError, DetailCode, DocPath};

/// A reference from one place in the document to a named resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    pub from: DocPath,
    /// The Admin API collection name of the target (`providers`, `guardrails`).
    pub collection: &'static str,
    pub name: String,
}

fn obj<'a>(doc: &'a Value, key: &str) -> impl Iterator<Item = (&'a String, &'a Value)> {
    doc.get(key)
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(Map::iter)
}

/// Every resource reference in `doc`.
#[must_use]
pub fn references(doc: &Value) -> Vec<Reference> {
    let mut out = Vec::new();
    let root = DocPath::root();
    let mut add = |from: DocPath, collection: &'static str, name: &str| {
        out.push(Reference {
            from,
            collection,
            name: name.to_owned(),
        });
    };
    for (name, d) in obj(doc, "deployments") {
        if let Some(p) = d.get("provider").and_then(Value::as_str) {
            add(root.key("deployments").key(name).key("provider"), "providers", p);
        }
    }
    for (name, m) in obj(doc, "models") {
        let at = root.key("models").key(name);
        if let Some(r) = m.get("route").and_then(Value::as_str) {
            add(at.key("route"), "routes", r);
        }
        if let Some(t) = m.get("target") {
            node_refs(t, &at.key("target"), &mut add);
        }
    }
    for (name, r) in obj(doc, "routes") {
        if let Some(t) = r.get("tree") {
            node_refs(t, &root.key("routes").key(name).key("tree"), &mut add);
        }
    }
    for (name, t) in obj(doc, "teams") {
        if let Some(o) = t.get("organization").and_then(Value::as_str) {
            add(
                root.key("teams").key(name).key("organization"),
                "organizations",
                o,
            );
        }
    }
    for (name, p) in obj(doc, "projects") {
        if let Some(t) = p.get("team").and_then(Value::as_str) {
            add(root.key("projects").key(name).key("team"), "teams", t);
        }
    }
    for (name, k) in obj(doc, "keys") {
        let at = root.key("keys").key(name);
        let project = k.get("project").and_then(Value::as_str).unwrap_or(DEFAULT_TENANT);
        add(at.key("project"), "projects", project);
        if let Some(owner) = k.get("owner").and_then(Value::as_str) {
            add(at.key("owner"), "users", owner);
        }
    }
    for (name, u) in obj(doc, "users") {
        let bindings = u
            .get("role_bindings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten();
        for (i, b) in bindings.enumerate() {
            let Some(scope) = b.get("scope").and_then(Value::as_str) else {
                continue;
            };
            let at = root
                .key("users")
                .key(name)
                .key("role_bindings")
                .index(i)
                .key("scope");
            let target = match scope.split_once('/') {
                Some(("organization", n)) => Some(("organizations", n)),
                Some(("team", n)) => Some(("teams", n)),
                Some(("project", n)) => Some(("projects", n)),
                _ => None,
            };
            if let Some((c, n)) = target {
                add(at, c, n);
            }
        }
    }
    for (path, block) in policy_blocks(doc) {
        policy_refs(block, &path, &mut add);
    }
    for level in policy_levels(doc) {
        let at = level.path.key("resilience").key("retry").key("class_fallbacks");
        let fallbacks = level
            .block
            .pointer("/resilience/retry/class_fallbacks")
            .and_then(Value::as_object);
        for (class, target) in fallbacks.into_iter().flatten() {
            if let Some(name) = target.as_str() {
                add(at.key(class), "models", name);
            }
        }
    }
    for (path, server) in crate::mcp_rules::references(doc) {
        add(path, "mcp/servers", &server);
    }
    out
}

fn node_refs(node: &Value, at: &DocPath, add: &mut impl FnMut(DocPath, &'static str, &str)) {
    match node {
        Value::String(d) => add(at.clone(), "deployments", d),
        Value::Object(m) => {
            let ty = m.get("type").and_then(Value::as_str);
            if let Some(d) = m.get("deployment").and_then(Value::as_str) {
                add(at.key("deployment"), "deployments", d);
            }
            if let Some(r) = m.get("route").and_then(Value::as_str)
                && (ty.is_none() || ty == Some("route"))
            {
                add(at.key("route"), "routes", r);
            }
            if ty == Some("provider_model")
                && let Some(p) = m.get("provider").and_then(Value::as_str)
            {
                add(at.key("provider"), "providers", p);
            }
            for list in ["children", "rules", "classes"] {
                for (i, item) in m
                    .get(list)
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    let item_at = at.key(list).index(i);
                    if list == "children" {
                        node_refs(item, &item_at, add);
                    } else if let Some(t) = item.get("target") {
                        node_refs(t, &item_at.key("target"), add);
                    }
                }
            }
            for single in ["default", "strong", "weak"] {
                if let Some(child) = m.get(single) {
                    node_refs(child, &at.key(single), add);
                }
            }
            if let Some(target) = m.get("mirror").and_then(|x| x.get("target")) {
                node_refs(target, &at.key("mirror").key("target"), add);
            }
        }
        _ => {}
    }
}

/// Every `policies` block of the document with its path, broadest level first.
#[must_use]
pub fn policy_blocks(doc: &Value) -> Vec<(DocPath, &Value)> {
    let root = DocPath::root();
    let mut out = Vec::new();
    if let Some(p) = doc.get("policies") {
        out.push((root.key("policies"), p));
    }
    for level in ["organizations", "teams", "projects", "keys", "routes", "models"] {
        for (name, r) in obj(doc, level) {
            if let Some(p) = r.get("policies") {
                out.push((root.key(level).key(name).key("policies"), p));
            }
        }
    }
    out
}

fn policy_refs(block: &Value, at: &DocPath, add: &mut impl FnMut(DocPath, &'static str, &str)) {
    if let Some(g) = block.get("guardrails") {
        for list in ["input", "output", "mcp_input", "mcp_output", "exclude"] {
            for (i, item) in g
                .get(list)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                if let Some(name) = item.as_str() {
                    add(at.key("guardrails").key(list).index(i), "guardrails", name);
                }
            }
        }
    }
    if let Some(p) = block.get("plugins") {
        for list in ["attach", "detach"] {
            for (i, item) in p
                .get(list)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                let item_at = at.key("plugins").key(list).index(i);
                match item {
                    Value::String(name) => add(item_at, "plugins", name),
                    Value::Object(o) => {
                        if let Some(name) = o.get("name").and_then(Value::as_str) {
                            add(item_at.key("name"), "plugins", name);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// True when the named resource exists in `doc` (the default tenancy resources always do).
#[must_use]
pub fn exists(doc: &Value, collection: &str, name: &str) -> bool {
    if matches!(collection, "organizations" | "teams" | "projects") && name == DEFAULT_TENANT {
        return true;
    }
    model::collection(collection)
        .and_then(|c| model::collection_map(doc, c))
        .is_some_and(|m| m.contains_key(name))
}

/// Every place in `doc` that references the named resource.
#[must_use]
pub fn referrers(doc: &Value, collection: &str, name: &str) -> Vec<DocPath> {
    references(doc)
        .into_iter()
        .filter(|r| r.collection == collection && r.name == name)
        .map(|r| r.from)
        .collect()
}

/// Runs every semantic rule and returns all violations.
#[must_use]
pub fn check(doc: &Value) -> Vec<ConfigError> {
    let mut errors = Vec::new();
    for r in references(doc) {
        let model_route = r.collection == "routes"
            && doc
                .get("models")
                .and_then(|m| m.get(&r.name))
                .is_some_and(|m| m.get("target").is_some());
        if !exists(doc, r.collection, &r.name) && !model_route {
            let what = r.collection.trim_end_matches('s').replace('/', " ");
            errors.push(ConfigError::new(
                &r.from,
                DetailCode::UnresolvedReference,
                format!("{what} {:?} does not exist", r.name),
            ));
        }
    }
    route_cycles(doc, &mut errors);
    weighted_nodes(doc, &mut errors);
    locks(doc, &mut errors);
    locked_detaches(doc, &mut errors);
    cors(doc, &mut errors);
    crate::access_rules::check(doc, &mut errors);
    crate::console_rules::check(doc, &mut errors);
    crate::resilience_rules::check(doc, &mut errors);
    crate::mcp_rules::check(doc, &mut errors);
    crate::obs_rules::check(doc, &mut errors);
    crate::metrics_rules::check(doc, &mut errors);
    crate::cache_rules::check(doc, &mut errors);
    crate::invalidation_rules::check(doc, &mut errors);
    crate::unsupported::check(doc, &mut errors);
    crate::bandit_rules::check(doc, &mut errors);
    crate::alert_rules::check(doc, &mut errors);
    crate::passthrough_rules::check(doc, &mut errors);
    crate::operations_rules::check(doc, &mut errors);
    crate::store_rules::check(doc, &mut errors);
    errors
}

fn route_cycles(doc: &Value, errors: &mut Vec<ConfigError>) {
    let mut edges: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for r in references(doc) {
        if r.collection != "routes" {
            continue;
        }
        if let [crate::Seg::Key(top), crate::Seg::Key(from), ..] = r.from.segments()
            && top == "routes"
        {
            edges.entry(from.clone()).or_default().push(r.name);
        }
    }
    for start in edges.keys() {
        let mut stack: Vec<&String> = edges.get(start).map(|v| v.iter().collect()).unwrap_or_default();
        let mut seen: HashSet<&String> = HashSet::new();
        while let Some(next) = stack.pop() {
            if next == start {
                errors.push(ConfigError::new(
                    &DocPath::root().key("routes").key(start).key("tree"),
                    DetailCode::Conflict,
                    "route references form a cycle",
                ));
                break;
            }
            if seen.insert(next) {
                stack.extend(edges.get(next).into_iter().flatten());
            }
        }
    }
}

fn weighted_nodes(doc: &Value, errors: &mut Vec<ConfigError>) {
    fn walk(node: &Value, at: &DocPath, errors: &mut Vec<ConfigError>) {
        let Value::Object(m) = node else { return };
        if let Some(children) = m.get("children").and_then(Value::as_array) {
            if m.get("type").and_then(Value::as_str) == Some("weighted") {
                let positive = children.iter().any(|c| match c {
                    Value::Object(o) => o.get("weight").and_then(Value::as_u64).unwrap_or(1) > 0,
                    _ => true,
                });
                if !positive && !children.is_empty() {
                    errors.push(ConfigError::new(
                        &at.key("children"),
                        DetailCode::OutOfRange,
                        "a weighted node needs at least one child with a weight above 0",
                    ));
                }
            }
            for (i, c) in children.iter().enumerate() {
                walk(c, &at.key("children").index(i), errors);
            }
        }
    }
    for (name, r) in obj(doc, "routes") {
        if let Some(t) = r.get("tree") {
            walk(t, &DocPath::root().key("routes").key(name).key("tree"), errors);
        }
    }
    for (name, m) in obj(doc, "models") {
        if let Some(t) = m.get("target") {
            walk(t, &DocPath::root().key("models").key(name).key("target"), errors);
        }
    }
}

/// The policy level of a block, with the names needed to place it in the hierarchy.
pub(crate) struct Level<'a> {
    pub(crate) path: DocPath,
    pub(crate) block: &'a Value,
    /// Every broader block this level inherits from, as paths.
    pub(crate) ancestors: Vec<DocPath>,
}

/// The `policies` blocks of every tenancy resource.
fn tenant_blocks(doc: &Value) -> Vec<DocPath> {
    let root = DocPath::root();
    ["organizations", "teams", "projects", "keys"]
        .iter()
        .flat_map(|level| {
            obj(doc, level)
                .filter(|(_, r)| r.get("policies").is_some())
                .map(|(name, _)| root.key(level).key(name).key("policies"))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The `policies` blocks of route-tree nodes, each narrower than its ancestors in the tree.
fn node_policies<'a>(node: &'a Value, at: &DocPath, ancestors: &[DocPath], out: &mut Vec<Level<'a>>) {
    let mut chain = ancestors.to_vec();
    if let Some(p) = node.get("policies") {
        out.push(Level {
            path: at.key("policies"),
            block: p,
            ancestors: chain.clone(),
        });
        chain.push(at.key("policies"));
    }
    for (i, c) in node
        .get("children")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        node_policies(c, &at.key("children").index(i), &chain, out);
    }
    for key in ["rules", "classes"] {
        for (i, c) in node
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            if let Some(t) = c.get("target") {
                node_policies(t, &at.key(key).index(i).key("target"), &chain, out);
            }
        }
    }
    if let Some(d) = node.get("default") {
        node_policies(d, &at.key("default"), &chain, out);
    }
}

/// Every `policies` block with the broader blocks it inherits from.
pub(crate) fn policy_levels(doc: &Value) -> Vec<Level<'_>> {
    let root = DocPath::root();
    let gateway = root.key("policies");
    let lookup = |level: &str, name: &str, field: &str| -> Option<String> {
        doc.get(level)?.get(name)?.get(field)?.as_str().map(str::to_owned)
    };
    let team_org =
        |team: &str| lookup("teams", team, "organization").unwrap_or_else(|| DEFAULT_TENANT.to_owned());
    let project_team = |p: &str| lookup("projects", p, "team").unwrap_or_else(|| DEFAULT_TENANT.to_owned());
    let mut levels: Vec<Level<'_>> = Vec::new();
    for (path, block) in policy_blocks(doc) {
        let segs = path.segments();
        let (kind, name) = match segs {
            [crate::Seg::Key(k), crate::Seg::Key(n), ..] if segs.len() == 3 => (k.as_str(), n.as_str()),
            _ => ("gateway", ""),
        };
        let mut ancestors = Vec::new();
        if kind != "gateway" {
            ancestors.push(gateway.clone());
        }
        let org_path = |o: &str| root.key("organizations").key(o).key("policies");
        let team_path = |t: &str| root.key("teams").key(t).key("policies");
        let project_path = |p: &str| root.key("projects").key(p).key("policies");
        match kind {
            "teams" => ancestors.push(org_path(&team_org(name))),
            "projects" => {
                let t = project_team(name);
                ancestors.push(org_path(&team_org(&t)));
                ancestors.push(team_path(&t));
            }
            "keys" => {
                let p = lookup("keys", name, "project").unwrap_or_else(|| DEFAULT_TENANT.to_owned());
                let t = project_team(&p);
                ancestors.push(org_path(&team_org(&t)));
                ancestors.push(team_path(&t));
                ancestors.push(project_path(&p));
            }
            "models" => {
                ancestors.extend(tenant_blocks(doc));
                if let Some(r) = lookup("models", name, "route") {
                    ancestors.push(root.key("routes").key(&r).key("policies"));
                }
            }
            "routes" => ancestors.extend(tenant_blocks(doc)),
            _ => {}
        }
        levels.push(Level {
            path,
            block,
            ancestors,
        });
    }
    for (level, tree_key) in [("routes", "tree"), ("models", "target")] {
        for (name, spec) in obj(doc, level) {
            let Some(tree) = spec.get(tree_key) else { continue };
            let mut ancestors = vec![gateway.clone()];
            ancestors.extend(tenant_blocks(doc));
            ancestors.push(root.key(level).key(name).key("policies"));
            if level == "models"
                && let Some(r) = spec.get("route").and_then(Value::as_str)
            {
                ancestors.push(root.key("routes").key(r).key("policies"));
            }
            node_policies(
                tree,
                &root.key(level).key(name).key(tree_key),
                &ancestors,
                &mut levels,
            );
        }
    }
    levels
}

fn locks(doc: &Value, errors: &mut Vec<ConfigError>) {
    let levels = policy_levels(doc);
    for narrow in &levels {
        for broad_path in &narrow.ancestors {
            let Some(broad) = broad_path.get(doc) else {
                continue;
            };
            let Some(members) = broad.as_object() else {
                continue;
            };
            for (member, settings) in members {
                let locked = settings
                    .get("locked")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten();
                for field in locked.filter_map(Value::as_str) {
                    let rel = DocPath::parse_dotted(field);
                    let Some(narrow_member) = narrow.block.get(member) else {
                        continue;
                    };
                    if member == "guardrails" && ATTACHMENT_LISTS.contains(&field) {
                        locked_excludes(settings, field, narrow_member, &narrow.path, broad_path, errors);
                        continue;
                    }
                    if rel.get(narrow_member).is_some() {
                        let mut at = narrow.path.key(member);
                        for seg in rel.segments() {
                            if let crate::Seg::Key(k) = seg {
                                at.push_key(k);
                            }
                        }
                        errors.push(ConfigError::new(
                            &at,
                            DetailCode::LockedByBroaderLevel,
                            format!("{member}.{field} is locked at {broad_path}"),
                        ));
                    }
                }
            }
        }
    }
}

/// The guardrail attachment lists a `locked` entry protects from narrower `exclude`s.
const ATTACHMENT_LISTS: &[&str] = &["input", "output", "mcp_input", "mcp_output"];

/// A narrower `exclude` naming a guardrail of a broader level's locked attachment list.
fn locked_excludes(
    broad: &Value,
    list: &str,
    narrow: &Value,
    narrow_path: &DocPath,
    broad_path: &DocPath,
    errors: &mut Vec<ConfigError>,
) {
    let attached: Vec<&str> = broad
        .get(list)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let excludes = narrow
        .get("exclude")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    for (i, name) in excludes.enumerate() {
        if name.as_str().is_some_and(|n| attached.contains(&n)) {
            errors.push(ConfigError::new(
                &narrow_path.key("guardrails").key("exclude").index(i),
                DetailCode::LockedByBroaderLevel,
                format!("guardrails.{list} is locked at {broad_path}"),
            ));
        }
    }
}

/// A narrower `detach` of an extension a broader level attached with `locked: true`.
fn locked_detaches(doc: &Value, errors: &mut Vec<ConfigError>) {
    for narrow in policy_levels(doc) {
        let detaches = narrow
            .block
            .get("plugins")
            .and_then(|p| p.get("detach"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten();
        for (i, name) in detaches.enumerate() {
            let Some(name) = name.as_str() else { continue };
            let locked = narrow.ancestors.iter().filter_map(|a| a.get(doc)).any(|broad| {
                broad
                    .get("plugins")
                    .and_then(|p| p.get("attach"))
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .any(|a| {
                        a.get("name").and_then(Value::as_str) == Some(name)
                            && a.get("locked").and_then(Value::as_bool) == Some(true)
                    })
            });
            if locked {
                errors.push(ConfigError::new(
                    &narrow.path.key("plugins").key("detach").index(i),
                    DetailCode::Conflict,
                    format!("extension {name:?} is attached with locked: true at a broader level"),
                ));
            }
        }
    }
}

fn cors(doc: &Value, errors: &mut Vec<ConfigError>) {
    for plane in ["data", "admin"] {
        let Some(c) = doc
            .get("security")
            .and_then(|s| s.get("cors"))
            .and_then(|c| c.get(plane))
        else {
            continue;
        };
        let wildcard = c
            .get("allowed_origins")
            .and_then(Value::as_array)
            .is_some_and(|a| a.iter().any(|o| o.as_str() == Some("*")));
        if wildcard && c.get("allow_credentials").and_then(Value::as_bool) == Some(true) {
            errors.push(ConfigError::new(
                &DocPath::root()
                    .key("security")
                    .key("cors")
                    .key(plane)
                    .key("allow_credentials"),
                DetailCode::Conflict,
                "allow_credentials cannot be combined with the \"*\" origin",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn codes(doc: &Value) -> Vec<(String, DetailCode)> {
        check(doc).into_iter().map(|e| (e.path, e.code)).collect()
    }

    #[test]
    fn unresolved_references_name_the_field() {
        let doc = json!({
            "deployments": {"orphan": {"provider": "nope", "model": "x"}},
            "routes": {"broken": {"tree": {"deployment": "ghost"}}},
            "models": {"mx": {"route": "nowhere"}},
            "projects": {"px": {"team": "no_such_team"}},
            "keys": {"kx": {"project": "no_such_project"}},
            "policies": {"guardrails": {"input": ["no_such_guardrail"]}}
        });
        let got = codes(&doc);
        for path in [
            "deployments.orphan.provider",
            "routes.broken.tree.deployment",
            "models.mx.route",
            "projects.px.team",
            "keys.kx.project",
            "policies.guardrails.input[0]",
        ] {
            assert!(
                got.contains(&(path.to_owned(), DetailCode::UnresolvedReference)),
                "{path}: {got:?}"
            );
        }
    }

    #[test]
    fn default_tenants_always_exist() {
        let doc = json!({"keys": {"k": {}}, "projects": {"p": {"team": "default"}}});
        assert!(check(&doc).is_empty());
    }

    #[test]
    fn class_fallbacks_name_existing_models() {
        let doc = json!({
            "models": {"big": {"target": "d"}},
            "deployments": {"d": {"provider": "p", "model": "x"}},
            "providers": {"p": {"type": "openai"}},
            "policies": {"resilience": {"retry": {"class_fallbacks": {"context_length": "big", "content_policy": "nope"}}}}
        });
        let got = codes(&doc);
        assert!(got.contains(&(
            "policies.resilience.retry.class_fallbacks.content_policy".to_owned(),
            DetailCode::UnresolvedReference
        )));
        assert!(!got.iter().any(|(p, _)| p.ends_with("context_length")));
    }

    #[test]
    fn referrers_list_every_path() {
        let doc =
            json!({"deployments": {"a": {"provider": "p"}, "b": {"provider": "p"}}, "providers": {"p": {}}});
        let paths: Vec<String> = referrers(&doc, "providers", "p")
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(paths, vec!["deployments.a.provider", "deployments.b.provider"]);
    }

    #[test]
    fn locked_fields_cannot_be_set_below() {
        let doc = json!({
            "providers": {"p": {}}, "deployments": {"p": {"provider": "p"}},
            "policies": {"resilience": {"retry": {"max_attempts": 2}, "locked": ["retry.max_attempts"]}},
            "models": {"m": {"target": "p", "policies": {"resilience": {"retry": {"max_attempts": 5}}}}}
        });
        assert_eq!(
            codes(&doc),
            vec![(
                "models.m.policies.resilience.retry.max_attempts".to_owned(),
                DetailCode::LockedByBroaderLevel
            )]
        );
        let mut ok = doc.clone();
        ok["models"]["m"]["policies"]["resilience"] = json!({"timeouts": {"connect": "5s"}});
        assert!(check(&ok).is_empty());
    }

    #[test]
    fn cycles_and_zero_weights() {
        let doc = json!({
            "providers": {"p": {}}, "deployments": {"d": {"provider": "p"}},
            "routes": {"a": {"tree": {"route": "b"}}, "b": {"tree": {"type": "fallback", "children": [{"route": "a"}]}},
                       "w": {"tree": {"type": "weighted", "children": [{"deployment": "d", "weight": 0}]}}}
        });
        let got = codes(&doc);
        assert!(
            got.contains(&("routes.a.tree".to_owned(), DetailCode::Conflict)),
            "{got:?}"
        );
        assert!(
            got.contains(&("routes.w.tree.children".to_owned(), DetailCode::OutOfRange)),
            "{got:?}"
        );
    }

    #[test]
    fn cors_credentials_with_wildcard() {
        let doc =
            json!({"security": {"cors": {"data": {"allowed_origins": ["*"], "allow_credentials": true}}}});
        assert_eq!(
            codes(&doc),
            vec![(
                "security.cors.data.allow_credentials".to_owned(),
                DetailCode::Conflict
            )]
        );
    }
}
