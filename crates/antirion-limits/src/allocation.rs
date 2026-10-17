//! How budgets are allocated below a parent: a budget's parent is the nearest enclosing tenancy
//! level that declares a budget of the same unit and period (a key's is its project's only), as for
//! `allow_above_parent`. The children of a parent are allocated the sum of their amounts, and a
//! parent whose children add up to more than its own amount is over-allocated, which is a warning,
//! not an error: every budget along the chain is still enforced.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::decl::BudgetDecl;
use crate::level::Level;
use crate::settings::BudgetsSettings;

/// A budget by its owner and name.
pub type BudgetRef = (Level, String, String);

/// The children of one parent budget.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Allocation {
    pub parent: BudgetRef,
    /// The parent's declared amount.
    pub amount: u128,
    /// The sum of its children's declared amounts.
    pub allocated: u128,
    pub children: Vec<BudgetRef>,
}

impl Allocation {
    #[must_use]
    pub fn over(&self) -> bool {
        self.allocated > self.amount
    }
}

type Declared = Vec<(String, BudgetDecl)>;

fn budgets_of(policies: Option<&Value>, settings: &BudgetsSettings) -> Declared {
    policies
        .and_then(|p| p.get("budgets"))
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(n, v)| BudgetDecl::parse(v, settings).ok().map(|b| (n.clone(), b)))
        .collect()
}

fn level_of(collection: &str) -> Level {
    match collection {
        "organizations" => Level::Organization,
        "teams" => Level::Team,
        "projects" => Level::Project,
        "keys" => Level::Key,
        _ => Level::Gateway,
    }
}

/// The resource a tenancy resource sits in, as `(collection, name)`.
fn parent_of(doc: &Value, collection: &str, name: &str) -> Option<(&'static str, String)> {
    let (field, parent) = match collection {
        "keys" => ("project", "projects"),
        "projects" => ("team", "teams"),
        "teams" => ("organization", "organizations"),
        "organizations" => return Some(("-", "-".to_owned())),
        _ => return None,
    };
    let spec = doc.get(collection)?.get(name)?;
    let named = spec
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or("default")
        .to_owned();
    Some((parent, named))
}

fn same(a: &BudgetDecl, b: &BudgetDecl) -> bool {
    std::mem::discriminant(&a.unit) == std::mem::discriminant(&b.unit) && a.period_key == b.period_key
}

/// Every parent budget that has children, with what they are allocated.
#[must_use]
pub fn allocations(doc: &Value) -> Vec<Allocation> {
    let settings = BudgetsSettings::from_value(doc.get("budgets"));
    let declared = |collection: &str, name: &str| -> Declared {
        if collection == "-" {
            return budgets_of(doc.get("policies"), &settings);
        }
        budgets_of(
            doc.get(collection)
                .and_then(|c| c.get(name))
                .and_then(|r| r.get("policies")),
            &settings,
        )
    };
    let mut out: BTreeMap<BudgetRef, Allocation> = BTreeMap::new();
    for collection in ["organizations", "teams", "projects", "keys"] {
        for name in doc
            .get(collection)
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|m| m.keys())
        {
            for (bname, b) in declared(collection, name) {
                let mut at = parent_of(doc, collection, name);
                while let Some((pc, pn)) = at {
                    let parents: Vec<(String, BudgetDecl)> = declared(pc, &pn)
                        .into_iter()
                        .filter(|(_, p)| same(p, &b))
                        .collect();
                    if !parents.is_empty() {
                        for (pname, p) in parents {
                            let key = (level_of(pc), pn.clone(), pname);
                            let entry = out.entry(key.clone()).or_insert_with(|| Allocation {
                                parent: key,
                                amount: p.amount,
                                allocated: 0,
                                children: Vec::new(),
                            });
                            entry.allocated = entry.allocated.saturating_add(b.amount);
                            entry
                                .children
                                .push((level_of(collection), name.clone(), bname.clone()));
                        }
                        break;
                    }
                    if collection == "keys" || pc == "-" {
                        break;
                    }
                    at = parent_of(doc, pc, &pn);
                }
            }
        }
    }
    out.into_values().collect()
}

/// The document path of a budget's amount.
#[must_use]
pub fn amount_path(b: &BudgetRef) -> String {
    let (level, owner, name) = b;
    match level {
        Level::Gateway => format!("policies.budgets.{name}.amount"),
        other => format!("{}.{owner}.policies.budgets.{name}.amount", other.collection()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn usd(amount: &str) -> Value {
        json!({"unit": "usd", "amount": amount, "period": "monthly"})
    }

    #[test]
    fn children_add_up_against_their_nearest_parent() {
        let doc = json!({
            "teams": {"t": {"policies": {"budgets": {"monthly": usd("100")}}}},
            "projects": {
                "a": {"team": "t", "policies": {"budgets": {"m": usd("60")}}},
                "b": {"team": "t", "policies": {"budgets": {"m": usd("60"),
                    "daily": {"unit": "usd", "amount": "5", "period": "daily"}}}}
            },
            "keys": {"k": {"project": "a", "policies": {"budgets": {"m": usd("10")}}}}
        });
        let all = allocations(&doc);
        let team = all.iter().find(|a| a.parent.0 == Level::Team).unwrap();
        assert_eq!(team.allocated, 120_000_000_000);
        assert!(team.over());
        assert_eq!(team.children.len(), 2);
        let project = all
            .iter()
            .find(|a| a.parent == (Level::Project, "a".into(), "m".into()))
            .unwrap();
        assert!(!project.over());
        assert_eq!(
            amount_path(&team.parent),
            "teams.t.policies.budgets.monthly.amount"
        );
    }

    #[test]
    fn a_level_without_a_matching_budget_is_skipped() {
        let doc = json!({
            "policies": {"budgets": {"g": usd("50")}},
            "teams": {"t": {"organization": "o", "policies": {"budgets": {"x": usd("30")}}}},
            "organizations": {"o": {}}
        });
        let all = allocations(&doc);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].parent, (Level::Gateway, "-".into(), "g".into()));
        assert_eq!(all[0].allocated, 30_000_000_000);
    }
}
