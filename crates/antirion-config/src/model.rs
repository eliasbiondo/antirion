//! The shape of the config document: which top-level keys are collections of named resources, which
//! are singleton settings, and which are static node settings.

use serde_json::{Map, Value};

/// The tenancy resources that always exist under this name.
pub const DEFAULT_TENANT: &str = "default";

/// A collection of named resources, as the Admin API exposes it.
#[derive(Debug)]
pub struct CollectionDef {
    /// The Admin API path under `/admin/v1` (`providers`, `mcp/servers`).
    pub name: &'static str,
    /// Where the map lives in the document.
    pub doc_path: &'static [&'static str],
}

impl CollectionDef {
    /// The document path of a resource, as a dotted prefix (`providers.p`).
    #[must_use]
    pub fn resource_path(&self, name: &str) -> crate::DocPath {
        let mut p = crate::DocPath::root();
        for seg in self.doc_path {
            p.push_key(seg);
        }
        p.push_key(name);
        p
    }

    /// The prefix of change paths for this collection (`providers`, `mcp.servers`).
    #[must_use]
    pub fn dotted(&self) -> String {
        self.doc_path.join(".")
    }
}

pub const COLLECTIONS: &[CollectionDef] = &[
    CollectionDef {
        name: "providers",
        doc_path: &["providers"],
    },
    CollectionDef {
        name: "deployments",
        doc_path: &["deployments"],
    },
    CollectionDef {
        name: "models",
        doc_path: &["models"],
    },
    CollectionDef {
        name: "routes",
        doc_path: &["routes"],
    },
    CollectionDef {
        name: "organizations",
        doc_path: &["organizations"],
    },
    CollectionDef {
        name: "teams",
        doc_path: &["teams"],
    },
    CollectionDef {
        name: "projects",
        doc_path: &["projects"],
    },
    CollectionDef {
        name: "keys",
        doc_path: &["keys"],
    },
    CollectionDef {
        name: "users",
        doc_path: &["users"],
    },
    CollectionDef {
        name: "plugins",
        doc_path: &["plugins"],
    },
    CollectionDef {
        name: "guardrails",
        doc_path: &["guardrails", "definitions"],
    },
    CollectionDef {
        name: "mcp/servers",
        doc_path: &["mcp", "servers"],
    },
    CollectionDef {
        name: "mcp/gateways",
        doc_path: &["mcp", "gateways"],
    },
    CollectionDef {
        name: "a2a/agents",
        doc_path: &["a2a", "agents"],
    },
    CollectionDef {
        name: "alerts/rules",
        doc_path: &["observability", "alerts", "rules"],
    },
    CollectionDef {
        name: "alerts/channels",
        doc_path: &["observability", "alerts", "channels"],
    },
    CollectionDef {
        name: "cache/invalidation_rules",
        doc_path: &["cache", "invalidation_rules"],
    },
];

/// A singleton settings object; `nested` names the collections that live inside it.
#[derive(Debug)]
pub struct SingletonDef {
    pub name: &'static str,
    pub nested: &'static [&'static str],
}

pub const SINGLETONS: &[SingletonDef] = &[
    SingletonDef {
        name: "api",
        nested: &[],
    },
    SingletonDef {
        name: "streaming",
        nested: &[],
    },
    SingletonDef {
        name: "routing",
        nested: &[],
    },
    SingletonDef {
        name: "resilience",
        nested: &[],
    },
    SingletonDef {
        name: "auth",
        nested: &[],
    },
    SingletonDef {
        name: "limits",
        nested: &[],
    },
    SingletonDef {
        name: "budgets",
        nested: &[],
    },
    SingletonDef {
        name: "pricing",
        nested: &[],
    },
    SingletonDef {
        name: "cache",
        nested: &["invalidation_rules"],
    },
    SingletonDef {
        name: "guardrails",
        nested: &["definitions"],
    },
    SingletonDef {
        name: "observability",
        nested: &[],
    },
    SingletonDef {
        name: "mcp",
        nested: &["servers", "gateways"],
    },
    SingletonDef {
        name: "a2a",
        nested: &["agents"],
    },
    SingletonDef {
        name: "security",
        nested: &[],
    },
    SingletonDef {
        name: "policies",
        nested: &[],
    },
    SingletonDef {
        name: "smtp",
        nested: &[],
    },
    SingletonDef {
        name: "governance",
        nested: &[],
    },
    SingletonDef {
        name: "catalog",
        nested: &[],
    },
];

/// Node bootstrap settings, read once at startup and never stored or applied.
pub const STATIC_KEYS: &[&str] = &["server", "admin", "store", "cluster"];

/// The collection with this top-level name.
#[must_use]
pub fn collection(name: &str) -> Option<&'static CollectionDef> {
    COLLECTIONS.iter().find(|c| c.name == name)
}

/// The singleton with this top-level name.
#[must_use]
pub fn singleton(name: &str) -> Option<&'static SingletonDef> {
    SINGLETONS.iter().find(|s| s.name == name)
}

/// The map of a collection in `doc`, if present.
#[must_use]
pub fn collection_map<'a>(doc: &'a Value, c: &CollectionDef) -> Option<&'a Map<String, Value>> {
    let mut cur = doc;
    for seg in c.doc_path {
        cur = cur.get(seg)?;
    }
    cur.as_object()
}

/// The map of a collection in `doc`, created (with its parents) when missing.
pub fn collection_map_mut<'a>(doc: &'a mut Value, c: &CollectionDef) -> Option<&'a mut Map<String, Value>> {
    let mut cur = doc;
    for seg in c.doc_path {
        let obj = cur.as_object_mut()?;
        cur = obj
            .entry((*seg).to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
    }
    cur.as_object_mut()
}

/// A singleton's value without its nested collections; `None` when the key is absent.
#[must_use]
pub fn singleton_value(doc: &Value, s: &SingletonDef) -> Option<Value> {
    let mut v = doc.get(s.name)?.clone();
    if let Some(obj) = v.as_object_mut() {
        for nested in s.nested {
            obj.remove(*nested);
        }
    }
    Some(v)
}

/// Replaces a singleton's settings, keeping its nested collections; `None` removes the settings.
pub fn set_singleton(doc: &mut Value, s: &SingletonDef, value: Option<Value>) {
    let Some(root) = doc.as_object_mut() else { return };
    let mut kept = Map::new();
    if let Some(Value::Object(old)) = root.get(s.name) {
        for nested in s.nested {
            if let Some(v) = old.get(*nested) {
                kept.insert((*nested).to_owned(), v.clone());
            }
        }
    }
    let mut next = match value {
        Some(Value::Object(m)) => m,
        Some(_) | None => Map::new(),
    };
    for (k, v) in kept {
        next.insert(k, v);
    }
    if next.is_empty() && !root.contains_key(s.name) {
        return;
    }
    root.insert(s.name.to_owned(), Value::Object(next));
}

/// Drops empty collection maps and empty singletons, so documents compare by content.
pub fn normalise(doc: &mut Value) {
    let Some(root) = doc.as_object_mut() else { return };
    for c in COLLECTIONS {
        match c.doc_path {
            [top] => {
                if root
                    .get(*top)
                    .and_then(Value::as_object)
                    .is_some_and(Map::is_empty)
                {
                    root.remove(*top);
                }
            }
            [top, nested] => {
                if let Some(Value::Object(parent)) = root.get_mut(*top)
                    && parent
                        .get(*nested)
                        .and_then(Value::as_object)
                        .is_some_and(Map::is_empty)
                {
                    parent.remove(*nested);
                }
            }
            [top, middle, leaf] => {
                if let Some(Value::Object(parent)) = root.get_mut(*top)
                    && let Some(Value::Object(mid)) = parent.get_mut(*middle)
                {
                    if mid
                        .get(*leaf)
                        .and_then(Value::as_object)
                        .is_some_and(Map::is_empty)
                    {
                        mid.remove(*leaf);
                    }
                    if mid.is_empty() {
                        parent.remove(*middle);
                    }
                }
            }
            _ => {}
        }
    }
    for s in SINGLETONS {
        if root
            .get(s.name)
            .and_then(Value::as_object)
            .is_some_and(Map::is_empty)
        {
            root.remove(s.name);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn singletons_keep_their_nested_collections() {
        let mut doc = json!({"mcp": {"servers": {"s": {}}, "session": {"x": 1}}});
        let s = singleton("mcp").unwrap();
        assert_eq!(singleton_value(&doc, s), Some(json!({"session": {"x": 1}})));
        set_singleton(&mut doc, s, Some(json!({"other": 2})));
        assert_eq!(doc, json!({"mcp": {"servers": {"s": {}}, "other": 2}}));
    }

    #[test]
    fn nested_collections_are_created_on_demand() {
        let mut doc = json!({});
        let c = collection("guardrails").unwrap();
        collection_map_mut(&mut doc, c)
            .unwrap()
            .insert("g".into(), json!({"type": "regex"}));
        assert_eq!(
            doc,
            json!({"guardrails": {"definitions": {"g": {"type": "regex"}}}})
        );
        assert_eq!(c.resource_path("g").to_string(), "guardrails.definitions.g");
    }

    #[test]
    fn normalise_drops_empty_maps() {
        let mut doc = json!({"version": 1, "models": {}, "policies": {}, "mcp": {"servers": {}}});
        normalise(&mut doc);
        assert_eq!(doc, json!({"version": 1}));
    }
}
