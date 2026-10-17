//! Change sets between two documents, and the document an apply would produce.

use std::collections::BTreeSet;

use antirion_config::model::{self, COLLECTIONS, CollectionDef, STATIC_KEYS};
use antirion_config::semantic;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::state::ConfigState;

/// What a change does at its path.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    Create,
    Update,
    Delete,
}

/// One entry of a change set, with `before` and `after` already redacted.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Change {
    pub op: Op,
    pub path: String,
    pub before: Option<Value>,
    pub after: Option<Value>,
}

/// What a change is about: a resource of a collection, or a singleton.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Target {
    Resource(&'static str, String),
    Setting(&'static str),
}

impl Target {
    #[must_use]
    pub fn path(&self) -> String {
        match self {
            Target::Resource(c, name) => {
                model::collection(c).map_or_else(String::new, |c| c.resource_path(name).to_string())
            }
            Target::Setting(s) => (*s).to_owned(),
        }
    }
}

/// Every resource and singleton that differs between `old` and `new`, in document order.
#[must_use]
pub fn compute(old: &Value, new: &Value) -> Vec<(Target, Op, Option<Value>, Option<Value>)> {
    let mut out = Vec::new();
    for c in COLLECTIONS {
        let empty = Map::new();
        let a = model::collection_map(old, c).unwrap_or(&empty);
        let b = model::collection_map(new, c).unwrap_or(&empty);
        let names: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
        for name in names {
            let (before, after) = (a.get(name), b.get(name));
            let op = match (before, after) {
                (None, Some(_)) => Op::Create,
                (Some(_), None) => Op::Delete,
                (Some(x), Some(y)) if x != y => Op::Update,
                _ => continue,
            };
            out.push((
                Target::Resource(c.name, name.clone()),
                op,
                before.cloned(),
                after.cloned(),
            ));
        }
    }
    for s in model::SINGLETONS {
        let before = model::singleton_value(old, s).filter(|v| !is_empty_object(v));
        let after = model::singleton_value(new, s).filter(|v| !is_empty_object(v));
        let op = match (&before, &after) {
            (None, Some(_)) => Op::Create,
            (Some(_), None) => Op::Delete,
            (Some(x), Some(y)) if x != y => Op::Update,
            _ => continue,
        };
        out.push((Target::Setting(s.name), op, before, after));
    }
    let before = old.pointer("/server/admission").cloned();
    let after = new.pointer("/server/admission").cloned();
    let op = match (&before, &after) {
        (None, Some(_)) => Some(Op::Create),
        (Some(_), None) => Some(Op::Delete),
        (Some(x), Some(y)) if x != y => Some(Op::Update),
        _ => None,
    };
    if let Some(op) = op {
        out.push((Target::Setting("server.admission"), op, before, after));
    }
    out
}

fn is_empty_object(v: &Value) -> bool {
    v.as_object().is_some_and(Map::is_empty)
}

/// The document an apply would produce, before validation.
#[derive(Debug)]
pub struct ApplyPlan {
    /// The resulting document; it still holds the incoming `version` and static keys, so that
    /// validation sees them.
    pub candidate: Value,
    /// What the apply wrote; these take `managed_by: apply:<source>` when they change.
    pub written: Vec<Target>,
    pub static_ignored: Vec<String>,
}

fn written_collection(key: &str) -> Option<&'static CollectionDef> {
    COLLECTIONS
        .iter()
        .find(|c| c.doc_path.len() == 1 && c.doc_path.first() == Some(&key))
}

/// Merges an applied document into the current configuration. With `prune`, resources and settings
/// this source manages that the document no longer has are removed, except those that remaining
/// resources still reference, which stay until nothing does.
#[must_use]
pub fn merge_apply(
    current: &ConfigState,
    incoming: &Map<String, Value>,
    source: &str,
    prune: bool,
    statics: &Value,
) -> ApplyPlan {
    let mut candidate = current.doc.clone();
    let mut written = Vec::new();
    let mut static_ignored = Vec::new();
    set_top(&mut candidate, "version", incoming.get("version").cloned());
    let mut settings_written: BTreeSet<&str> = BTreeSet::new();
    for (key, value) in incoming {
        if key == "version" {
            continue;
        }
        if STATIC_KEYS.contains(&key.as_str()) {
            let applied = without_admission(key, Some(value));
            if applied.is_some() && without_admission(key, statics.get(key)) != applied {
                static_ignored.push(key.clone());
            }
            set_top(&mut candidate, key, Some(value.clone()));
            continue;
        }
        if let Some(c) = written_collection(key) {
            match value.as_object() {
                Some(entries) => {
                    for (name, spec) in entries {
                        insert_resource(&mut candidate, c, name, spec.clone());
                        written.push(Target::Resource(c.name, name.clone()));
                    }
                }
                None => set_top(&mut candidate, key, Some(value.clone())),
            }
            continue;
        }
        if let Some(s) = model::singleton(key) {
            let mut settings = value.clone();
            if let Some(obj) = settings.as_object_mut() {
                for nested in s.nested {
                    let Some(entries) = obj.remove(*nested) else {
                        continue;
                    };
                    let Some(c) = COLLECTIONS.iter().find(|c| c.doc_path == [s.name, *nested]) else {
                        continue;
                    };
                    for (name, spec) in entries.as_object().into_iter().flatten() {
                        insert_resource(&mut candidate, c, name, spec.clone());
                        written.push(Target::Resource(c.name, name.clone()));
                    }
                }
            }
            for c in COLLECTIONS
                .iter()
                .filter(|c| c.doc_path.len() > 2 && c.doc_path.first() == Some(&s.name))
            {
                let mut at = Some(&settings);
                for seg in c.doc_path.iter().skip(1) {
                    at = at.and_then(|v| v.get(*seg));
                }
                for name in at.and_then(Value::as_object).into_iter().flat_map(Map::keys) {
                    written.push(Target::Resource(c.name, name.clone()));
                }
            }
            if settings.is_object() {
                model::set_singleton(&mut candidate, s, Some(settings));
            } else {
                set_top(&mut candidate, key, Some(settings));
            }
            settings_written.insert(s.name);
            written.push(Target::Setting(s.name));
            continue;
        }
        set_top(&mut candidate, key, Some(value.clone()));
    }
    if prune {
        prune_source(
            current,
            &mut candidate,
            &written,
            &settings_written,
            &format!("apply:{source}"),
        );
    }
    keep_referenced_auth(&current.doc, &mut candidate);
    ApplyPlan {
        candidate,
        written,
        static_ignored,
    }
}

/// A static key's value without `server.admission`, which is dynamic.
fn without_admission(key: &str, value: Option<&Value>) -> Option<Value> {
    let mut v = value.cloned()?;
    if key == "server"
        && let Some(obj) = v.as_object_mut()
    {
        obj.remove("admission");
        if obj.is_empty() {
            return None;
        }
    }
    Some(v)
}

fn set_top(doc: &mut Value, key: &str, value: Option<Value>) {
    if let Some(root) = doc.as_object_mut() {
        match value {
            Some(v) => {
                root.insert(key.to_owned(), v);
            }
            None => {
                root.remove(key);
            }
        }
    }
}

fn insert_resource(doc: &mut Value, c: &CollectionDef, name: &str, spec: Value) {
    if let Some(map) = model::collection_map_mut(doc, c) {
        map.insert(name.to_owned(), spec);
    }
}

/// Puts back the IP sets and model groups an apply's `auth` dropped while a tenant or key still
/// names them, as a prune keeps a resource that is still referenced: the document that wrote `auth`
/// owns its settings, not the references other writers made to them.
fn keep_referenced_auth(current: &Value, candidate: &mut Value) {
    for (map, name) in antirion_config::auth_references(candidate) {
        let Some(kept) = current
            .get("auth")
            .and_then(|a| a.get(map))
            .and_then(|m| m.get(&name))
        else {
            continue;
        };
        let Some(auth) = candidate
            .as_object_mut()
            .map(|c| c.entry("auth").or_insert_with(|| Value::Object(Map::new())))
        else {
            continue;
        };
        let Some(entries) = auth
            .as_object_mut()
            .map(|a| a.entry(map).or_insert_with(|| Value::Object(Map::new())))
            .and_then(Value::as_object_mut)
        else {
            continue;
        };
        entries.entry(name).or_insert_with(|| kept.clone());
    }
}

fn prune_source(
    current: &ConfigState,
    candidate: &mut Value,
    written: &[Target],
    settings_written: &BTreeSet<&str>,
    managed_by: &str,
) {
    for s in model::SINGLETONS {
        let managed = current
            .meta
            .settings
            .get(s.name)
            .is_some_and(|m| m.managed_by == managed_by);
        if managed && !settings_written.contains(s.name) {
            model::set_singleton(candidate, s, None);
        }
    }
    let mut pruned: Vec<(&'static CollectionDef, String, Value)> = Vec::new();
    for c in COLLECTIONS {
        let Some(metas) = current.meta.resources.get(c.name) else {
            continue;
        };
        for (name, meta) in metas {
            if meta.managed_by != managed_by || written.contains(&Target::Resource(c.name, name.clone())) {
                continue;
            }
            let Some(map) = model::collection_map_mut(candidate, c) else {
                continue;
            };
            if let Some(spec) = map.remove(name) {
                pruned.push((c, name.clone(), spec));
            }
        }
    }
    loop {
        let referenced: Vec<usize> = pruned
            .iter()
            .enumerate()
            .filter(|(_, (c, name, _))| !semantic::referrers(candidate, c.name, name).is_empty())
            .map(|(i, _)| i)
            .collect();
        if referenced.is_empty() {
            break;
        }
        for i in referenced.into_iter().rev() {
            let (c, name, spec) = pruned.remove(i);
            if let Some(map) = model::collection_map_mut(candidate, c) {
                map.insert(name, spec);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::state::{Metadata, ResourceMeta};

    fn meta(managed_by: &str) -> ResourceMeta {
        ResourceMeta {
            etag: String::new(),
            generation: 1,
            created_at: String::new(),
            updated_at: String::new(),
            managed_by: managed_by.into(),
            uid: antirion_core::ResourceUid::random(),
            credential: None,
            created_by: None,
            disabled_reason: None,
            lifecycle_at: None,
            credentials_rotated_at: std::collections::BTreeMap::new(),
            rotated_at: None,
        }
    }

    fn state() -> ConfigState {
        let doc = json!({
            "version": 1,
            "providers": {"p": {"type": "openai"}},
            "deployments": {"d": {"provider": "p", "model": "x"}},
            "models": {"m": {"target": "d"}, "api_model": {"target": "d"}}
        });
        let mut meta = Metadata::default();
        meta.resources
            .entry("providers".into())
            .or_default()
            .insert("p".into(), meta_of("apply:import"));
        meta.resources
            .entry("deployments".into())
            .or_default()
            .insert("d".into(), meta_of("apply:import"));
        meta.resources
            .entry("models".into())
            .or_default()
            .insert("m".into(), meta_of("apply:import"));
        meta.resources
            .entry("models".into())
            .or_default()
            .insert("api_model".into(), meta_of("api"));
        ConfigState {
            generation: 1,
            doc,
            meta,
        }
    }

    fn meta_of(m: &str) -> ResourceMeta {
        meta(m)
    }

    #[test]
    fn changes_between_documents() {
        let old = json!({"models": {"keep": {}, "change": {"d": 1}, "remove": {}}});
        let new = json!({"models": {"keep": {}, "change": {"d": 2}, "create": {}}, "policies": {"x": 1}});
        let got: Vec<(String, Op)> = compute(&old, &new)
            .into_iter()
            .map(|(t, op, _, _)| (t.path(), op))
            .collect();
        assert_eq!(
            got,
            vec![
                ("models.change".to_owned(), Op::Update),
                ("models.create".to_owned(), Op::Create),
                ("models.remove".to_owned(), Op::Delete),
                ("policies".to_owned(), Op::Create),
            ]
        );
    }

    #[test]
    fn prune_removes_only_unreferenced_resources_of_the_source() {
        let s = state();
        let incoming = json!({"version": 1}).as_object().cloned().unwrap();
        let plan = merge_apply(&s, &incoming, "import", true, &json!({}));
        assert!(plan.candidate["models"].get("m").is_none());
        assert!(plan.candidate["models"].get("api_model").is_some());
        assert!(
            plan.candidate["deployments"].get("d").is_some(),
            "still referenced by an api model"
        );
        assert!(
            plan.candidate["providers"].get("p").is_some(),
            "still referenced through the kept deployment"
        );
        let other = merge_apply(&s, &incoming, "git", true, &json!({}));
        assert_eq!(
            other.candidate,
            json!({"version": 1, "providers": {"p": {"type": "openai"}},
            "deployments": {"d": {"provider": "p", "model": "x"}}, "models": {"m": {"target": "d"}, "api_model": {"target": "d"}}})
        );
    }

    #[test]
    fn an_apply_of_auth_keeps_the_ip_sets_and_groups_other_writers_still_name() {
        let mut current = ConfigState::empty();
        current.doc = json!({
            "auth": {"admin": {"bootstrap_token": "t"},
                     "ip_sets": {"office": ["203.0.113.0/24"], "lab": ["198.51.100.0/24"]},
                     "model_groups": {"fast": ["m"], "old": ["n"]}},
            "keys": {"k": {"allowed_ips": ["set:office"], "models": ["group:fast"]}}
        });
        let incoming = json!({"version": 1, "auth": {"admin": {"bootstrap_token": "t"}}})
            .as_object()
            .cloned()
            .unwrap();
        let plan = merge_apply(&current, &incoming, "file", true, &json!({}));
        assert_eq!(
            plan.candidate["auth"],
            json!({"admin": {"bootstrap_token": "t"},
                   "ip_sets": {"office": ["203.0.113.0/24"]},
                   "model_groups": {"fast": ["m"]}})
        );
        let replaced = json!({"auth": {"ip_sets": {"office": ["192.0.2.0/24"]}}})
            .as_object()
            .cloned()
            .unwrap();
        let plan = merge_apply(&current, &replaced, "file", true, &json!({}));
        assert_eq!(
            plan.candidate["auth"]["ip_sets"],
            json!({"office": ["192.0.2.0/24"]}),
            "the document's own value wins"
        );
    }

    #[test]
    fn a_setting_the_document_no_longer_has_keeps_nothing_alive() {
        let mut current = ConfigState::empty();
        current.doc = json!({"policies": {"guardrails": {"input": ["g"]}},
                             "guardrails": {"definitions": {"g": {"type": "regex"}}}});
        let mut m = Metadata::default();
        m.resources
            .entry("guardrails".to_owned())
            .or_default()
            .insert("g".to_owned(), meta("apply:s"));
        m.settings.insert("policies".to_owned(), meta("apply:s"));
        current.meta = m;
        let incoming = json!({"version": 1});
        let plan = merge_apply(&current, incoming.as_object().unwrap(), "s", true, &json!({}));
        assert!(
            plan.candidate.pointer("/guardrails/definitions/g").is_none(),
            "{}",
            plan.candidate
        );
    }

    #[test]
    fn collections_inside_a_setting_count_as_written() {
        let mut current = ConfigState::empty();
        current.doc = json!({"observability": {"alerts": {"rules": {"r": {"category": "latency"}}}}});
        let mut m = Metadata::default();
        m.resources
            .entry("alerts/rules".to_owned())
            .or_default()
            .insert("r".to_owned(), meta("apply:s"));
        current.meta = m;
        let incoming = json!({"observability": {"alerts": {"rules": {"r": {"category": "latency"}}}}});
        let plan = merge_apply(&current, incoming.as_object().unwrap(), "s", true, &json!({}));
        assert!(
            plan.written
                .contains(&Target::Resource("alerts/rules", "r".to_owned()))
        );
        assert!(
            plan.candidate.pointer("/observability/alerts/rules/r").is_some(),
            "{}",
            plan.candidate
        );
    }

    #[test]
    fn nested_collections_and_static_keys() {
        let s = ConfigState::empty();
        let incoming = json!({"version": 1, "guardrails": {"definitions": {"g": {"type": "regex"}}, "expose_results": true},
                              "server": {"listen": "x"}})
            .as_object()
            .cloned()
            .unwrap();
        let plan = merge_apply(&s, &incoming, "c", false, &json!({}));
        assert_eq!(plan.candidate["guardrails"]["definitions"]["g"]["type"], "regex");
        assert_eq!(plan.candidate["guardrails"]["expose_results"], true);
        assert_eq!(plan.static_ignored, vec!["server"]);
        assert!(plan.written.contains(&Target::Resource("guardrails", "g".into())));
    }
}
