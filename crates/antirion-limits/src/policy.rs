//! Every limit and budget declaration of one config generation, indexed by the level owner that
//! declares it, so a request's applicable set is a union of a few short lists.

use std::collections::{BTreeMap, HashMap};

use antirion_core::ResourceUid;
use serde_json::Value;

use crate::decl::{BudgetDecl, DeclError, LimitDecl};
use crate::level::Level;
use crate::period::PeriodError;
use crate::settings::{BudgetsSettings, LimitsSettings, PricingSettings};
use crate::weights::WeightsError;

/// A declaration's content.
#[derive(Clone, Debug, PartialEq)]
pub enum DeclKind {
    Limit(LimitDecl),
    Budget(BudgetDecl),
}

/// A declaration with its owner.
#[derive(Clone, Debug, PartialEq)]
pub struct Declared {
    pub level: Level,
    /// The owning resource's name (`-` for the gateway); a node's route path.
    pub owner: String,
    pub owner_uid: ResourceUid,
    pub name: String,
    pub kind: DeclKind,
}

impl Declared {
    /// `"<level>.<name>"`, the policy name of the rate-limit headers.
    #[must_use]
    pub fn policy_name(&self) -> String {
        format!("{}.{}", self.level.as_str(), self.name)
    }
}

/// A declaration that does not compile, with its document path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileError {
    pub path: String,
    pub message: String,
}

/// Every declaration and the `limits`, `budgets` and `pricing` settings of one generation.
#[derive(Clone, Debug)]
pub struct Policies {
    pub limits: LimitsSettings,
    pub budgets: BudgetsSettings,
    pub pricing: PricingSettings,
    decls: Vec<Declared>,
    by_owner: HashMap<(Level, String), Vec<usize>>,
    organizations: HashMap<String, ResourceUid>,
    /// Some budget declares a downgrade.
    downgrades: bool,
}

impl Default for Policies {
    fn default() -> Self {
        Policies {
            limits: LimitsSettings::default(),
            budgets: BudgetsSettings::from_value(None),
            pricing: PricingSettings::from_value(None).unwrap_or_else(|_| PricingSettings {
                bundled: true,
                on_unpriced: crate::settings::OnUnpriced::RejectIfBudgeted,
                report_in_body: false,
                multipliers: BTreeMap::new(),
                tools: BTreeMap::new(),
                agents: BTreeMap::new(),
                models: BTreeMap::new(),
            }),
            decls: Vec::new(),
            by_owner: HashMap::new(),
            organizations: HashMap::new(),
            downgrades: false,
        }
    }
}

struct Builder<'a> {
    p: Policies,
    errors: Vec<CompileError>,
    uid_of: &'a dyn Fn(&str, &str) -> ResourceUid,
}

impl Builder<'_> {
    fn policies(
        &mut self,
        level: Level,
        owner: &str,
        uid: ResourceUid,
        policies: Option<&Value>,
        path: &str,
    ) {
        let Some(policies) = policies else { return };
        for (member, is_limit) in [("limits", true), ("budgets", false)] {
            for (name, v) in policies
                .get(member)
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
            {
                if name == "locked" || name == "request_overridable" {
                    continue;
                }
                let at = format!("{path}.{member}.{name}");
                let kind = if is_limit {
                    LimitDecl::parse(v, &self.p.limits).map(DeclKind::Limit)
                } else {
                    BudgetDecl::parse(v, &self.p.budgets).map(DeclKind::Budget)
                };
                self.add(level, owner, uid, name, kind, at);
            }
        }
    }

    fn add(
        &mut self,
        level: Level,
        owner: &str,
        uid: ResourceUid,
        name: &str,
        kind: Result<DeclKind, DeclError>,
        at: String,
    ) {
        match kind {
            Ok(kind) => {
                if matches!(&kind, DeclKind::Budget(b) if b.downgrade.is_some()) {
                    self.p.downgrades = true;
                }
                let ix = self.p.decls.len();
                self.p.decls.push(Declared {
                    level,
                    owner: owner.to_owned(),
                    owner_uid: uid,
                    name: name.to_owned(),
                    kind,
                });
                self.p
                    .by_owner
                    .entry((level, owner.to_owned()))
                    .or_default()
                    .push(ix);
            }
            Err(e) => self.errors.push(CompileError {
                path: error_path(&at, &e),
                message: e.to_string(),
            }),
        }
    }

    /// Registers the declarations of a tree's nodes under their route paths.
    fn tree(&mut self, node: &Value, path: &str, uid: ResourceUid, doc_path: &str) {
        let Some(obj) = node.as_object() else { return };
        self.policies(
            Level::Node,
            path,
            uid,
            obj.get("policies"),
            &format!("{doc_path}.policies"),
        );
        if obj.contains_key("route") {
            return;
        }
        let child = |this: &mut Self, n: &Value, label: String, at: String| {
            let seg = segment(n).unwrap_or(label);
            this.tree(n, &format!("{path}/{seg}"), uid, &at);
        };
        for (i, c) in obj
            .get("children")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            child(self, c, i.to_string(), format!("{doc_path}.children[{i}]"));
        }
        for (i, r) in obj
            .get("rules")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            if let Some(t) = r.get("target") {
                child(self, t, format!("r{i}"), format!("{doc_path}.rules[{i}].target"));
            }
        }
        for (i, c) in obj
            .get("classes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            if let (Some(name), Some(t)) = (c.get("name").and_then(Value::as_str), c.get("target")) {
                child(
                    self,
                    t,
                    name.to_owned(),
                    format!("{doc_path}.classes[{i}].target"),
                );
            }
        }
        for key in ["default", "strong", "weak"] {
            if let Some(t) = obj.get(key) {
                child(self, t, key.to_owned(), format!("{doc_path}.{key}"));
            }
        }
    }
}

/// The document path of a declaration error: the field it names, when it names one.
fn error_path(at: &str, e: &DeclError) -> String {
    match e {
        DeclError::Field { field, .. } => format!("{at}.{field}"),
        DeclError::Period(PeriodError::TimeZone(_)) => format!("{at}.timezone"),
        DeclError::Period(PeriodError::Period(_)) => format!("{at}.period"),
        DeclError::Period(PeriodError::Weekday(_)) => format!("{at}.week_start"),
        DeclError::Weights(WeightsError::Preset(_)) => format!("{at}.tokens"),
        DeclError::Weights(WeightsError::Class(c) | WeightsError::Value { class: c }) => {
            format!("{at}.weights.{c}")
        }
        DeclError::Shape(_) | DeclError::Price(_) => at.to_owned(),
    }
}

/// A node's own segment: its `id`, else a leaf's deployment (or `<provider>:<model>`), else a
/// route reference's name.
fn segment(n: &Value) -> Option<String> {
    if let Some(name) = n.as_str() {
        return Some(name.to_owned());
    }
    if let Some(id) = n.get("id").and_then(Value::as_str) {
        return Some(id.to_owned());
    }
    if let Some(d) = n.get("deployment").and_then(Value::as_str) {
        return Some(d.to_owned());
    }
    if let (Some(p), Some(m)) = (
        n.get("provider").and_then(Value::as_str),
        n.get("upstream_model").and_then(Value::as_str),
    ) {
        return Some(format!("{p}:{m}"));
    }
    n.get("route").and_then(Value::as_str).map(str::to_owned)
}

impl Policies {
    /// Compiles every declaration of a validated, secret-resolved document. `uid_of(collection,
    /// name)` is the incarnation of a resource, so counters of a deleted and re-created owner start
    /// fresh.
    pub fn compile(
        doc: &Value,
        uid_of: &dyn Fn(&str, &str) -> ResourceUid,
    ) -> Result<Policies, Vec<CompileError>> {
        let mut errors = Vec::new();
        let pricing = PricingSettings::from_value(doc.get("pricing")).unwrap_or_else(|e| {
            errors.push(CompileError {
                path: "pricing".into(),
                message: e.to_string(),
            });
            Policies::default().pricing
        });
        let mut b = Builder {
            p: Policies {
                limits: LimitsSettings::from_value(doc.get("limits")),
                budgets: BudgetsSettings::from_value(doc.get("budgets")),
                pricing,
                ..Policies::default()
            },
            errors,
            uid_of,
        };
        b.policies(
            Level::Gateway,
            "-",
            ResourceUid::GATEWAY,
            doc.get("policies"),
            "policies",
        );
        for level in [
            Level::Organization,
            Level::Team,
            Level::Project,
            Level::Key,
            Level::Route,
            Level::Model,
        ] {
            for (owner, spec) in doc
                .get(level.collection())
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
            {
                let uid = (b.uid_of)(level.collection(), owner);
                if level == Level::Organization {
                    b.p.organizations.insert(owner.clone(), uid);
                }
                let path = format!("{}.{owner}", level.collection());
                b.policies(
                    level,
                    owner,
                    uid,
                    spec.get("policies"),
                    &format!("{path}.policies"),
                );
                let tree = match level {
                    Level::Route => spec.get("tree").map(|t| (t, "tree")),
                    Level::Model => spec.get("target").map(|t| (t, "target")),
                    _ => None,
                };
                if let Some((t, field)) = tree {
                    let root = match t.get("id").and_then(Value::as_str) {
                        Some(id) => format!("{owner}/{id}"),
                        None => owner.clone(),
                    };
                    b.tree(t, &root, uid, &format!("{path}.{field}"));
                }
            }
        }
        for (owner, spec) in doc
            .get("deployments")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            let uid = (b.uid_of)("deployments", owner);
            for (name, v) in spec
                .get("limits")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
            {
                let kind = LimitDecl::parse(v, &b.p.limits).map(DeclKind::Limit);
                b.add(
                    Level::Deployment,
                    owner,
                    uid,
                    name,
                    kind,
                    format!("deployments.{owner}.limits.{name}"),
                );
            }
        }
        if b.errors.is_empty() {
            Ok(b.p)
        } else {
            Err(b.errors)
        }
    }

    /// The declarations owned by `(level, owner)`.
    pub fn owned(&self, level: Level, owner: &str) -> impl Iterator<Item = &Declared> {
        self.by_owner
            .get(&(level, owner.to_owned()))
            .into_iter()
            .flatten()
            .filter_map(|ix| self.decls.get(*ix))
    }

    /// Every declaration.
    pub fn all(&self) -> impl Iterator<Item = &Declared> {
        self.decls.iter()
    }

    /// Whether some budget declares a downgrade, so requests need checking for one.
    #[must_use]
    pub const fn has_downgrades(&self) -> bool {
        self.downgrades
    }

    /// The incarnation of an organization, which groups its tenants' counters in the store.
    #[must_use]
    pub fn organization_uid(&self, name: &str) -> u128 {
        self.organizations.get(name).map_or(0, |u| u.0)
    }

    /// Whether any declaration exists at all (no declarations: no counter-store round trip).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.decls.is_empty()
    }
}

/// Budgets that exceed a parent budget of the same unit and period without `allow_above_parent`.
/// A tenancy budget's parent is the nearest enclosing level (project → team → organization →
/// gateway) that declares a budget of the same unit and period; only that level is compared. A key
/// budget's parent is its project's budget of the same unit and period.
#[must_use]
pub fn budgets_above_parents(doc: &Value) -> Vec<CompileError> {
    let settings = BudgetsSettings::from_value(doc.get("budgets"));
    let budgets_of = |policies: Option<&Value>| -> Vec<(String, BudgetDecl, bool)> {
        policies
            .and_then(|p| p.get("budgets"))
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(n, v)| {
                let allow = v
                    .get("allow_above_parent")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                BudgetDecl::parse(v, &settings)
                    .ok()
                    .map(|b| (n.clone(), b, allow))
            })
            .collect()
    };
    let parent_of = |collection: &str, name: &str| -> Option<(&'static str, String)> {
        let spec = doc.get(collection)?.get(name)?;
        let field = match collection {
            "keys" => "project",
            "projects" => "team",
            "teams" => "organization",
            _ => return None,
        };
        let parent = spec
            .get(field)
            .and_then(Value::as_str)
            .unwrap_or("default")
            .to_owned();
        let parent_collection = match field {
            "project" => "projects",
            "team" => "teams",
            _ => "organizations",
        };
        Some((parent_collection, parent))
    };
    let gateway = budgets_of(doc.get("policies"));
    // The enclosing tenancy levels' budgets, nearest first, ending with the gateway's. A key's
    // budget is compared with its project's only.
    let ancestors = |collection: &str, name: &str| -> Vec<Vec<(String, BudgetDecl, bool)>> {
        let mut out = Vec::new();
        let mut at = parent_of(collection, name);
        while let Some((pc, pn)) = at {
            out.push(budgets_of(
                doc.get(pc)
                    .and_then(|x| x.get(&pn))
                    .and_then(|x| x.get("policies")),
            ));
            if collection == "keys" {
                return out;
            }
            at = parent_of(pc, &pn);
        }
        out.push(gateway.clone());
        out
    };
    let mut errors = Vec::new();
    for collection in ["organizations", "teams", "projects", "keys"] {
        for (name, spec) in doc
            .get(collection)
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            let levels = ancestors(collection, name);
            for (bname, b, allow) in budgets_of(spec.get("policies")) {
                let same = |p: &BudgetDecl| {
                    std::mem::discriminant(&p.unit) == std::mem::discriminant(&b.unit)
                        && p.period_key == b.period_key
                };
                let parent = levels.iter().find(|l| l.iter().any(|(_, p, _)| same(p)));
                let too_big = parent
                    .into_iter()
                    .flatten()
                    .any(|(_, p, _)| same(p) && p.amount < b.amount);
                if too_big && !allow {
                    errors.push(CompileError {
                        path: format!("{collection}.{name}.policies.budgets.{bname}.amount"),
                        message: "exceeds a parent budget of the same unit and period".into(),
                    });
                }
            }
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn uid(_: &str, name: &str) -> ResourceUid {
        ResourceUid(u128::from(name.len() as u64 + 100))
    }

    #[test]
    fn every_level_and_node_paths() {
        let doc = json!({
            "policies": {"limits": {"g": {"unit": "requests", "limit": 1, "per": "1h"}, "locked": ["x"]}},
            "organizations": {"acme": {"policies": {"budgets": {"b": {"unit": "usd", "amount": "1.00"}}}}},
            "keys": {"k1": {"policies": {"limits": {"rpm": {"unit": "requests", "limit": 2, "per": "1m"}}}}},
            "routes": {"chat_frontier": {"tree": {"type": "fallback", "children": [
                {"deployment": "openai_main_gpt5", "policies": {"limits": {"leg_rpm": {"unit": "requests", "limit": 100, "per": "1m"}}}},
                "azure_east_gpt5",
                {"type": "weighted", "id": "pool", "children": [{"deployment": "d", "policies": {"limits": {"x": {"unit": "concurrency", "limit": 1}}}}]}
            ]}}},
            "models": {"m": {"target": {"type": "conditional", "rules": [{"target": {"deployment": "d2", "policies": {"budgets": {"nb": {"unit": "tokens", "amount": 5}}}}}]}}},
            "deployments": {"d": {"provider": "p", "model": "x", "limits": {"rpm": {"unit": "requests", "limit": 2, "per": "1h"}}}}
        });
        let p = Policies::compile(&doc, &uid).unwrap();
        assert_eq!(p.all().count(), 7);
        assert_eq!(p.owned(Level::Gateway, "-").count(), 1);
        assert_eq!(p.owned(Level::Key, "k1").count(), 1);
        assert_eq!(
            p.owned(Level::Node, "chat_frontier/openai_main_gpt5")
                .next()
                .unwrap()
                .name,
            "leg_rpm"
        );
        assert_eq!(p.owned(Level::Node, "chat_frontier/pool/d").count(), 1);
        assert_eq!(p.owned(Level::Node, "m/d2").count(), 1);
        assert_eq!(p.owned(Level::Deployment, "d").count(), 1);
        assert_eq!(p.organization_uid("acme"), 104);
        assert_eq!(p.owned(Level::Key, "k1").next().unwrap().policy_name(), "key.rpm");
    }

    #[test]
    fn errors_carry_paths() {
        let doc = json!({"keys": {"k": {"policies": {"budgets": {"b": {"unit": "usd", "amount": "1", "period": "daily", "timezone": "Nowhere/Land"}}}}}});
        let errs = Policies::compile(&doc, &uid).unwrap_err();
        assert_eq!(errs[0].path, "keys.k.policies.budgets.b.timezone");
    }

    #[test]
    fn child_budgets_above_parents() {
        let doc = json!({
            "organizations": {"o": {"policies": {"budgets": {"m": {"unit": "usd", "amount": "100", "period": "monthly"}}}}},
            "teams": {"big": {"organization": "o", "policies": {"budgets": {"m": {"unit": "usd", "amount": "999", "period": "monthly"}}}},
                      "ok": {"organization": "o", "policies": {"budgets": {"m": {"unit": "usd", "amount": "50", "period": "monthly"}}}},
                      "allowed": {"organization": "o", "policies": {"budgets": {"m": {"unit": "usd", "amount": "999", "period": "monthly", "allow_above_parent": true}}}},
                      "daily": {"organization": "o", "policies": {"budgets": {"m": {"unit": "usd", "amount": "999", "period": "daily"}}}}},
            "projects": {"p": {"team": "ok"}},
            "keys": {"k": {"project": "p", "policies": {"budgets": {"m": {"unit": "usd", "amount": "999", "period": "monthly"}}}}}
        });
        let paths: Vec<String> = budgets_above_parents(&doc).into_iter().map(|e| e.path).collect();
        assert_eq!(paths, ["teams.big.policies.budgets.m.amount"]);
    }

    #[test]
    fn the_parent_budget_is_the_nearest_level_that_has_one() {
        let budget =
            |amount: &str| json!({"budgets": {"m": {"unit": "usd", "amount": amount, "period": "monthly"}}});
        let doc = json!({
            "policies": budget("1"),
            "organizations": {"o": {"policies": budget("100")}},
            "teams": {"t": {"organization": "o"}},
            "projects": {"over": {"team": "t", "policies": budget("500")},
                         "under": {"team": "t", "policies": budget("50")}}
        });
        let paths: Vec<String> = budgets_above_parents(&doc).into_iter().map(|e| e.path).collect();
        assert_eq!(
            paths,
            [
                "organizations.o.policies.budgets.m.amount",
                "projects.over.policies.budgets.m.amount"
            ],
            "a project under a team without budgets is compared with the organization's, not the gateway's"
        );
    }
}
