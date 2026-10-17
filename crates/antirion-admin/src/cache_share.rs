//! Shared cache namespaces let organizations read and write each other's entries, so only a
//! principal bound at the gateway scope may set `scope: shared` or `shared_namespace` at any level.

use std::collections::HashMap;

use antirion_identity::rbac::{AdminPrincipal, Scope};
use serde_json::Value;

use crate::error::AdminError;

/// The sharing a policy block asks for: whether its cache scope is `shared`, and its namespace.
fn sharing(block: &Value) -> (bool, Option<&Value>) {
    let cache = block.get("cache");
    let shared = cache.and_then(|c| c.get("scope")).and_then(Value::as_str) == Some("shared");
    let namespace = cache
        .and_then(|c| c.get("shared_namespace"))
        .filter(|v| !v.is_null());
    (shared, namespace)
}

/// Refuses a change that sets or changes `scope: shared` or `shared_namespace` in any policy block
/// unless the principal holds a binding at the gateway scope.
pub(crate) fn check(principal: &AdminPrincipal, before: &Value, after: &Value) -> Result<(), AdminError> {
    if principal.bindings.iter().any(|b| b.scope == Scope::Gateway) {
        return Ok(());
    }
    let old: HashMap<String, (bool, Option<&Value>)> = antirion_config::semantic::policy_blocks(before)
        .into_iter()
        .map(|(at, block)| (at.to_string(), sharing(block)))
        .collect();
    for (at, block) in antirion_config::semantic::policy_blocks(after) {
        let now = sharing(block);
        let asks = now.0 || now.1.is_some();
        if asks && old.get(&at.to_string()) != Some(&now) {
            return Err(AdminError::forbidden(
                "a gateway-scope binding (shared cache namespaces)",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use antirion_core::audit::ActorKind;
    use antirion_identity::rbac::{Binding, Role};
    use serde_json::json;

    use super::*;

    fn principal(scope: Scope) -> AdminPrincipal {
        AdminPrincipal {
            kind: ActorKind::Token,
            id: "t".into(),
            user: None,
            bindings: vec![Binding {
                role: Role::OrgAdmin,
                scope,
            }],
            read_only: false,
        }
    }

    #[test]
    fn only_gateway_bindings_set_shared_namespaces() {
        let before = json!({"organizations": {"o": {}}});
        let shared = json!({"organizations": {"o": {"policies": {"cache": {"scope": "shared",
            "shared_namespace": "n"}}}}});
        let org = principal(Scope::Organization("o".into()));
        assert!(check(&org, &before, &shared).is_err());
        assert!(check(&principal(Scope::Gateway), &before, &shared).is_ok());
        assert!(
            check(&org, &shared, &shared).is_ok(),
            "an unchanged namespace may stay"
        );
        let other = json!({"organizations": {"o": {"policies": {"cache": {"enabled": true}}}}});
        assert!(check(&org, &before, &other).is_ok());
    }
}
