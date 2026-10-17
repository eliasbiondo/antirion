//! Admin-plane authorization: built-in and custom roles, scoped role bindings and
//! `<resource>:<action>` permissions.

use std::fmt;
use std::sync::Arc;

use antirion_core::audit::ActorKind;

/// Where a role binding applies; a binding applies to everything below its scope.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    Gateway,
    Organization(String),
    Team(String),
    Project(String),
}

impl Scope {
    #[must_use]
    pub fn parse(s: &str) -> Option<Scope> {
        if s == "gateway" {
            return Some(Scope::Gateway);
        }
        let (kind, name) = s.split_once('/')?;
        let name = name.to_owned();
        match kind {
            "organization" => Some(Scope::Organization(name)),
            "team" => Some(Scope::Team(name)),
            "project" => Some(Scope::Project(name)),
            _ => None,
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scope::Gateway => f.write_str("gateway"),
            Scope::Organization(n) => write!(f, "organization/{n}"),
            Scope::Team(n) => write!(f, "team/{n}"),
            Scope::Project(n) => write!(f, "project/{n}"),
        }
    }
}

/// Every admin permission (`<resource>:<action>`).
pub const CATALOGUE: &[&str] = &[
    "organizations:read",
    "organizations:create",
    "organizations:update",
    "organizations:delete",
    "teams:read",
    "teams:create",
    "teams:update",
    "teams:delete",
    "projects:read",
    "projects:create",
    "projects:update",
    "projects:delete",
    "users:read",
    "users:create",
    "users:update",
    "users:delete",
    "keys:read",
    "keys:create",
    "keys:update",
    "keys:delete",
    "keys:rotate",
    "keys:grant_permissions",
    "role_bindings:read",
    "role_bindings:create",
    "role_bindings:delete",
    "providers:read",
    "providers:write",
    "deployments:read",
    "deployments:write",
    "models:read",
    "models:write",
    "pricing:read",
    "pricing:write",
    "plugins:read",
    "plugins:write",
    "mcp:read",
    "mcp:write",
    "security:read",
    "security:write",
    "policies:read",
    "policies:write",
    "budgets:adjust",
    "health:read",
    "health:write",
    "cluster:read",
    "node:drain",
    "config:read",
    "config:apply",
    "logs:read",
    "logs:read_content",
    "logs:delete",
    "analytics:read",
    "audit:read",
    "metrics:read",
    "cache:admin",
    "playground:send",
    "alerts:read",
    "alerts:write",
    "notifications:send",
    "safety:review",
];

/// A role an operator defined in `auth.admin.roles`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CustomRole {
    pub name: String,
    /// Catalogue permissions, sorted.
    pub permissions: Vec<String>,
    /// Scope kinds the role can be bound at.
    pub bindable_at: Vec<String>,
    pub description: Option<String>,
}

/// Scope kinds a custom role can be bound at when it names none.
pub const CUSTOM_BINDABLE_AT: [&str; 3] = ["organization", "team", "project"];

/// A built-in role, or a custom one.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    GatewayAdmin,
    GatewayViewer,
    SecurityAuditor,
    OrgAdmin,
    TeamAdmin,
    Developer,
    Viewer,
    Custom(Arc<CustomRole>),
}

impl Role {
    /// The built-in roles.
    pub const ALL: [Role; 7] = [
        Role::GatewayAdmin,
        Role::GatewayViewer,
        Role::SecurityAuditor,
        Role::OrgAdmin,
        Role::TeamAdmin,
        Role::Developer,
        Role::Viewer,
    ];

    /// A built-in role by name.
    #[must_use]
    pub fn parse(s: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|r| r.as_str() == s)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Role::GatewayAdmin => "gateway_admin",
            Role::GatewayViewer => "gateway_viewer",
            Role::SecurityAuditor => "security_auditor",
            Role::OrgAdmin => "org_admin",
            Role::TeamAdmin => "team_admin",
            Role::Developer => "developer",
            Role::Viewer => "viewer",
            Role::Custom(c) => &c.name,
        }
    }

    #[must_use]
    pub fn is_builtin(&self) -> bool {
        !matches!(self, Role::Custom(_))
    }

    /// What the role is for.
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        Some(match self {
            Role::GatewayAdmin => "Every permission, gateway-wide.",
            Role::GatewayViewer => "Reads everything except request and response content, gateway-wide.",
            Role::SecurityAuditor => "Reads the audit log, users, role bindings and keys.",
            Role::OrgAdmin => "Manages an organization's teams, projects, keys, users, policies and alerts.",
            Role::TeamAdmin => "Manages a team's projects, keys, role bindings and policies.",
            Role::Developer => "Manages the keys it owns and reads its projects' logs and analytics.",
            Role::Viewer => "Reads everything in its scope except request and response content.",
            Role::Custom(c) => return c.description.as_deref(),
        })
    }

    /// The scope kinds the role can be bound at.
    #[must_use]
    pub fn bindable_at(&self) -> Vec<&str> {
        match self {
            Role::GatewayAdmin | Role::GatewayViewer => vec!["gateway"],
            Role::SecurityAuditor => vec!["gateway", "organization"],
            Role::OrgAdmin => vec!["organization"],
            Role::TeamAdmin => vec!["team"],
            Role::Developer => vec!["team", "project"],
            Role::Viewer => vec!["organization", "team", "project"],
            Role::Custom(c) => c.bindable_at.iter().map(String::as_str).collect(),
        }
    }

    /// The order in which built-in roles may grant each other: a principal binds only roles of at
    /// most its own rank, and the gateway-wide roles only from a gateway administrator. Custom roles
    /// have no rank.
    const fn rank(&self) -> Option<u8> {
        Some(match self {
            Role::GatewayAdmin => 6,
            Role::GatewayViewer => 5,
            Role::OrgAdmin | Role::SecurityAuditor => 4,
            Role::TeamAdmin => 3,
            Role::Developer => 2,
            Role::Viewer => 1,
            Role::Custom(_) => return None,
        })
    }

    /// Whether the role grants `permission` (`<resource>:<action>`) within its scope. The developer's
    /// key permissions hold only for keys it owns (see [`AdminPrincipal::allows_owned`]).
    #[must_use]
    pub fn allows(&self, permission: &str) -> bool {
        let (resource, action) = permission.split_once(':').unwrap_or((permission, ""));
        let read_any = action == "read" && permission != "logs:read_content";
        match self {
            Role::GatewayAdmin => true,
            Role::GatewayViewer | Role::Viewer => read_any,
            Role::SecurityAuditor => {
                matches!(
                    permission,
                    "audit:read" | "role_bindings:read" | "users:read" | "keys:read"
                )
            }
            Role::OrgAdmin => {
                matches!(
                    resource,
                    "teams" | "projects" | "keys" | "users" | "role_bindings" | "policies"
                ) || matches!(
                    permission,
                    "organizations:read"
                        | "budgets:adjust"
                        | "logs:read"
                        | "logs:delete"
                        | "analytics:read"
                        | "audit:read"
                        | "playground:send"
                        | "alerts:read"
                        | "alerts:write"
                        | "notifications:send"
                        | "safety:review"
                )
            }
            Role::TeamAdmin => {
                (matches!(resource, "projects" | "role_bindings" | "policies"))
                    || (resource == "keys" && action != "grant_permissions")
                    || matches!(
                        permission,
                        "teams:read" | "logs:read" | "analytics:read" | "alerts:read" | "notifications:send"
                    )
            }
            Role::Developer => {
                (resource == "keys" && matches!(action, "read" | "create" | "update" | "delete" | "rotate"))
                    || matches!(permission, "projects:read" | "logs:read" | "analytics:read")
            }
            Role::Custom(c) => c.permissions.iter().any(|p| p == permission),
        }
    }

    /// Every catalogue permission the role grants.
    #[must_use]
    pub fn permissions(&self) -> Vec<&'static str> {
        CATALOGUE.iter().copied().filter(|p| self.allows(p)).collect()
    }
}

/// A role at a scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub role: Role,
    pub scope: Scope,
}

/// An authenticated admin principal.
#[derive(Clone, Debug)]
pub struct AdminPrincipal {
    pub kind: ActorKind,
    /// The audit actor id: a token kid, or `bootstrap`.
    pub id: String,
    /// The user or service account the credential belongs to, when there is one.
    pub user: Option<String>,
    pub bindings: Vec<Binding>,
    /// A read-only personal token: only the `read` permissions of the bindings hold.
    pub read_only: bool,
}

/// Whether a permission only reads (`read`, `read_content`).
fn reads(permission: &str) -> bool {
    permission
        .split_once(':')
        .is_some_and(|(_, action)| action.starts_with("read"))
}

impl AdminPrincipal {
    /// Who stands behind the credential: its user or service account, else the credential itself
    /// (the bootstrap token). Every credential of one user is the same person, so a rule that
    /// needs a second person compares this, never [`id`](Self::id).
    #[must_use]
    pub fn person(&self) -> &str {
        self.user.as_deref().unwrap_or(&self.id)
    }

    /// Whether any binding on a scope of `chain` (the resource's scope and every scope above it)
    /// grants `permission`. Permissions a role holds only over what the principal owns are not
    /// granted here.
    #[must_use]
    pub fn allows(&self, permission: &str, chain: &[Scope]) -> bool {
        self.allows_owned(permission, chain, None)
    }

    /// As [`allows`](Self::allows), for a key owned by `owner`.
    #[must_use]
    pub fn allows_owned(&self, permission: &str, chain: &[Scope], owner: Option<&str>) -> bool {
        let owned = owner.is_some() && owner == self.user.as_deref();
        if self.read_only && !reads(permission) {
            return false;
        }
        self.bindings.iter().any(|b| {
            chain.contains(&b.scope)
                && b.role.allows(permission)
                && (b.role != Role::Developer || !permission.starts_with("keys:") || owned)
        })
    }

    /// Whether the resource is inside any of the principal's scopes at all.
    #[must_use]
    pub fn sees(&self, chain: &[Scope]) -> bool {
        self.bindings.iter().any(|b| chain.contains(&b.scope))
    }

    #[must_use]
    pub fn is_gateway_admin(&self) -> bool {
        !self.read_only
            && self
                .bindings
                .iter()
                .any(|b| b.role == Role::GatewayAdmin && b.scope == Scope::Gateway)
    }

    /// Whether the principal may bind `role` on the scope whose chain is `chain`: it needs
    /// `role_bindings:create` there, and either a built-in binding whose role ranks at least as
    /// high as a built-in `role`, or every permission `role` grants in that scope.
    #[must_use]
    pub fn may_grant(&self, role: &Role, chain: &[Scope]) -> bool {
        if self.read_only {
            return false;
        }
        if self.is_gateway_admin() {
            return true;
        }
        if role.bindable_at() == ["gateway"] {
            return false;
        }
        let creators: Vec<&Binding> = self
            .bindings
            .iter()
            .filter(|b| chain.contains(&b.scope) && b.role.allows("role_bindings:create"))
            .collect();
        if creators.is_empty() {
            return false;
        }
        let outranks = |b: &&Binding| matches!((b.role.rank(), role.rank()), (Some(mine), Some(theirs)) if mine >= theirs);
        creators.iter().any(outranks) || role.permissions().iter().all(|p| self.allows(p, chain))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal(role: Role, scope: Scope) -> AdminPrincipal {
        AdminPrincipal {
            kind: ActorKind::Token,
            id: "t".into(),
            user: None,
            bindings: vec![Binding { role, scope }],
            read_only: false,
        }
    }

    #[test]
    fn a_read_only_principal_keeps_only_reads() {
        let mut p = principal(Role::GatewayAdmin, Scope::Gateway);
        p.read_only = true;
        assert!(p.allows("keys:read", &[Scope::Gateway]));
        assert!(p.allows("logs:read_content", &[Scope::Gateway]));
        assert!(!p.allows("keys:update", &[Scope::Gateway]));
        assert!(!p.is_gateway_admin());
        assert!(!p.may_grant(&Role::Viewer, &[Scope::Gateway]));
    }

    #[test]
    fn every_credential_of_a_user_is_the_same_person() {
        let mut a = principal(Role::GatewayAdmin, Scope::Gateway);
        a.user = Some("sec-admin".into());
        let mut b = a.clone();
        b.id = "other-kid".into();
        assert_eq!(a.person(), "sec-admin");
        assert_eq!(a.person(), b.person());
        let bootstrap = principal(Role::GatewayAdmin, Scope::Gateway);
        assert_eq!(bootstrap.person(), "t");
    }

    #[test]
    fn scopes_round_trip() {
        for s in ["gateway", "organization/acme", "team/t", "project/p"] {
            assert_eq!(Scope::parse(s).unwrap().to_string(), s);
        }
        assert!(Scope::parse("cluster/x").is_none());
    }

    #[test]
    fn role_catalogue() {
        assert!(Role::GatewayViewer.allows("config:read"));
        assert!(!Role::GatewayViewer.allows("logs:read_content"));
        assert!(!Role::SecurityAuditor.allows("config:read"));
        assert!(Role::SecurityAuditor.allows("audit:read"));
        assert!(!Role::Developer.allows("audit:read"));
        assert!(!Role::Developer.allows("health:read"));
        assert!(!Role::TeamAdmin.allows("keys:grant_permissions"));
        assert!(Role::OrgAdmin.allows("keys:grant_permissions"));
        for r in Role::ALL {
            assert_eq!(Role::parse(r.as_str()), Some(r.clone()));
            assert!(r.permissions().iter().all(|p| CATALOGUE.contains(p)));
        }
        assert_eq!(Role::GatewayAdmin.permissions().len(), CATALOGUE.len());
    }

    #[test]
    fn custom_roles_grant_their_permissions_and_need_them_to_be_granted() {
        let finance = Role::Custom(Arc::new(CustomRole {
            name: "finance".into(),
            permissions: vec!["analytics:read".into()],
            bindable_at: vec!["organization".into()],
            description: None,
        }));
        let org = [Scope::Gateway, Scope::Organization("o".into())];
        let holder = principal(finance.clone(), Scope::Organization("o".into()));
        assert!(holder.allows("analytics:read", &org));
        assert!(!holder.allows("logs:read", &org));
        assert_eq!(finance.permissions(), vec!["analytics:read"]);
        assert!(principal(Role::OrgAdmin, Scope::Organization("o".into())).may_grant(&finance, &org));
        let auditors = Role::Custom(Arc::new(CustomRole {
            name: "auditors".into(),
            permissions: vec!["audit:read".into(), "role_bindings:create".into()],
            bindable_at: vec!["organization".into()],
            description: None,
        }));
        let granter = principal(auditors, Scope::Organization("o".into()));
        assert!(!granter.may_grant(&finance, &org));
        assert!(!granter.may_grant(&Role::Viewer, &org));
    }

    #[test]
    fn developers_act_only_on_keys_they_own() {
        let mut dev = principal(Role::Developer, Scope::Team("t".into()));
        dev.user = Some("alice".into());
        let chain = [Scope::Gateway, Scope::Team("t".into())];
        assert!(dev.allows_owned("keys:update", &chain, Some("alice")));
        assert!(!dev.allows_owned("keys:update", &chain, Some("bob")));
        assert!(!dev.allows("keys:update", &chain));
        assert!(dev.allows("projects:read", &chain));
    }

    #[test]
    fn grants_never_exceed_the_granter() {
        let team_admin = principal(Role::TeamAdmin, Scope::Team("t".into()));
        let team = [
            Scope::Gateway,
            Scope::Organization("o".into()),
            Scope::Team("t".into()),
        ];
        assert!(team_admin.may_grant(&Role::Developer, &team));
        assert!(team_admin.may_grant(&Role::TeamAdmin, &team));
        assert!(!team_admin.may_grant(&Role::OrgAdmin, &team));
        assert!(!team_admin.may_grant(&Role::Developer, &[Scope::Gateway, Scope::Team("u".into())]));
        assert!(!principal(Role::Viewer, Scope::Team("t".into())).may_grant(&Role::Viewer, &team));
        assert!(
            principal(Role::GatewayAdmin, Scope::Gateway).may_grant(&Role::GatewayViewer, &[Scope::Gateway])
        );
    }

    #[test]
    fn bindings_apply_below_their_scope_only() {
        let viewer = principal(Role::Viewer, Scope::Project("acme_project".into()));
        let inside = [
            Scope::Gateway,
            Scope::Organization("acme".into()),
            Scope::Team("t".into()),
            Scope::Project("acme_project".into()),
        ];
        let outside = [
            Scope::Gateway,
            Scope::Organization("beta".into()),
            Scope::Team("b".into()),
            Scope::Project("beta_project".into()),
        ];
        assert!(viewer.allows("keys:read", &inside));
        assert!(!viewer.allows("keys:update", &inside));
        assert!(viewer.sees(&inside) && !viewer.sees(&outside));
        assert!(!viewer.allows("providers:write", &[Scope::Gateway]));
    }
}
