//! Typed views of the parts of a validated document the runtime compiles. The schema has already
//! enforced every field's shape, so these views only name what they read and default the rest.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

fn yes() -> bool {
    true
}

fn one() -> u64 {
    1
}

/// Deserialises a view from a validated fragment.
pub fn view<T: for<'de> Deserialize<'de>>(v: &Value) -> Result<T, serde_json::Error> {
    T::deserialize(v)
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ProviderView {
    #[serde(rename = "type")]
    pub kind: String,
    pub base_url: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub auth: ProviderAuthView,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ProviderAuthView {
    pub method: Option<String>,
    pub api_key: Option<String>,
    #[serde(default)]
    pub api_keys: Vec<ApiKeyView>,
    pub header: Option<String>,
    pub prefix: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ApiKeyView {
    pub name: String,
    pub key: String,
    #[serde(default = "one")]
    pub weight: u64,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct DeploymentView {
    pub provider: String,
    pub model: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub limits: BTreeMap<String, LimitView>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ModelView {
    pub route: Option<String>,
    pub target: Option<Value>,
    pub visibility: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub owned_by: Option<String>,
}

/// A limit declaration.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct LimitView {
    pub unit: String,
    pub limit: u64,
    pub per: Option<String>,
    pub burst: Option<u64>,
    #[serde(default)]
    pub partition_by: Vec<String>,
    pub on_missing: Option<String>,
    #[serde(default)]
    pub overrides: BTreeMap<String, u64>,
    #[serde(rename = "match")]
    pub matching: Option<MatchView>,
    pub exclude: Option<MatchView>,
    #[serde(default = "yes")]
    pub enforce: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct MatchView {
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub endpoints: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// The limit declarations of a `policies.limits` block, without its `locked` and
/// `request_overridable` lists.
#[must_use]
pub fn limit_declarations(policies: Option<&Value>) -> BTreeMap<String, LimitView> {
    let mut out = BTreeMap::new();
    let Some(map) = policies.and_then(|p| p.get("limits")).and_then(Value::as_object) else {
        return out;
    };
    for (name, v) in map {
        if name == "locked" || name == "request_overridable" {
            continue;
        }
        if let Ok(l) = view::<LimitView>(v) {
            out.insert(name.clone(), l);
        }
    }
    out
}

/// The access block and identity fields shared by organizations, teams, projects and keys.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct TenantView {
    pub organization: Option<String>,
    pub team: Option<String>,
    pub project: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub models: Option<Vec<String>>,
    pub deny_models: Option<Vec<String>>,
    pub providers: Option<Vec<String>>,
    pub deny_providers: Option<Vec<String>>,
    pub deployments: Option<Vec<String>>,
    pub endpoints: Option<Vec<String>>,
    pub allowed_ips: Option<Vec<String>>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub kind: Option<String>,
    pub secret: Option<String>,
    pub expires_at: Option<String>,
    pub owner: Option<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
    pub allowed_request_tags: Option<Vec<String>>,
    /// Keys only: `required` refuses requests that name no end user.
    pub end_user: Option<String>,
    /// Keys only: the priority class of limits with priority shares.
    pub priority_class: Option<String>,
    /// Organizations only: whether password sign-in is allowed to users bound in it.
    pub local_login: Option<bool>,
    pub regions: Option<Vec<String>>,
    pub deny_regions: Option<Vec<String>>,
    /// Organizations, teams and projects: the longest lifetime a key below may have.
    pub max_key_lifetime: Option<String>,
    pub key_policy: Option<KeyPolicyView>,
    /// Keys only: the client certificate the key is bound to.
    pub require_client_cert: Option<Value>,
}

/// `key_policy` of an organization, team or project: each field set at the narrowest level wins.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct KeyPolicyView {
    pub default_lifetime: Option<String>,
    pub rotation_max_age: Option<String>,
    pub reminder_days: Option<Vec<u32>>,
    pub default_grace_period: Option<String>,
}

/// The data-plane authentication settings and the admin session settings (`auth`).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct AuthView {
    pub mode: Option<String>,
    pub store_outage_grace: Option<String>,
    #[serde(default)]
    pub model_groups: BTreeMap<String, Vec<String>>,
    /// Named CIDR lists that `allowed_ips` entries reference as `set:<name>`.
    #[serde(default)]
    pub ip_sets: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub admin: AuthAdminView,
    /// A trusted proxy may pass the client certificate it verified.
    #[serde(default)]
    pub forwarded_client_cert: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct AuthAdminView {
    pub public_origin: Option<String>,
    #[serde(default)]
    pub session: SessionView,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SessionView {
    pub idle_timeout: Option<String>,
    pub absolute_timeout: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct UserView {
    pub kind: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub role_bindings: Vec<RoleBindingView>,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub title: Option<String>,
    pub primary_team: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct RoleBindingView {
    pub role: String,
    pub scope: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SecurityView {
    #[serde(default)]
    pub cors: CorsView,
    #[serde(default)]
    pub limits: SecurityLimitsView,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct CorsView {
    pub data: Option<CorsPlaneView>,
    pub admin: Option<CorsPlaneView>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct CorsPlaneView {
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    pub allowed_methods: Option<Vec<String>>,
    pub allow_headers: Option<Value>,
    pub expose_headers: Option<Vec<String>>,
    pub max_age: Option<String>,
    #[serde(default)]
    pub allow_credentials: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SecurityLimitsView {
    pub max_body: Option<String>,
    pub max_upload_body: Option<String>,
    pub max_upstream_response: Option<String>,
    pub max_json_depth: Option<usize>,
    pub max_messages: Option<usize>,
    pub max_uri_length: Option<usize>,
}

/// The static settings a node reads once at startup.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct StaticView {
    #[serde(default)]
    pub server: ServerView,
    #[serde(default)]
    pub admin: AdminView,
    #[serde(default)]
    pub store: StoreView,
    #[serde(default)]
    pub cluster: ClusterView,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ServerView {
    pub listen: Option<String>,
    #[serde(default)]
    pub admission: AdmissionView,
    #[serde(default)]
    pub shutdown: ShutdownView,
    #[serde(default)]
    pub probes: ProbesView,
    #[serde(default)]
    pub upstream: UpstreamView,
    #[serde(default)]
    pub http: ServerHttpView,
}

/// `server.http`: the listeners' timeouts.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ServerHttpView {
    pub header_read_timeout: Option<String>,
    pub body_read_timeout: Option<String>,
    pub idle_timeout: Option<String>,
}

/// `server.admission`, the one dynamic part of `server`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct AdmissionView {
    pub max_in_flight: Option<u64>,
    pub max_queue: Option<u64>,
    pub queue_timeout: Option<String>,
    pub fairness: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ShutdownView {
    pub drain_min: Option<String>,
    pub drain_max: Option<String>,
    pub flush_timeout: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ProbesView {
    pub liveness_stall: Option<String>,
    #[serde(default)]
    pub readiness_requires: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct UpstreamView {
    pub http2: Option<String>,
    pub max_connections_per_origin: Option<usize>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct AdminView {
    pub listen: Option<String>,
    #[serde(default)]
    pub config: AdminConfigView,
    #[serde(default)]
    pub api: AdminApiView,
    #[serde(default)]
    pub audit: AdminAuditView,
    #[serde(default)]
    pub ui: AdminUiView,
}

/// `admin.ui`: where the console is served.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct AdminUiView {
    pub enabled: Option<bool>,
    pub path: Option<String>,
    pub data_plane_url: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct AdminConfigView {
    pub file: Option<String>,
    pub source: Option<String>,
    pub prune: Option<bool>,
    pub watch: Option<bool>,
    pub watch_debounce: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct AdminApiView {
    pub default_page_size: Option<usize>,
    pub max_page_size: Option<usize>,
    pub max_document: Option<String>,
    pub max_wait: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct AdminAuditView {
    pub include_diffs: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct StoreView {
    pub backend: Option<String>,
    #[serde(default)]
    pub embedded: EmbeddedStoreView,
    #[serde(default)]
    pub postgres: PostgresView,
    #[serde(default)]
    pub ledger: LedgerView,
    #[serde(default)]
    pub spool: SpoolView,
}

/// `store.postgres`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PostgresView {
    pub url: Option<String>,
    pub pool_size: Option<usize>,
    pub connect_timeout: Option<String>,
    pub statement_timeout: Option<String>,
    pub migrate: Option<String>,
    pub migrate_timeout: Option<String>,
}

/// `store.ledger`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct LedgerView {
    pub flush_interval: Option<String>,
}

/// `store.spool`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct SpoolView {
    pub path: Option<String>,
    pub max_size: Option<String>,
    pub on_full: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct EmbeddedStoreView {
    pub path: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ClusterView {
    pub mode: Option<String>,
    pub node_id: Option<String>,
    pub heartbeat_interval: Option<String>,
    pub node_ttl: Option<String>,
    pub probe_interval: Option<String>,
    #[serde(default)]
    pub config_sync: ConfigSyncView,
    #[serde(default)]
    pub counters: CountersView,
}

/// `cluster.config_sync`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ConfigSyncView {
    pub poll_interval: Option<String>,
}

/// `cluster.counters`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct CountersView {
    pub backend: Option<String>,
    #[serde(default)]
    pub redis: RedisView,
}

/// `cluster.counters.redis`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct RedisView {
    pub url: Option<String>,
    pub command_timeout: Option<String>,
    pub key_prefix: Option<String>,
    pub topology: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn limit_declarations_skip_the_field_lists() {
        let p = json!({"limits": {"rpm": {"unit": "requests", "limit": 2, "per": "1m"}, "locked": ["rpm"]}});
        let d = limit_declarations(Some(&p));
        assert_eq!(d.len(), 1);
        assert_eq!(d["rpm"].limit, 2);
        assert!(d["rpm"].enforce);
    }

    #[test]
    fn defaults_apply_to_absent_fields() {
        let m: ModelView = view(&json!({"target": "d"})).unwrap();
        assert!(m.enabled);
        let p: ProviderView = view(&json!({"type": "openai"})).unwrap();
        assert!(p.enabled && p.headers.is_empty());
    }
}
