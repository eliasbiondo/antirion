//! Which permission each operation needs, and the scope chain of a resource.

use antirion_config::model::DEFAULT_TENANT;
use antirion_identity::rbac::Scope;
use antirion_runtime::Snapshot;
use serde_json::Value;

/// The kind of operation on a resource.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Verb {
    Read,
    Create,
    Update,
    Delete,
}

/// The tenancy-scoped collections, whose resources outside a caller's scope are reported as absent.
pub(crate) fn is_tenancy(collection: &str) -> bool {
    matches!(collection, "organizations" | "teams" | "projects" | "keys")
}

/// The permission an operation on a collection needs.
pub(crate) fn permission(collection: &str, verb: Verb) -> String {
    if collection.starts_with("cache/") {
        return "cache:admin".to_owned();
    }
    let resource = match collection {
        "routes" | "models" => "models",
        "guardrails" => "policies",
        c if c.starts_with("mcp/") || c.starts_with("a2a/") => "mcp",
        c if c.starts_with("alerts/") => "alerts",
        c => c,
    };
    let crud = matches!(
        resource,
        "organizations" | "teams" | "projects" | "keys" | "users"
    );
    let action = match (verb, crud) {
        (Verb::Read, _) => "read",
        (Verb::Create, true) => "create",
        (Verb::Update, true) => "update",
        (Verb::Delete, true) => "delete",
        (_, false) => "write",
    };
    format!("{resource}:{action}")
}

fn parent(spec: Option<&Value>, field: &str) -> String {
    spec.and_then(|s| s.get(field))
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_TENANT)
        .to_owned()
}

/// The scopes containing a resource, broadest first; `spec` is its stored or proposed spec.
pub(crate) fn chain(snap: &Snapshot, collection: &str, name: &str, spec: Option<&Value>) -> Vec<Scope> {
    let id = &snap.identity;
    match collection {
        "organizations" => vec![Scope::Gateway, Scope::Organization(name.to_owned())],
        "teams" => vec![
            Scope::Gateway,
            Scope::Organization(parent(spec, "organization")),
            Scope::Team(name.to_owned()),
        ],
        "projects" => {
            let mut c = id.team_scopes(&parent(spec, "team"));
            c.push(Scope::Project(name.to_owned()));
            c
        }
        "keys" => id.project_scopes(&parent(spec, "project")),
        _ => vec![Scope::Gateway],
    }
}

/// The scope chain an update moves a resource to, when it changes the resource's parent (a
/// project's team, a team's organization); `None` when the resource stays where it is.
pub(crate) fn moved(
    snap: &Snapshot,
    collection: &str,
    name: &str,
    old: &Value,
    new: &Value,
) -> Option<Vec<Scope>> {
    let to = chain(snap, collection, name, Some(new));
    (chain(snap, collection, name, Some(old)) != to).then_some(to)
}

/// The owner of a resource, which decides what a developer may do with a key.
pub(crate) fn owner<'a>(collection: &str, spec: Option<&'a Value>) -> Option<&'a str> {
    (collection == "keys")
        .then(|| spec?.get("owner")?.as_str())
        .flatten()
}

/// The scopes containing a role binding's scope, broadest first.
pub(crate) fn scope_chain(snap: &Snapshot, scope: &Scope) -> Vec<Scope> {
    match scope {
        Scope::Gateway => vec![Scope::Gateway],
        Scope::Organization(o) => vec![Scope::Gateway, Scope::Organization(o.clone())],
        Scope::Team(t) => snap.identity.team_scopes(t),
        Scope::Project(p) => snap.identity.project_scopes(p),
    }
}

/// The permission of a singleton settings key, for reads (`write` false) or writes.
pub(crate) fn setting_permission(key: &str, write: bool) -> &'static str {
    match (key, write) {
        ("api" | "streaming" | "observability", false) => "config:read",
        ("api" | "streaming" | "observability", true) => "config:apply",
        ("routing", false) => "models:read",
        ("routing", true) => "models:write",
        ("auth" | "security" | "smtp" | "governance", false) => "security:read",
        ("auth" | "security" | "smtp" | "governance", true) => "security:write",
        ("pricing", false) => "pricing:read",
        ("pricing", true) => "pricing:write",
        ("catalog", false) => "models:read",
        ("catalog", true) => "models:write",
        ("mcp" | "a2a", false) => "mcp:read",
        ("mcp" | "a2a", true) => "mcp:write",
        ("server" | "admin" | "store" | "cluster", _) => "config:read",
        (_, false) => "policies:read",
        (_, true) => "policies:write",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissions_follow_the_catalogue() {
        assert_eq!(permission("models", Verb::Create), "models:write");
        assert_eq!(permission("routes", Verb::Read), "models:read");
        assert_eq!(permission("keys", Verb::Update), "keys:update");
        assert_eq!(permission("guardrails", Verb::Delete), "policies:write");
        assert_eq!(permission("mcp/servers", Verb::Read), "mcp:read");
        assert_eq!(permission("alerts/rules", Verb::Create), "alerts:write");
        assert_eq!(permission("alerts/channels", Verb::Read), "alerts:read");
        assert_eq!(setting_permission("policies", true), "policies:write");
        assert_eq!(setting_permission("server", false), "config:read");
    }

    /// The permissions here match the contract's `x-antirion-permission` of every collection and
    /// settings operation.
    #[test]
    fn permissions_match_the_admin_api_contract() {
        let paths = crate::ops::openapi_json()["paths"].as_object().unwrap();
        let declared = |path: &str, method: &str| -> String {
            let p = paths[path][method]["x-antirion-permission"]
                .as_str()
                .unwrap_or_else(|| panic!("{method} {path}"));
            p.split(" (").next().unwrap().to_owned()
        };
        for c in antirion_config::model::COLLECTIONS {
            let list = format!("/{}", c.name);
            let prefix = format!("/{}/{{", c.name);
            let one = paths
                .keys()
                .find(|p| p.starts_with(&prefix) && p.ends_with('}') && !p.contains(':'))
                .cloned()
                .unwrap_or_else(|| panic!("no resource path under {list}"));
            assert_eq!(permission(c.name, Verb::Read), declared(&list, "get"), "{list}");
            assert_eq!(
                permission(c.name, Verb::Create),
                declared(&list, "post"),
                "{list}"
            );
            assert_eq!(permission(c.name, Verb::Read), declared(&one, "get"), "{one}");
            assert_eq!(permission(c.name, Verb::Update), declared(&one, "patch"), "{one}");
            assert_eq!(
                permission(c.name, Verb::Delete),
                declared(&one, "delete"),
                "{one}"
            );
        }
        for s in antirion_config::model::SINGLETONS {
            let path = format!("/settings/{}", s.name);
            assert_eq!(
                setting_permission(s.name, false),
                declared(&path, "get"),
                "{path}"
            );
            assert_eq!(
                setting_permission(s.name, true),
                declared(&path, "patch"),
                "{path}"
            );
        }
    }
}
