//! Tenancy rules of the Admin API: the default organization, team and project always exist, a
//! cascading delete removes the whole subtree, and deleting a user disables the keys it owns.

use std::collections::BTreeSet;

use antirion_config::model::{CollectionDef, DEFAULT_TENANT};
use antirion_runtime::MetaEdit;
use serde_json::{Value, json};

/// The resources a cascading delete of a tenancy resource removes with it.
pub(crate) fn descendants(doc: &Value, c: &CollectionDef, name: &str) -> Vec<(&'static str, String)> {
    let children = |collection: &str, field: &str, parents: &BTreeSet<String>| -> BTreeSet<String> {
        let default = (field == "project").then_some(DEFAULT_TENANT);
        doc.get(collection)
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(_, s)| {
                s.get(field)
                    .and_then(Value::as_str)
                    .or(default)
                    .is_some_and(|p| parents.contains(p))
            })
            .map(|(n, _)| n.clone())
            .collect()
    };
    let one = BTreeSet::from([name.to_owned()]);
    let (teams, projects) = match c.name {
        "organizations" => {
            let teams = children("teams", "organization", &one);
            let projects = children("projects", "team", &teams);
            (teams, projects)
        }
        "teams" => (BTreeSet::new(), children("projects", "team", &one)),
        "projects" => (BTreeSet::new(), one.clone()),
        _ => return Vec::new(),
    };
    let keys = children("keys", "project", &projects);
    let mut out: Vec<(&'static str, String)> = teams.into_iter().map(|t| ("teams", t)).collect();
    out.extend(
        projects
            .into_iter()
            .filter(|p| !(c.name == "projects" && p == name))
            .map(|p| ("projects", p)),
    );
    out.extend(keys.into_iter().map(|k| ("keys", k)));
    out
}

/// The spec of a default organization, team or project that was never written to the document.
pub(crate) fn implicit_default(collection: &str, name: &str) -> Option<Value> {
    if name != DEFAULT_TENANT {
        return None;
    }
    match collection {
        "organizations" => Some(json!({})),
        "teams" => Some(json!({"organization": DEFAULT_TENANT})),
        "projects" => Some(json!({"team": DEFAULT_TENANT})),
        _ => None,
    }
}

/// Disables, in `doc`, every key owned by `user` and drops the reference to it; returns the keys
/// and the metadata that records why they were disabled.
pub(crate) fn disable_owned_keys(doc: &mut Value, user: &str) -> (Vec<String>, Vec<MetaEdit>) {
    let mut names = Vec::new();
    for (name, spec) in doc
        .get_mut("keys")
        .and_then(Value::as_object_mut)
        .into_iter()
        .flatten()
    {
        if spec.get("owner").and_then(Value::as_str) != Some(user) {
            continue;
        }
        if let Some(obj) = spec.as_object_mut() {
            obj.remove("owner");
            obj.insert("enabled".into(), Value::Bool(false));
        }
        names.push(name.clone());
    }
    let edits = names
        .iter()
        .map(|k| MetaEdit::DisabledReason {
            key: k.clone(),
            reason: "owner_deleted".to_owned(),
        })
        .collect();
    (names, edits)
}

#[cfg(test)]
mod tests {
    use antirion_config::model;

    use super::*;

    #[test]
    fn cascades_follow_the_hierarchy() {
        let doc = json!({
            "teams": {"t": {"organization": "o"}, "other": {"organization": "x"}},
            "projects": {"p": {"team": "t"}, "q": {"team": "other"}},
            "keys": {"k1": {"project": "p"}, "k2": {"project": "q"}, "k3": {}}
        });
        let org = model::collection("organizations").unwrap();
        let got = descendants(&doc, org, "o");
        assert_eq!(
            got,
            vec![
                ("teams", "t".to_owned()),
                ("projects", "p".to_owned()),
                ("keys", "k1".to_owned())
            ]
        );
        let project = model::collection("projects").unwrap();
        assert_eq!(descendants(&doc, project, "p"), vec![("keys", "k1".to_owned())]);
    }

    #[test]
    fn defaults_exist_only_by_name() {
        assert_eq!(
            implicit_default("teams", "default"),
            Some(json!({"organization": "default"}))
        );
        assert!(implicit_default("teams", "other").is_none());
        assert!(implicit_default("keys", "default").is_none());
    }

    #[test]
    fn owned_keys_are_disabled_and_released() {
        let mut doc = json!({"keys": {"a": {"owner": "u"}, "b": {"owner": "v"}, "c": {}}});
        let (names, edits) = disable_owned_keys(&mut doc, "u");
        assert_eq!(names, vec!["a".to_owned()]);
        assert_eq!(edits.len(), 1);
        assert_eq!(doc["keys"]["a"], json!({"enabled": false}));
        assert_eq!(doc["keys"]["b"], json!({"owner": "v"}));
    }
}
