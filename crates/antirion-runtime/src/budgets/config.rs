//! The configuration rules budgets add: an `owner` names a user, a downgrade names a Model or a
//! tier that exists, and a parent budget whose children add up to more than its amount is reported
//! as a warning (`budget_over_allocated`), never an error.

use antirion_config::{ConfigError, DetailCode, DocPath};
use antirion_limits::allocation::{allocations, amount_path};
use antirion_limits::money::Nusd;
use serde_json::{Map, Value, json};

fn exists(doc: &Value, collection: &str, name: &str) -> bool {
    doc.get(collection).and_then(|c| c.get(name)).is_some()
}

fn model_known(doc: &Value, name: &str) -> bool {
    exists(doc, "models", name)
        || doc
            .get("models")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|m| m.values())
            .filter_map(|m| m.get("aliases").and_then(Value::as_array))
            .flatten()
            .any(|a| a.as_str() == Some(name))
}

fn budget_refs(doc: &Value, at: &DocPath, budgets: &Map<String, Value>, errors: &mut Vec<ConfigError>) {
    for (name, b) in budgets {
        let here = at.key(name);
        if let Some(owner) = b.get("owner").and_then(Value::as_str)
            && !exists(doc, "users", owner)
        {
            errors.push(ConfigError::new(
                &here.key("owner"),
                DetailCode::UnresolvedReference,
                format!("no user is named {owner:?}"),
            ));
        }
        let Some(d) = b.get("downgrade") else {
            continue;
        };
        if let Some(m) = d.get("to_model").and_then(Value::as_str)
            && !model_known(doc, m)
        {
            errors.push(ConfigError::new(
                &here.key("downgrade").key("to_model"),
                DetailCode::UnresolvedReference,
                format!("no Model is named {m:?}"),
            ));
        }
        if let Some(t) = d.get("to_tier").and_then(Value::as_str)
            && !antirion_router::tier_known(doc, t)
        {
            errors.push(ConfigError::new(
                &here.key("downgrade").key("to_tier"),
                DetailCode::UnresolvedReference,
                format!("no tier is named {t:?}"),
            ));
        }
    }
}

fn walk(doc: &Value, v: &Value, at: &DocPath, errors: &mut Vec<ConfigError>) {
    match v {
        Value::Object(m) => {
            for (k, child) in m {
                let here = at.key(k);
                if k == "policies"
                    && let Some(budgets) = child.get("budgets").and_then(Value::as_object)
                {
                    budget_refs(doc, &here.key("budgets"), budgets, errors);
                }
                walk(doc, child, &here, errors);
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                walk(doc, child, &at.index(i), errors);
            }
        }
        _ => {}
    }
}

/// The reference errors of every budget declaration of a document.
#[must_use]
pub fn check_document(doc: &Value) -> Vec<ConfigError> {
    let mut errors = Vec::new();
    walk(doc, doc, &DocPath::root(), &mut errors);
    errors
}

/// The `budget_over_allocated` warnings of a document, in the Admin API's warning form.
#[must_use]
pub fn warnings(doc: &Value) -> Vec<Value> {
    let money = |v: u128| Nusd::from_units(v).to_usd_string();
    allocations(doc)
        .into_iter()
        .filter(|a| a.over())
        .map(|a| {
            let (level, owner, name) = &a.parent;
            json!({
                "path": amount_path(&a.parent),
                "code": "budget_over_allocated",
                "message": format!(
                    "the {} budgets directly below {} budget {owner}/{name} add up to {}, more than its amount {}",
                    a.children.len(),
                    level.as_str(),
                    money(a.allocated),
                    money(a.amount)
                ),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owners_and_downgrade_targets_must_exist() {
        let doc = json!({
            "users": {"alice": {}},
            "models": {"cheap": {"aliases": ["thrifty"]}},
            "teams": {"t": {"policies": {"budgets": {
                "a": {"owner": "alice", "downgrade": {"at": 0.5, "to_model": "thrifty"}},
                "b": {"owner": "ghost", "downgrade": {"at": 0.5, "to_model": "gone"}},
                "c": {"downgrade": {"at": 0.5, "to_tier": "huge"}},
                "d": {"downgrade": {"at": 0.5, "to_tier": "fast"}}
            }}}},
            "routes": {"r": {"tree": {"type": "fallback", "children": [
                {"type": "deployment", "deployment": "p", "policies": {"budgets": {"n": {"owner": "nobody"}}}}
            ]}}}
        });
        let got: Vec<(String, DetailCode)> = check_document(&doc)
            .into_iter()
            .map(|e| (e.path, e.code))
            .collect();
        let paths: Vec<&str> = got.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "teams.t.policies.budgets.b.owner",
                "teams.t.policies.budgets.b.downgrade.to_model",
                "teams.t.policies.budgets.c.downgrade.to_tier",
                "routes.r.tree.children[0].policies.budgets.n.owner",
            ]
        );
        assert!(got.iter().all(|(_, c)| *c == DetailCode::UnresolvedReference));
    }

    #[test]
    fn over_allocation_is_a_warning_at_the_parent() {
        let usd = |a: &str| json!({"unit": "usd", "amount": a, "period": "monthly"});
        let doc = json!({
            "teams": {"t": {"policies": {"budgets": {"m": usd("100")}}}},
            "projects": {"a": {"team": "t", "policies": {"budgets": {"m": usd("60")}}},
                         "b": {"team": "t", "policies": {"budgets": {"m": usd("60")}}}}
        });
        let w = warnings(&doc);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0]["code"], "budget_over_allocated");
        assert_eq!(w[0]["path"], "teams.t.policies.budgets.m.amount");
    }
}
