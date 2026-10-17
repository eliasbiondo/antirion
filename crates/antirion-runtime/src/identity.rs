//! The compiled identity part of a snapshot: keys by kid, the tenancy hierarchy with its access
//! lists, admin users with their role bindings, and the `auth` settings.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use antirion_config::model::DEFAULT_TENANT;
use antirion_config::scalar::{Cidr, parse_duration};
use antirion_config::views::{AuthView, TenantView, UserView, view};
use antirion_config::{ConfigError, DetailCode, DocPath};
use antirion_core::{ResourceUid, UtcMicros};
use antirion_identity::KeyPermissions;
use antirion_identity::access::ListPair;
use antirion_identity::rbac::{Binding, Scope};
use antirion_identity::{Credential, Pepper, Plane};
use serde_json::Value;

use crate::state::ConfigState;

/// The access block of one tenancy level.
#[derive(Clone, Debug, Default)]
pub struct Access {
    pub models: ListPair,
    pub providers: ListPair,
    pub deployments: ListPair,
    pub regions: ListPair,
    pub endpoints: Option<Vec<String>>,
    pub allowed_ips: Option<Vec<Cidr>>,
}

/// What an access block's lists are expanded with: model groups and named IP sets.
struct Expansions<'a> {
    groups: &'a BTreeMap<String, Vec<String>>,
    ip_sets: &'a BTreeMap<String, Vec<String>>,
}

/// Replaces each `group:<name>` pattern by the patterns of that model group.
fn expand_groups(list: Option<&Vec<String>>, groups: &BTreeMap<String, Vec<String>>) -> Option<Vec<String>> {
    list.map(|patterns| {
        patterns
            .iter()
            .flat_map(|p| match p.strip_prefix("group:") {
                Some(g) => groups.get(g).cloned().unwrap_or_default(),
                None => vec![p.clone()],
            })
            .collect()
    })
}

impl Access {
    fn of(t: &TenantView, x: &Expansions<'_>) -> Access {
        let groups = x.groups;
        Access {
            models: ListPair {
                allow: expand_groups(t.models.as_ref(), groups),
                deny: expand_groups(t.deny_models.as_ref(), groups),
            },
            providers: ListPair {
                allow: t.providers.clone(),
                deny: t.deny_providers.clone(),
            },
            deployments: ListPair {
                allow: t.deployments.clone(),
                deny: None,
            },
            regions: ListPair {
                allow: t.regions.clone(),
                deny: t.deny_regions.clone(),
            },
            endpoints: t.endpoints.clone(),
            allowed_ips: t.allowed_ips.as_ref().map(|l| {
                l.iter()
                    .flat_map(|entry| match entry.strip_prefix(antirion_config::IP_SET_PREFIX) {
                        Some(set) => x.ip_sets.get(set).cloned().unwrap_or_default(),
                        None => vec![entry.clone()],
                    })
                    .filter_map(|c| Cidr::parse(&c).ok())
                    .collect()
            }),
        }
    }
}

/// The `auth` settings a request or an admin session reads.
#[derive(Clone, Debug)]
pub struct AuthSettings {
    pub store_outage_grace: Duration,
    pub session_idle: Duration,
    pub session_absolute: Duration,
    /// The origin cookie-authenticated admin writes must come from; `None` means the admin
    /// listener's own origin as the client addressed it.
    pub public_origin: Option<String>,
    /// A trusted proxy may pass the client certificate it verified in `Client-Cert`.
    pub forwarded_client_cert: bool,
}

impl Default for AuthSettings {
    fn default() -> AuthSettings {
        AuthSettings {
            store_outage_grace: Duration::from_secs(600),
            session_idle: Duration::from_secs(3600),
            session_absolute: Duration::from_secs(12 * 3600),
            public_origin: None,
            forwarded_client_cert: false,
        }
    }
}

impl AuthSettings {
    fn of(v: &AuthView) -> AuthSettings {
        let d = AuthSettings::default();
        let dur =
            |s: &Option<String>, default: Duration| s.as_deref().and_then(parse_duration).unwrap_or(default);
        AuthSettings {
            store_outage_grace: dur(&v.store_outage_grace, d.store_outage_grace),
            session_idle: dur(&v.admin.session.idle_timeout, d.session_idle),
            session_absolute: dur(&v.admin.session.absolute_timeout, d.session_absolute),
            public_origin: v.admin.public_origin.clone(),
            forwarded_client_cert: v.forwarded_client_cert,
        }
    }
}

/// An organization, team or project.
#[derive(Clone, Debug, Default)]
pub struct TenantRt {
    pub enabled: bool,
    /// Organizations: whether users bound in it may sign in with a password.
    pub local_login: bool,
    pub parent: Option<String>,
    pub access: Access,
    pub tags: Vec<String>,
}

/// A virtual key that can authenticate.
#[derive(Clone, Debug)]
pub struct KeyRt {
    pub name: String,
    pub uid: ResourceUid,
    pub organization: String,
    pub team: String,
    pub project: String,
    pub enabled: bool,
    pub expires_at: Option<UtcMicros>,
    pub owner: Option<String>,
    pub access: Access,
    pub tags: Vec<String>,
    pub permissions: KeyPermissions,
    pub allowed_request_tags: Vec<String>,
    /// `end_user: required`: requests that name no end user are refused.
    pub end_user_required: bool,
    /// The priority class limits with priority shares count the key's requests in.
    pub priority_class: Option<String>,
    /// The client certificate requests with this key must present.
    pub client_cert: Option<antirion_identity::certificate::CertBinding>,
}

/// One secret that authenticates a key: its current one, or a rotated-out one until `valid_until`.
#[derive(Clone, Debug)]
pub struct KeySecret {
    pub key: Arc<KeyRt>,
    pub verifier: String,
    pub valid_until: Option<UtcMicros>,
}

/// An admin user or service account.
#[derive(Clone, Debug, Default)]
pub struct UserRt {
    pub enabled: bool,
    pub human: bool,
    pub bindings: Vec<Binding>,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub title: Option<String>,
    pub primary_team: Option<String>,
}

/// The identity part of a snapshot.
#[derive(Clone, Debug, Default)]
pub struct IdentitySnapshot {
    pub keys_by_kid: HashMap<String, KeySecret>,
    /// Imported legacy keys by the lowercase hex SHA-256 of their secret.
    pub imported: HashMap<String, KeySecret>,
    pub organizations: HashMap<String, TenantRt>,
    pub teams: HashMap<String, TenantRt>,
    pub projects: HashMap<String, TenantRt>,
    pub users: HashMap<String, UserRt>,
    pub settings: AuthSettings,
    /// The console's operator-identity and mail settings.
    pub console: crate::operators::ConsoleSettings,
    /// Keys of `kind: jwt`, by name: what a mapped token authenticates as.
    pub jwt_keys: HashMap<String, Arc<KeyRt>>,
    /// The JWT providers of `auth.jwt`.
    pub jwt: Vec<Arc<crate::jwt::JwtProviderRt>>,
    /// The `anonymous` key every request is attributed to when `auth.mode` is `disabled`.
    pub anonymous: Option<Arc<KeyRt>>,
    /// The single sign-on providers of the admin plane.
    pub oidc: HashMap<String, crate::jwt::OidcProviderRt>,
    /// `auth.admin.scim`, its token resolved.
    pub scim: ScimRt,
    /// The GitHub sign-in providers of the admin plane.
    pub oauth: HashMap<String, crate::jwt::OauthProviderRt>,
    /// `auth.admin.sso.saml`, by name.
    pub saml: std::collections::BTreeMap<String, std::sync::Arc<antirion_identity::saml::Provider>>,
    /// The failed-authentication throttle, access rules and external authorization settings.
    pub access: crate::access::AccessSettings,
    /// The custom roles of `auth.admin.roles`, resolved beside the built-in ones.
    pub roles: antirion_identity::roles::Roles,
    /// `auth.anomaly_detection`.
    pub anomaly: crate::people::anomalies::AnomalySettings,
}

/// SCIM provisioning settings.
#[derive(Clone, Default)]
pub struct ScimRt {
    pub enabled: bool,
    /// The token configured by reference, resolved; `None` when the gateway generates it.
    pub token: Option<String>,
    /// `(group, role, scope)`.
    pub group_mappings: Vec<(String, String, String)>,
}

impl std::fmt::Debug for ScimRt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScimRt")
            .field("enabled", &self.enabled)
            .field("token", &self.token.as_ref().map(|_| "[REDACTED]"))
            .field("group_mappings", &self.group_mappings)
            .finish()
    }
}

fn scim_of(resolved: &Value) -> ScimRt {
    let s = resolved.pointer("/auth/admin/scim");
    let get = |k: &str| s.and_then(|s| s.get(k));
    ScimRt {
        enabled: get("enabled").and_then(Value::as_bool).unwrap_or(false),
        token: get("token")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .map(str::to_owned),
        group_mappings: get("group_mappings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|m| {
                let f = |k: &str| m.get(k).and_then(Value::as_str).map(str::to_owned);
                Some((f("group")?, f("role")?, f("scope")?))
            })
            .collect(),
    }
}

impl IdentitySnapshot {
    /// The organization, team and project records of a key, broadest first.
    #[must_use]
    pub fn chain(&self, key: &KeyRt) -> [Option<&TenantRt>; 3] {
        [
            self.organizations.get(&key.organization),
            self.teams.get(&key.team),
            self.projects.get(&key.project),
        ]
    }

    /// The access blocks that apply to a key: its organization, team and project, then its own.
    #[must_use]
    pub fn access_levels<'a>(&'a self, key: &'a KeyRt) -> Vec<&'a Access> {
        self.chain(key)
            .into_iter()
            .flatten()
            .map(|t| &t.access)
            .chain(std::iter::once(&key.access))
            .collect()
    }

    /// The admin scopes containing a key (or a project), broadest first.
    #[must_use]
    pub fn project_scopes(&self, project: &str) -> Vec<Scope> {
        let team = self
            .projects
            .get(project)
            .and_then(|p| p.parent.clone())
            .unwrap_or_else(|| DEFAULT_TENANT.to_owned());
        let org = self
            .teams
            .get(&team)
            .and_then(|t| t.parent.clone())
            .unwrap_or_else(|| DEFAULT_TENANT.to_owned());
        vec![
            Scope::Gateway,
            Scope::Organization(org),
            Scope::Team(team),
            Scope::Project(project.to_owned()),
        ]
    }

    #[must_use]
    pub fn team_scopes(&self, team: &str) -> Vec<Scope> {
        let org = self
            .teams
            .get(team)
            .and_then(|t| t.parent.clone())
            .unwrap_or_else(|| DEFAULT_TENANT.to_owned());
        vec![
            Scope::Gateway,
            Scope::Organization(org),
            Scope::Team(team.to_owned()),
        ]
    }
}

fn default_tenant(parent: Option<&str>) -> TenantRt {
    TenantRt {
        enabled: true,
        local_login: true,
        parent: parent.map(str::to_owned),
        ..TenantRt::default()
    }
}

/// A key as the data plane sees it: its tenancy chain, access block and permissions.
fn key_rt(id: &IdentitySnapshot, name: &str, t: &TenantView, uid: ResourceUid, x: &Expansions<'_>) -> KeyRt {
    let project = t.project.clone().unwrap_or_else(|| DEFAULT_TENANT.to_owned());
    let team = id
        .projects
        .get(&project)
        .and_then(|p| p.parent.clone())
        .unwrap_or_else(|| DEFAULT_TENANT.to_owned());
    let organization = id
        .teams
        .get(&team)
        .and_then(|p| p.parent.clone())
        .unwrap_or_else(|| DEFAULT_TENANT.to_owned());
    KeyRt {
        name: name.to_owned(),
        uid,
        organization,
        team,
        project,
        enabled: t.enabled,
        expires_at: t
            .expires_at
            .as_deref()
            .and_then(|s| UtcMicros::parse_rfc3339(s).ok()),
        owner: t.owner.clone(),
        access: Access::of(t, x),
        tags: t.tags.clone(),
        permissions: KeyPermissions::from_names(t.permissions.iter().map(String::as_str)),
        allowed_request_tags: t
            .allowed_request_tags
            .clone()
            .unwrap_or_else(|| vec!["*".to_owned()]),
        end_user_required: t.end_user.as_deref() == Some("required"),
        priority_class: t.priority_class.clone(),
        client_cert: t
            .require_client_cert
            .as_ref()
            .and_then(antirion_identity::certificate::CertBinding::parse),
    }
}

/// Compiles the identity part from the stored state and its resolved document.
pub fn compile(
    state: &ConfigState,
    resolved: &Value,
    pepper: &Pepper,
) -> Result<IdentitySnapshot, Vec<ConfigError>> {
    let mut errors = Vec::new();
    let mut id = IdentitySnapshot::default();
    let auth: AuthView = resolved
        .get("auth")
        .map(view)
        .and_then(Result::ok)
        .unwrap_or_default();
    id.settings = AuthSettings::of(&auth);
    id.console = crate::operators::ConsoleSettings::of(resolved);
    let groups = &auth.model_groups;
    let x = Expansions {
        groups,
        ip_sets: &auth.ip_sets,
    };
    id.access = crate::access::AccessSettings::of(resolved, groups);
    id.roles = antirion_identity::roles::Roles::compile(resolved);
    id.anomaly = crate::people::anomalies::AnomalySettings::of(resolved);
    let tenants = |key: &str| resolved.get(key).and_then(Value::as_object).into_iter().flatten();
    for (name, spec) in tenants("organizations") {
        let t: TenantView = view(spec).unwrap_or_default();
        id.organizations.insert(
            name.clone(),
            TenantRt {
                enabled: t.enabled,
                local_login: t.local_login.unwrap_or(true),
                parent: None,
                access: Access::of(&t, &x),
                tags: t.tags,
            },
        );
    }
    for (name, spec) in tenants("teams") {
        let t: TenantView = view(spec).unwrap_or_default();
        let parent = t.organization.clone();
        id.teams.insert(
            name.clone(),
            TenantRt {
                enabled: t.enabled,
                local_login: true,
                parent,
                access: Access::of(&t, &x),
                tags: t.tags,
            },
        );
    }
    for (name, spec) in tenants("projects") {
        let t: TenantView = view(spec).unwrap_or_default();
        let parent = t.team.clone();
        id.projects.insert(
            name.clone(),
            TenantRt {
                enabled: t.enabled,
                local_login: true,
                parent,
                access: Access::of(&t, &x),
                tags: t.tags,
            },
        );
    }
    id.organizations
        .entry(DEFAULT_TENANT.to_owned())
        .or_insert_with(|| default_tenant(None));
    id.teams
        .entry(DEFAULT_TENANT.to_owned())
        .or_insert_with(|| default_tenant(Some(DEFAULT_TENANT)));
    id.projects
        .entry(DEFAULT_TENANT.to_owned())
        .or_insert_with(|| default_tenant(Some(DEFAULT_TENANT)));

    for (name, spec) in tenants("users") {
        let u: UserView = view(spec).unwrap_or_default();
        let bindings = u
            .role_bindings
            .iter()
            .filter_map(|b| {
                Some(Binding {
                    role: id.roles.resolve(&b.role)?,
                    scope: Scope::parse(&b.scope)?,
                })
            })
            .collect();
        id.users.insert(
            name.clone(),
            UserRt {
                enabled: u.enabled,
                human: u.kind.as_deref() != Some("service"),
                bindings,
                email: u.email,
                display_name: u.display_name,
                title: u.title,
                primary_team: u.primary_team,
            },
        );
    }

    let key_meta = state.meta.resources.get("keys");
    for (name, spec) in tenants("keys") {
        let at = DocPath::root().key("keys").key(name);
        let t: TenantView = view(spec).unwrap_or_default();
        let meta = key_meta.and_then(|m| m.get(name));
        if t.kind.as_deref() == Some("jwt") {
            let key = key_rt(&id, name, &t, meta.map_or(ResourceUid::GATEWAY, |m| m.uid), &x);
            id.jwt_keys.insert(name.clone(), Arc::new(key));
            continue;
        }
        let stored = meta.and_then(|m| m.credential.as_ref());
        let (kid, verifier) = match (&t.secret, stored) {
            (Some(secret), _) => match Credential::parse(secret) {
                Some(c) if c.plane() == Plane::Data => (c.kid().to_owned(), pepper.verifier(c.expose())),
                _ => {
                    errors.push(ConfigError::new(
                        &at.key("secret"),
                        DetailCode::InvalidFormat,
                        "the secret is not a well-formed virtual key",
                    ));
                    continue;
                }
            },
            (None, Some(cred)) => (cred.kid.clone(), cred.verifier.clone()),
            (None, None) => continue,
        };
        let key = Arc::new(key_rt(
            &id,
            name,
            &t,
            meta.map_or(ResourceUid::GATEWAY, |m| m.uid),
            &x,
        ));
        if let Some(prev) = stored.and_then(|c| c.previous.as_ref()) {
            let secret = KeySecret {
                key: Arc::clone(&key),
                verifier: prev.verifier.clone(),
                valid_until: UtcMicros::parse_rfc3339(&prev.valid_until).ok(),
            };
            match prev.imported_digest() {
                Some(digest) => id.imported.insert(digest.to_owned(), secret),
                None => id.keys_by_kid.insert(prev.kid.clone(), secret),
            };
        }
        let secret = KeySecret {
            key,
            verifier,
            valid_until: None,
        };
        match stored
            .and_then(|c| c.imported_digest())
            .filter(|_| t.secret.is_none())
        {
            Some(digest) => id.imported.insert(digest.to_owned(), secret),
            None => id.keys_by_kid.insert(kid, secret),
        };
    }
    let disabled = auth.mode.as_deref() == Some("disabled");
    if disabled {
        let t = TenantView {
            enabled: true,
            ..TenantView::default()
        };
        id.anonymous = Some(Arc::new(key_rt(&id, "anonymous", &t, ResourceUid::GATEWAY, &x)));
    }
    id.oidc = crate::jwt::compile_oidc(resolved);
    id.scim = scim_of(resolved);
    id.oauth = crate::jwt::compile_oauth(resolved);
    id.saml = antirion_identity::saml::compile(resolved);
    match crate::jwt::compile(resolved, &id.jwt_keys) {
        Ok(p) => id.jwt = p,
        Err(e) => errors.extend(e),
    }
    if disabled && (!id.keys_by_kid.is_empty() || !id.imported.is_empty() || !id.jwt.is_empty()) {
        errors.push(ConfigError::new(
            &DocPath::root().key("auth").key("mode"),
            DetailCode::Conflict,
            "authentication cannot be disabled while keys with secrets or JWT providers are configured",
        ));
    }
    if errors.is_empty() { Ok(id) } else { Err(errors) }
}
