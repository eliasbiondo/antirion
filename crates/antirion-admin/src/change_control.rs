//! Change control: which API writes `governance.change_control` puts under review, a change set
//! staged on a document as the Admin API would apply it, and the two-person rule of
//! `governance.dual_control` for destructive actions.

use antirion_config::model;
use antirion_config::scalar::{parse_duration, parse_size};
use antirion_config::{ConfigError, DetailCode, DocPath};
use antirion_identity::rbac::Scope;
use antirion_runtime::changes::{self, Target};
use axum::response::Response;
use http::StatusCode;
use serde_json::{Map, Value, json};

use crate::call::Call;
use crate::commit::merge_patch;
use crate::error::AdminError;
use crate::perms::{Verb, chain, moved, permission, setting_permission};

/// The refusal of a write that needs a change request.
pub(crate) fn required() -> AdminError {
    AdminError::new(
        StatusCode::CONFLICT,
        "change_request_required",
        "This change is under change control: open a change request (POST /admin/v1/change_requests).",
    )
}

/// Whether `resource` (a `change_control.resources` entry) covers the target.
fn covers(resource: &str, t: &Target) -> bool {
    match t {
        Target::Resource(c, _) => *c == resource || c.starts_with(&format!("{resource}/")),
        Target::Setting(s) => resource == "settings" || *s == resource,
    }
}

/// A scalar read for `above`: a number, a duration in seconds or a size in bytes.
fn magnitude(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s
            .trim()
            .parse::<f64>()
            .ok()
            .or_else(|| parse_duration(s).map(|d| d.as_secs_f64()))
            .or_else(|| parse_size(s).map(|b| b as f64)),
        _ => None,
    }
}

fn at<'a>(doc: &'a Value, dotted: &str) -> Option<&'a Value> {
    dotted.split('.').try_fold(doc, |cur, k| cur.get(k))
}

/// Whether going from `before` to `after` touches what `before`'s change control governs.
#[must_use]
pub(crate) fn governed(before: &Value, after: &Value) -> bool {
    let Some(cc) = before.pointer("/governance/change_control") else {
        return false;
    };
    let resources: Vec<&str> = cc
        .get("resources")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let paths: Vec<&Value> = cc
        .get("protected_paths")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect();
    if resources.is_empty() && paths.is_empty() {
        return false;
    }
    let changed = changes::compute(before, after);
    if changed
        .iter()
        .any(|(t, ..)| resources.iter().any(|r| covers(r, t)))
    {
        return true;
    }
    paths.iter().any(|p| {
        let Some(path) = p.get("path").and_then(Value::as_str) else {
            return false;
        };
        let (old, new) = (at(before, path), at(after, path));
        if old == new {
            return false;
        }
        match p.get("above").and_then(magnitude) {
            None => true,
            Some(limit) => new.and_then(magnitude).is_some_and(|n| n > limit),
        }
    })
}

/// A change set staged on a document.
#[derive(Debug)]
pub(crate) struct Staged {
    pub(crate) candidate: Value,
    pub(crate) written: Vec<Target>,
    /// The write permission each change needs, with the scope chain it needs it on.
    pub(crate) permissions: Vec<(String, Vec<Scope>)>,
    /// The read permission each change's resource needs.
    pub(crate) reads: Vec<(String, Vec<Scope>)>,
}

fn bad(i: usize, message: &str) -> AdminError {
    AdminError::validation(vec![ConfigError::new(
        &DocPath::root().key("changes").index(i),
        DetailCode::InvalidFormat,
        message,
    )])
}

fn spec_of(i: usize, body: Option<&Value>) -> Result<Value, AdminError> {
    match body.and_then(|b| b.get("spec")) {
        Some(v @ Value::Object(_)) => Ok(v.clone()),
        None => Ok(Value::Object(Map::new())),
        Some(_) => Err(bad(i, "spec must be an object")),
    }
}

/// The Admin API path of a change, relative to `/admin/v1`, as segments.
fn segments(path: &str) -> Vec<String> {
    let rel = path.strip_prefix("/admin/v1").unwrap_or(path);
    let rel = rel.split('?').next().unwrap_or_default();
    rel.split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Applies `changes` (each `{method, path, body}`) to `doc` as the Admin API's resource and
/// settings writes would.
pub(crate) fn stage(call: &Call, doc: &Value, list: &[Value]) -> Result<Staged, AdminError> {
    let mut candidate = doc.clone();
    let mut written = Vec::new();
    let mut permissions = Vec::new();
    let mut reads = Vec::new();
    for (i, ch) in list.iter().enumerate() {
        let method = ch.get("method").and_then(Value::as_str).unwrap_or_default();
        let path = ch.get("path").and_then(Value::as_str).unwrap_or_default();
        if !path.starts_with('/') {
            return Err(bad(i, "path is an Admin API path starting with /"));
        }
        let segs = segments(path);
        let body = ch.get("body");
        if let [first, key] = segs.as_slice()
            && first == "settings"
        {
            let def = model::singleton(key).ok_or_else(|| bad(i, "not a settings object"))?;
            let new = match method {
                "PUT" => spec_of(i, body)?,
                "PATCH" => {
                    let mut current =
                        model::singleton_value(&candidate, def).unwrap_or_else(|| Value::Object(Map::new()));
                    merge_patch(&mut current, &spec_of(i, body)?);
                    current
                }
                _ => return Err(bad(i, "settings take PUT or PATCH")),
            };
            model::set_singleton(&mut candidate, def, Some(new));
            written.push(Target::Setting(def.name));
            permissions.push((setting_permission(key, true).to_owned(), vec![Scope::Gateway]));
            reads.push((setting_permission(key, false).to_owned(), vec![Scope::Gateway]));
            continue;
        }
        let (collection, name) = match segs.as_slice() {
            [c] => (c.clone(), None),
            [a, b] if model::collection(&format!("{a}/{b}")).is_some() => (format!("{a}/{b}"), None),
            [c, n] => (c.clone(), Some(n.clone())),
            [a, b, n] => (format!("{a}/{b}"), Some(n.clone())),
            _ => return Err(bad(i, "not a resource or settings path")),
        };
        let def = model::collection(&collection).ok_or_else(|| bad(i, "not a resource or settings path"))?;
        if matches!(def.name, "keys" | "users") {
            return Err(bad(
                i,
                "keys and users cannot be changed through a change request",
            ));
        }
        let name = match (method, name) {
            ("POST", None) => body
                .and_then(|b| b.get("name"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| bad(i, "a create names its resource"))?,
            ("PUT" | "PATCH" | "DELETE", Some(n)) => n,
            _ => return Err(bad(i, "the method does not apply to this path")),
        };
        let map = model::collection_map_mut(&mut candidate, def).ok_or_else(AdminError::internal)?;
        let exists = map.contains_key(&name);
        let before = map.get(&name).cloned();
        let verb = match method {
            "POST" if exists => {
                return Err(AdminError::conflict(
                    "already_exists",
                    format!("{collection}/{name} already exists."),
                    Vec::new(),
                ));
            }
            "POST" => {
                map.insert(name.clone(), spec_of(i, body)?);
                Verb::Create
            }
            "PUT" => {
                map.insert(name.clone(), spec_of(i, body)?);
                if exists { Verb::Update } else { Verb::Create }
            }
            "PATCH" if exists => {
                if let Some(cur) = map.get_mut(&name) {
                    merge_patch(cur, &spec_of(i, body)?);
                }
                Verb::Update
            }
            "DELETE" if exists => {
                map.remove(&name);
                Verb::Delete
            }
            _ => return Err(AdminError::not_found()),
        };
        let spec = model::collection_map(&candidate, def).and_then(|m| m.get(&name));
        let scopes = chain(&call.snap, def.name, &name, spec);
        match (verb, &before, spec) {
            (Verb::Update, Some(old), Some(new)) => {
                let from = chain(&call.snap, def.name, &name, Some(old));
                if let Some(to) = moved(&call.snap, def.name, &name, old, new) {
                    permissions.push((permission(def.name, Verb::Create), to));
                }
                permissions.push((permission(def.name, verb), from));
            }
            _ => permissions.push((permission(def.name, verb), scopes.clone())),
        }
        reads.push((permission(def.name, Verb::Read), scopes));
        written.push(Target::Resource(def.name, name));
    }
    Ok(Staged {
        candidate,
        written,
        permissions,
        reads,
    })
}

/// Whether the principal holds every permission of `needed`.
pub(crate) fn holds(call: &Call, needed: &[(String, Vec<Scope>)]) -> bool {
    needed.iter().all(|(p, scopes)| call.principal.allows(p, scopes))
}

/// The actions `governance.dual_control` lists.
fn dual(call: &Call, action: &str) -> bool {
    call.snap
        .config
        .doc
        .pointer("/governance/dual_control/actions")
        .and_then(Value::as_array)
        .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(action)))
}

/// Under the two-person rule, an unapproved call of `action` opens a change request of kind
/// `action` for the call and answers 202 with it, instead of acting.
pub(crate) async fn second_person(call: &Call, action: &str) -> Result<Option<Response>, AdminError> {
    if call.approved || !dual(call, action) {
        return Ok(None);
    }
    let body = if call.body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&call.body).unwrap_or(Value::Null)
    };
    let path = match call.uri_query() {
        Some(q) => format!("{}?{q}", call.path),
        None => call.path.clone(),
    };
    let change = json!({"method": call.method.as_str(), "path": path, "body": body});
    let created = crate::change_requests::open_action(call, action, change).await?;
    Ok(Some(call.json(StatusCode::ACCEPTED, &created)))
}

/// Whether a write of `governance` from `current` to `new` removes an audit sink.
#[must_use]
pub(crate) fn removes_audit_sink(current: &Value, new: &Value) -> bool {
    let names = |v: &Value| -> Vec<String> {
        v.get("audit_sinks")
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    };
    let kept = names(new);
    names(current).iter().any(|n| !kept.contains(n))
}

/// Runs an action a change request held back, as the applying principal, with the two-person
/// rule satisfied.
pub(crate) async fn run_approved(
    call: &Call,
    action: &str,
    change: &Value,
    note: &str,
) -> Result<Response, AdminError> {
    let method = change
        .get("method")
        .and_then(Value::as_str)
        .and_then(|m| m.parse::<http::Method>().ok())
        .ok_or_else(AdminError::internal)?;
    let full = change.get("path").and_then(Value::as_str).unwrap_or_default();
    let (path, query) = match full.split_once('?') {
        Some((p, q)) => (p.to_owned(), Some(q.to_owned())),
        None => (full.to_owned(), None),
    };
    let body = match change.get("body") {
        None | Some(Value::Null) => bytes::Bytes::new(),
        Some(b) => bytes::Bytes::from(b.to_string()),
    };
    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    let replay = Call {
        rt: std::sync::Arc::clone(&call.rt),
        snap: call.rt.gens.load(),
        principal: call.principal.clone(),
        session: None,
        request_id: call.request_id.clone(),
        source_ip: call.source_ip.clone(),
        method,
        query: crate::query_pairs(query.as_deref()),
        raw_query: query,
        path: path.clone(),
        headers,
        body,
        reason: Some(note.to_owned()),
        approved: true,
    };
    let rest = path.strip_prefix("/admin/v1/").unwrap_or(&path).to_owned();
    let segments: Vec<String> = rest.split('/').map(crate::percent_decode).collect();
    let segs: Vec<&str> = segments.iter().map(String::as_str).collect();
    match (action, segs.as_slice()) {
        ("cache_flush_all", _) => Box::pin(crate::cache::invalidate(&replay)).await,
        ("budget_reset", ["runtime", "budgets", level, owner, last]) => {
            Box::pin(crate::budgets::one(&replay, level, owner, last)).await
        }
        ("audit_sink_delete", _) => Box::pin(crate::settings::handle(&replay, "governance")).await,
        ("ingress_pause" | "organization_delete" | "secrets_rotate", _)
            if crate::danger_routes::routes(&segs) =>
        {
            // On a task of its own: the replay nests a whole handler under the change request's, deeper
            // than a worker's stack holds when the action commits a generation.
            let owned = segments.clone();
            call.rt
                .tasks
                .spawn(async move {
                    let segs: Vec<&str> = owned.iter().map(String::as_str).collect();
                    crate::danger_routes::dispatch(&replay, &segs).await
                })
                .await
                .map_err(|_| AdminError::internal())?
        }
        _ => Err(AdminError::not_found()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collections_settings_and_protected_paths_are_governed() {
        let rules = json!({"governance": {"change_control": {
            "resources": ["routes"],
            "protected_paths": [{"path": "observability.request_logs.sinks.store.retention", "above": "90d"}]
        }}});
        let mut route = rules.clone();
        route["routes"] = json!({"r": {"tree": "d"}});
        assert!(governed(&rules, &route));
        let mut model = rules.clone();
        model["models"] = json!({"m": {"target": "d"}});
        assert!(!governed(&rules, &model));
        let mut short = rules.clone();
        short["observability"] = json!({"request_logs": {"sinks": {"store": {"retention": "30d"}}}});
        assert!(!governed(&rules, &short));
        let mut long = rules.clone();
        long["observability"] = json!({"request_logs": {"sinks": {"store": {"retention": "120d"}}}});
        assert!(governed(&rules, &long));
        assert!(!governed(&json!({}), &route));
        assert!(covers("settings", &Target::Setting("routing")));
        assert!(covers("mcp", &Target::Resource("mcp/servers", "s".into())));
    }

    #[test]
    fn removed_sinks_are_seen() {
        let cur = json!({"audit_sinks": {"a": {}, "b": {}}});
        assert!(removes_audit_sink(&cur, &json!({"audit_sinks": {"a": {}}})));
        assert!(!removes_audit_sink(
            &cur,
            &json!({"audit_sinks": {"a": {}, "b": {}, "c": {}}})
        ));
    }
}
