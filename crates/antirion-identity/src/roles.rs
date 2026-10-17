//! Custom roles (`auth.admin.roles`): their compiled form, name resolution beside the built-in roles,
//! and the configuration rules the schema cannot express.

use std::collections::HashMap;
use std::sync::Arc;

use antirion_config::{ConfigError, DetailCode, DocPath};
use serde_json::Value;

use crate::rbac::{CATALOGUE, CUSTOM_BINDABLE_AT, CustomRole, Role};

/// The custom roles of one configuration.
#[derive(Clone, Debug, Default)]
pub struct Roles {
    custom: HashMap<String, Arc<CustomRole>>,
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

impl Roles {
    /// Compiles `auth.admin.roles` of a validated document.
    #[must_use]
    pub fn compile(doc: &Value) -> Roles {
        let custom = doc
            .pointer("/auth/admin/roles")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(name, _)| Role::parse(name).is_none())
            .map(|(name, spec)| {
                let mut permissions = strings(spec.get("permissions"));
                permissions.sort_unstable();
                permissions.dedup();
                let mut bindable_at = strings(spec.get("bindable_at"));
                if bindable_at.is_empty() {
                    bindable_at = CUSTOM_BINDABLE_AT.iter().map(|s| (*s).to_owned()).collect();
                }
                let role = CustomRole {
                    name: name.clone(),
                    permissions,
                    bindable_at,
                    description: spec.get("description").and_then(Value::as_str).map(str::to_owned),
                };
                (name.clone(), Arc::new(role))
            })
            .collect();
        Roles { custom }
    }

    /// The built-in or custom role named `name`.
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<Role> {
        Role::parse(name).or_else(|| self.custom.get(name).map(|c| Role::Custom(Arc::clone(c))))
    }

    /// The built-in roles, then the custom ones by name.
    #[must_use]
    pub fn all(&self) -> Vec<Role> {
        let mut custom: Vec<&Arc<CustomRole>> = self.custom.values().collect();
        custom.sort_by(|a, b| a.name.cmp(&b.name));
        Role::ALL
            .into_iter()
            .chain(custom.into_iter().map(|c| Role::Custom(Arc::clone(c))))
            .collect()
    }
}

fn bindings(v: Option<&Value>) -> impl Iterator<Item = (usize, &str)> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(i, b)| Some((i, b.get("role")?.as_str()?)))
}

/// Custom roles may not reuse a built-in name and hold only catalogue permissions; every role a
/// binding or a mapping names must exist.
#[must_use]
pub fn check_document(doc: &Value) -> Vec<ConfigError> {
    let mut errors = Vec::new();
    let admin = DocPath::root().key("auth").key("admin");
    let defined = doc.pointer("/auth/admin/roles").and_then(Value::as_object);
    for (name, spec) in defined.into_iter().flatten() {
        let at = admin.key("roles").key(name);
        if Role::parse(name).is_some() {
            errors.push(ConfigError::new(
                &at,
                DetailCode::Conflict,
                format!("{name:?} is a built-in role"),
            ));
        }
        for (i, p) in strings(spec.get("permissions")).iter().enumerate() {
            if !CATALOGUE.contains(&p.as_str()) {
                errors.push(ConfigError::new(
                    &at.key("permissions").index(i),
                    DetailCode::InvalidEnum,
                    format!("{p:?} is not an admin permission"),
                ));
            }
        }
    }
    let exists = |role: &str| Role::parse(role).is_some() || defined.is_some_and(|d| d.contains_key(role));
    let mut unresolved = |at: DocPath, role: &str| {
        if !exists(role) {
            errors.push(ConfigError::new(
                &at,
                DetailCode::UnresolvedReference,
                format!("role {role:?} does not exist"),
            ));
        }
    };
    for (name, user) in doc.get("users").and_then(Value::as_object).into_iter().flatten() {
        for (i, role) in bindings(user.get("role_bindings")) {
            let at = DocPath::root()
                .key("users")
                .key(name)
                .key("role_bindings")
                .index(i);
            unresolved(at.key("role"), role);
        }
    }
    for kind in ["oidc", "saml"] {
        for (name, p) in doc
            .pointer(&format!("/auth/admin/sso/{kind}"))
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            let at = admin.key("sso").key(kind).key(name);
            for list in ["role_mappings", "default_role_bindings"] {
                for (i, role) in bindings(p.get(list)) {
                    unresolved(at.key(list).index(i).key("role"), role);
                }
            }
        }
    }
    for (name, p) in doc
        .pointer("/auth/admin/sso/oauth")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        for (i, role) in bindings(p.get("role_mappings")) {
            let at = admin.key("sso").key("oauth").key(name).key("role_mappings");
            unresolved(at.index(i).key("role"), role);
        }
    }
    for (i, role) in bindings(doc.pointer("/auth/admin/scim/group_mappings")) {
        unresolved(admin.key("scim").key("group_mappings").index(i).key("role"), role);
    }
    errors
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn custom_roles_resolve_beside_the_built_in_ones() {
        let doc = json!({"auth": {"admin": {"roles": {
            "finance": {"permissions": ["analytics:read"], "bindable_at": ["organization"], "description": "Spend"},
            "wide": {"permissions": ["audit:read"]}
        }}}});
        let roles = Roles::compile(&doc);
        let finance = roles.resolve("finance").unwrap();
        assert!(finance.allows("analytics:read") && !finance.allows("logs:read"));
        assert_eq!(finance.bindable_at(), vec!["organization"]);
        assert_eq!(finance.description(), Some("Spend"));
        assert_eq!(
            roles.resolve("wide").unwrap().bindable_at(),
            CUSTOM_BINDABLE_AT.to_vec()
        );
        assert_eq!(roles.resolve("viewer"), Some(Role::Viewer));
        assert!(roles.resolve("ghost").is_none());
        let names: Vec<String> = roles.all().iter().map(|r| r.as_str().to_owned()).collect();
        assert_eq!(names.len(), 9);
        assert_eq!(names[7..], ["finance".to_owned(), "wide".to_owned()]);
    }

    #[test]
    fn definitions_and_references_are_checked() {
        let doc = json!({
            "auth": {"admin": {
                "roles": {"viewer": {"permissions": ["keys:read"]}, "x": {"permissions": ["keys:fly"]}},
                "scim": {"group_mappings": [{"group": "g", "role": "nobody", "scope": "gateway"}]}
            }},
            "users": {"u": {"role_bindings": [{"role": "x", "scope": "gateway"}, {"role": "ghost", "scope": "gateway"}]}}
        });
        let got: Vec<(String, DetailCode)> = check_document(&doc)
            .into_iter()
            .map(|e| (e.path, e.code))
            .collect();
        assert_eq!(
            got,
            vec![
                ("auth.admin.roles.viewer".to_owned(), DetailCode::Conflict),
                (
                    "auth.admin.roles.x.permissions[0]".to_owned(),
                    DetailCode::InvalidEnum
                ),
                (
                    "users.u.role_bindings[1].role".to_owned(),
                    DetailCode::UnresolvedReference
                ),
                (
                    "auth.admin.scim.group_mappings[0].role".to_owned(),
                    DetailCode::UnresolvedReference
                ),
            ]
        );
    }
}
