//! A walk over every route-tree node of a document: route trees and Model targets, with the
//! children, rule targets, defaults, cost-quality branches and mirror targets beneath them.

use serde_json::Value;

use crate::DocPath;

fn node(n: &Value, at: &DocPath, f: &mut dyn FnMut(&serde_json::Map<String, Value>, &DocPath)) {
    let Value::Object(m) = n else {
        return;
    };
    f(m, at);
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
                node(item, &item_at, f);
            } else if let Some(t) = item.get("target") {
                node(t, &item_at.key("target"), f);
            }
        }
    }
    for single in ["default", "strong", "weak"] {
        if let Some(child) = m.get(single) {
            node(child, &at.key(single), f);
        }
    }
    if let Some(t) = m.get("mirror").and_then(|x| x.get("target")) {
        node(t, &at.key("mirror").key("target"), f);
    }
}

/// Calls `f` with every node object of the document's route trees and Model targets, and its path.
pub(crate) fn each_node(doc: &Value, f: &mut dyn FnMut(&serde_json::Map<String, Value>, &DocPath)) {
    let root = DocPath::root();
    for (collection, member) in [("routes", "tree"), ("models", "target")] {
        for (name, r) in doc
            .get(collection)
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            if let Some(t) = r.get(member) {
                node(t, &root.key(collection).key(name).key(member), f);
            }
        }
    }
}
