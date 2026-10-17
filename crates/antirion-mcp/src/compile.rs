//! The compiled MCP and A2A part of a snapshot: servers, gateways, agents, grants per policy level and
//! the session keys, read from a validated, secret-resolved document.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use antirion_config::scalar::{parse_duration, parse_size};
use antirion_config::{ConfigError, DetailCode, DocPath};
use base64::Engine;
use http::{HeaderMap, HeaderName, HeaderValue, Uri};
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// The gateway's own build version, the default `serverInfo.version`.
const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_MAX_BODY: u64 = 8 << 20;

/// How an upstream MCP server is reached.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Transport {
    StreamableHttp,
    /// The 2024-11-05 HTTP+SSE transport.
    Sse,
    /// Child processes of this node, over stdin and stdout.
    Stdio,
    /// An HTTP API described by an OpenAPI document; the gateway answers as the MCP server.
    OpenApi,
    /// A transport this build does not know: such a server always fails.
    Unsupported,
}

/// How a `stdio` server's processes are started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StdioSpec {
    pub(crate) command: String,
    pub(crate) args: Vec<String>,
    pub(crate) env: Vec<(String, String)>,
    pub(crate) processes: usize,
}

/// Which protocol era a server is assumed to speak.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Protocol {
    Auto,
    Legacy,
    Modern,
}

/// Static include/exclude filters over upstream names or URIs.
#[derive(Clone, Debug)]
pub(crate) struct Filter {
    pub(crate) include: Vec<String>,
    pub(crate) exclude: Vec<String>,
}

impl Filter {
    pub(crate) fn admits(&self, name: &str) -> bool {
        crate::glob::any(&self.include, name) && !crate::glob::any(&self.exclude, name)
    }
}

/// One upstream MCP server.
#[derive(Debug)]
pub(crate) struct ServerRt {
    pub(crate) name: String,
    pub(crate) transport: Transport,
    pub(crate) enabled: bool,
    pub(crate) url: Uri,
    pub(crate) protocol: Protocol,
    pub(crate) discovery_ttl: Duration,
    pub(crate) prefix: String,
    pub(crate) headers: HeaderMap,
    /// Lowercase header names a client may send as `x-antirion-mcp-<server>-<header>`.
    pub(crate) client_headers: Vec<String>,
    pub(crate) identity_headers: Vec<(HeaderName, String)>,
    pub(crate) tools: Filter,
    pub(crate) prompts: Filter,
    pub(crate) resources: Filter,
    pub(crate) tool_cache_ttl: Duration,
    pub(crate) init_timeout: Duration,
    pub(crate) request_timeout: Duration,
    pub(crate) stream_idle_timeout: Duration,
    pub(crate) retries: u8,
    pub(crate) pin_tool_definitions: bool,
    /// Fingerprint of everything that makes an upstream session belong to this configuration: the
    /// transport, URL and credentials. A token whose entry carries another value is stale.
    pub(crate) generation: u64,
    /// OAuth credentials obtained at run time.
    pub(crate) auth: Option<Arc<crate::credentials::UpstreamAuth>>,
    pub(crate) stdio: Option<StdioSpec>,
    pub(crate) openapi: Option<Arc<crate::openapi::OpenApi>>,
}

/// What a gateway answers as `serverInfo`.
#[derive(Clone, Debug)]
pub(crate) enum ServerInfo {
    Gateway(Value),
    Upstream,
}

/// OAuth protected-resource settings of a gateway.
#[derive(Clone, Debug)]
pub(crate) struct OauthRt {
    pub(crate) resource: Option<String>,
    pub(crate) authorization_servers: Vec<String>,
    pub(crate) scopes_supported: Vec<String>,
    /// The audiences a JWT must name; `None` is every resource URL of the gateway.
    pub(crate) audiences: Option<Vec<String>>,
    /// Dynamic client registration is relayed to the authorization server.
    pub(crate) proxy_registration: bool,
}

/// One virtual MCP server.
#[derive(Debug)]
pub(crate) struct GatewayRt {
    pub(crate) name: String,
    /// Server names in configured order.
    pub(crate) servers: Vec<String>,
    pub(crate) enabled: bool,
    pub(crate) prefixed: bool,
    pub(crate) stateless: bool,
    pub(crate) fail_closed: bool,
    pub(crate) server_info: ServerInfo,
    pub(crate) client_cache_max_ttl: Duration,
    pub(crate) allowed_origins: Vec<String>,
    pub(crate) max_body: u64,
    pub(crate) oauth: Option<OauthRt>,
    /// Modern requests may reach legacy-only servers through sessions opened for them.
    pub(crate) modern_bridge: bool,
    /// `tools/list` answers only the tool-search tools.
    pub(crate) tool_search: bool,
    /// Some grant anywhere in the configuration restricts this gateway's servers or items, so a list
    /// result depends on who asks.
    pub(crate) restricted: bool,
}

/// One A2A agent.
#[derive(Debug)]
pub(crate) struct AgentRt {
    pub(crate) name: String,
    pub(crate) card_url: Uri,
    pub(crate) url: Option<Uri>,
    pub(crate) protocol: Option<String>,
    pub(crate) enabled: bool,
    pub(crate) public_card: bool,
    pub(crate) headers: HeaderMap,
    pub(crate) identity_headers: Vec<(HeaderName, String)>,
    pub(crate) card_cache_ttl: Duration,
    pub(crate) request_timeout: Duration,
    pub(crate) stream_idle_timeout: Duration,
    pub(crate) extra_methods: Vec<String>,
    pub(crate) generation: u64,
}

/// `policies.mcp` grants of one level.
#[derive(Clone, Debug, Default)]
pub(crate) struct McpGrant {
    pub(crate) servers: Option<Vec<String>>,
    pub(crate) tools: BTreeMap<String, Vec<String>>,
    pub(crate) prompts: BTreeMap<String, Vec<String>>,
    pub(crate) resources: BTreeMap<String, Vec<String>>,
    /// Conditional grants: when set, the level also grants only what its matching rules grant.
    pub(crate) when: Option<Arc<Vec<crate::when::Rule>>>,
}

/// The MCP and A2A grants of one policy level.
#[derive(Clone, Debug, Default)]
pub(crate) struct Level {
    pub(crate) mcp: Option<McpGrant>,
    pub(crate) agents: Option<Vec<String>>,
}

/// Grants at every level of the hierarchy, by resource name.
#[derive(Clone, Debug, Default)]
pub(crate) struct Grants {
    pub(crate) gateway: Level,
    pub(crate) organizations: HashMap<String, Level>,
    pub(crate) teams: HashMap<String, Level>,
    pub(crate) projects: HashMap<String, Level>,
    pub(crate) keys: HashMap<String, Level>,
}

/// A session key: its id byte and 32 bytes of key material.
#[derive(Clone)]
pub(crate) struct SessionKey {
    pub(crate) id: u8,
    pub(crate) secret: [u8; 32],
}

impl std::fmt::Debug for SessionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionKey")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// The MCP and A2A part of one generation.
#[derive(Debug, Default)]
pub struct McpSnapshot {
    pub(crate) servers: BTreeMap<String, Arc<ServerRt>>,
    pub(crate) gateways: BTreeMap<String, Arc<GatewayRt>>,
    pub(crate) agents: BTreeMap<String, Arc<AgentRt>>,
    pub(crate) grants: Grants,
    /// Configured session keys; the first encrypts. Empty: the node's generated key is used.
    pub(crate) session_keys: Vec<SessionKey>,
    pub(crate) max_lifetime: Duration,
}

impl McpSnapshot {
    pub(crate) fn gateway(&self, name: &str) -> Option<&Arc<GatewayRt>> {
        self.gateways.get(name).filter(|g| g.enabled)
    }

    pub(crate) fn server(&self, name: &str) -> Option<&Arc<ServerRt>> {
        self.servers.get(name).filter(|s| s.enabled)
    }

    /// Whether any MCP or A2A resource is configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.gateways.is_empty() && self.agents.is_empty()
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FilterView {
    include: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ServerView {
    transport: Option<String>,
    enabled: Option<bool>,
    url: Option<String>,
    protocol: Option<String>,
    discovery_ttl: Option<String>,
    prefix: Option<String>,
    headers: BTreeMap<String, String>,
    auth: Option<Value>,
    client_headers: Vec<String>,
    identity_headers: BTreeMap<String, String>,
    tools: FilterView,
    prompts: FilterView,
    resources: FilterView,
    tool_cache_ttl: Option<String>,
    init_timeout: Option<String>,
    request_timeout: Option<String>,
    stream_idle_timeout: Option<String>,
    retries: Option<u8>,
    pin_tool_definitions: Option<bool>,
    command: Option<String>,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    processes: Option<usize>,
    openapi: Option<OpenApiView>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct OpenApiView {
    spec: Option<String>,
    operations: FilterView,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct OauthView {
    resource: Option<String>,
    authorization_servers: Vec<String>,
    scopes_supported: Vec<String>,
    audiences: Option<Vec<String>>,
    proxy_registration: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GatewayView {
    servers: Vec<String>,
    enabled: Option<bool>,
    prefix_mode: Option<String>,
    session_mode: Option<String>,
    on_server_failure: Option<String>,
    server_info: Option<Value>,
    client_cache_max_ttl: Option<String>,
    allowed_origins: Vec<String>,
    max_body: Option<String>,
    oauth: Option<OauthView>,
    modern_bridge: Option<bool>,
    tool_search: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AgentView {
    card_url: Option<String>,
    url: Option<String>,
    protocol: Option<String>,
    enabled: Option<bool>,
    public_card: Option<bool>,
    headers: BTreeMap<String, String>,
    identity_headers: BTreeMap<String, String>,
    card_cache_ttl: Option<String>,
    request_timeout: Option<String>,
    stream_idle_timeout: Option<String>,
    extra_methods: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct McpGrantView {
    servers: Option<Vec<String>>,
    tools: BTreeMap<String, Vec<String>>,
    prompts: BTreeMap<String, Vec<String>>,
    resources: BTreeMap<String, Vec<String>>,
    when: Option<Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct A2aGrantView {
    agents: Option<Vec<String>>,
}

fn duration(v: Option<&String>, default: Duration) -> Duration {
    v.and_then(|s| parse_duration(s)).unwrap_or(default)
}

fn generation(parts: &[&str]) -> u64 {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0]);
    }
    let digest = h.finalize();
    let mut first = [0u8; 8];
    first.copy_from_slice(digest.get(..8).unwrap_or(&[0; 8]));
    u64::from_be_bytes(first)
}

fn header_map(values: &BTreeMap<String, String>, at: &DocPath, errors: &mut Vec<ConfigError>) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in values {
        let parsed = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        );
        match parsed {
            (Ok(n), Ok(v)) => {
                map.insert(n, v);
            }
            _ => errors.push(ConfigError::new(
                &at.key(name),
                DetailCode::InvalidFormat,
                "not a valid header name and value",
            )),
        }
    }
    map
}

fn identity_map(values: &BTreeMap<String, String>) -> Vec<(HeaderName, String)> {
    values
        .iter()
        .filter_map(|(k, v)| Some((HeaderName::from_bytes(k.as_bytes()).ok()?, v.clone())))
        .collect()
}

fn uri(s: Option<&str>) -> Option<Uri> {
    s.and_then(|u| u.parse::<Uri>().ok())
}

fn compile_server(name: &str, v: &Value, errors: &mut Vec<ConfigError>) -> Option<ServerRt> {
    let at = DocPath::root().key("mcp").key("servers").key(name);
    let s: ServerView = serde_json::from_value(v.clone()).unwrap_or_default();
    let transport = match s.transport.as_deref() {
        Some("streamable_http") => Transport::StreamableHttp,
        Some("sse") => Transport::Sse,
        Some("stdio") => Transport::Stdio,
        Some("openapi") => Transport::OpenApi,
        _ => Transport::Unsupported,
    };
    let stdio = (transport == Transport::Stdio).then(|| StdioSpec {
        command: s.command.clone().unwrap_or_default(),
        args: s.args.clone(),
        env: s.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        processes: s.processes.unwrap_or(1).clamp(1, 64),
    });
    let openapi = if transport == Transport::OpenApi {
        let o = s.openapi.as_ref();
        let filter = o.map_or(
            Filter {
                include: vec!["*".to_owned()],
                exclude: Vec::new(),
            },
            |o| Filter {
                include: o
                    .operations
                    .include
                    .clone()
                    .unwrap_or_else(|| vec!["*".to_owned()]),
                exclude: o.operations.exclude.clone().unwrap_or_default(),
            },
        );
        let parsed = o
            .and_then(|o| o.spec.as_deref())
            .and_then(|text| crate::openapi::OpenApi::parse(text, &filter));
        if parsed.is_none() {
            errors.push(ConfigError::new(
                &at.key("openapi").key("spec"),
                DetailCode::InvalidFormat,
                "not an OpenAPI 3 or Swagger 2.0 document",
            ));
        }
        parsed.map(Arc::new)
    } else {
        None
    };
    let local = stdio
        .as_ref()
        .map(|p| {
            format!(
                "{}\n{}\n{:?}\n{}",
                p.command,
                p.args.join("\u{1f}"),
                p.env,
                p.processes
            )
        })
        .unwrap_or_default();
    let url = uri(s.url.as_deref()).unwrap_or_else(|| Uri::from_static("http://invalid.invalid/"));
    let headers = header_map(&s.headers, &at.key("headers"), errors);
    let auth = s.auth.as_ref().map(Value::to_string).unwrap_or_default();
    let mut header_parts: Vec<String> = s.headers.iter().map(|(k, v)| format!("{k}:{v}")).collect();
    header_parts.sort();
    let joined = header_parts.join("\n");
    let generation = generation(&[
        s.transport.as_deref().unwrap_or_default(),
        s.url.as_deref().unwrap_or_default(),
        &joined,
        &auth,
        &local,
    ]);
    let filter = |f: &FilterView| Filter {
        include: f.include.clone().unwrap_or_else(|| vec!["*".to_owned()]),
        exclude: f.exclude.clone().unwrap_or_default(),
    };
    Some(ServerRt {
        name: name.to_owned(),
        transport,
        enabled: s.enabled.unwrap_or(true),
        url,
        protocol: match s.protocol.as_deref() {
            Some("legacy") => Protocol::Legacy,
            Some("modern") => Protocol::Modern,
            _ => Protocol::Auto,
        },
        discovery_ttl: duration(s.discovery_ttl.as_ref(), Duration::from_secs(300)),
        prefix: s.prefix.clone().unwrap_or_else(|| name.to_owned()),
        headers,
        client_headers: s.client_headers.iter().map(|h| h.to_ascii_lowercase()).collect(),
        identity_headers: identity_map(&s.identity_headers),
        tools: filter(&s.tools),
        prompts: filter(&s.prompts),
        resources: filter(&s.resources),
        tool_cache_ttl: duration(s.tool_cache_ttl.as_ref(), Duration::from_secs(60)),
        init_timeout: duration(s.init_timeout.as_ref(), Duration::from_secs(10)),
        request_timeout: duration(s.request_timeout.as_ref(), Duration::from_secs(120)),
        stream_idle_timeout: duration(s.stream_idle_timeout.as_ref(), Duration::from_secs(300)),
        retries: s.retries.unwrap_or(1).min(3),
        pin_tool_definitions: s.pin_tool_definitions.unwrap_or(false),
        generation,
        auth: s
            .auth
            .as_ref()
            .and_then(|a| crate::credentials::UpstreamAuth::compile(a, s.url.as_deref().unwrap_or_default()))
            .map(Arc::new),
        stdio,
        openapi,
    })
}

fn compile_gateway(name: &str, v: &Value) -> GatewayRt {
    let g: GatewayView = serde_json::from_value(v.clone()).unwrap_or_default();
    let prefixed = match g.prefix_mode.as_deref() {
        Some("always") => true,
        Some("never") => false,
        _ => g.servers.len() > 1,
    };
    let server_info = match &g.server_info {
        Some(Value::String(s)) if s == "upstream" => ServerInfo::Upstream,
        Some(Value::Object(o)) => {
            let mut info = Map::new();
            info.insert(
                "name".into(),
                o.get("name").cloned().unwrap_or_else(|| Value::from("antirion")),
            );
            info.insert(
                "version".into(),
                o.get("version")
                    .cloned()
                    .unwrap_or_else(|| Value::from(BUILD_VERSION)),
            );
            if let Some(t) = o.get("title").filter(|t| !t.is_null()) {
                info.insert("title".into(), t.clone());
            }
            ServerInfo::Gateway(Value::Object(info))
        }
        _ => ServerInfo::Gateway(serde_json::json!({"name": "antirion", "version": BUILD_VERSION})),
    };
    GatewayRt {
        name: name.to_owned(),
        servers: g.servers.clone(),
        enabled: g.enabled.unwrap_or(true),
        prefixed,
        stateless: g.session_mode.as_deref() == Some("stateless"),
        fail_closed: g.on_server_failure.as_deref() == Some("fail"),
        server_info,
        client_cache_max_ttl: duration(g.client_cache_max_ttl.as_ref(), Duration::ZERO),
        allowed_origins: g.allowed_origins.clone(),
        max_body: g
            .max_body
            .as_deref()
            .and_then(parse_size)
            .unwrap_or(DEFAULT_MAX_BODY),
        oauth: g.oauth.map(|o| OauthRt {
            resource: o.resource,
            authorization_servers: o.authorization_servers,
            scopes_supported: o.scopes_supported,
            audiences: o.audiences,
            proxy_registration: o.proxy_registration,
        }),
        restricted: false,
        modern_bridge: g.modern_bridge.unwrap_or(false),
        tool_search: g.tool_search,
    }
}

fn compile_agent(name: &str, v: &Value, errors: &mut Vec<ConfigError>) -> Option<AgentRt> {
    let at = DocPath::root().key("a2a").key("agents").key(name);
    let a: AgentView = serde_json::from_value(v.clone()).unwrap_or_default();
    let card_url = uri(a.card_url.as_deref())?;
    let headers = header_map(&a.headers, &at.key("headers"), errors);
    let mut header_parts: Vec<String> = a.headers.iter().map(|(k, v)| format!("{k}:{v}")).collect();
    header_parts.sort();
    let joined = header_parts.join("\n");
    Some(AgentRt {
        name: name.to_owned(),
        generation: generation(&[
            a.card_url.as_deref().unwrap_or_default(),
            a.url.as_deref().unwrap_or_default(),
            &joined,
        ]),
        card_url,
        url: uri(a.url.as_deref()),
        protocol: a.protocol.filter(|p| p != "auto"),
        enabled: a.enabled.unwrap_or(true),
        public_card: a.public_card.unwrap_or(false),
        headers,
        identity_headers: identity_map(&a.identity_headers),
        card_cache_ttl: duration(a.card_cache_ttl.as_ref(), Duration::from_secs(300)),
        request_timeout: duration(a.request_timeout.as_ref(), Duration::from_secs(300)),
        stream_idle_timeout: duration(a.stream_idle_timeout.as_ref(), Duration::from_secs(300)),
        extra_methods: a.extra_methods,
    })
}

fn level(block: Option<&Value>) -> Level {
    let policies = block.and_then(|b| b.get("policies"));
    Level {
        mcp: policies
            .and_then(|p| p.get("mcp"))
            .and_then(|m| serde_json::from_value::<McpGrantView>(m.clone()).ok())
            .map(|m| McpGrant {
                servers: m.servers,
                tools: m.tools,
                prompts: m.prompts,
                resources: m.resources,
                when: m.when.as_ref().map(|w| Arc::new(crate::when::rules(w))),
            }),
        agents: policies
            .and_then(|p| p.get("a2a"))
            .and_then(|m| serde_json::from_value::<A2aGrantView>(m.clone()).ok())
            .and_then(|m| m.agents),
    }
}

fn levels(doc: &Value, collection: &str) -> HashMap<String, Level> {
    doc.get(collection)
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(name, v)| (name.clone(), level(Some(v))))
        .collect()
}

fn restricts(grant: &McpGrant, gateway: &GatewayRt) -> bool {
    let servers_restricted = grant.servers.as_ref().is_some_and(|list| {
        !list.iter().any(|s| s == "*") && !gateway.servers.iter().all(|s| list.contains(s))
    });
    let items = |m: &BTreeMap<String, Vec<String>>| gateway.servers.iter().any(|s| m.contains_key(s));
    servers_restricted
        || grant.when.is_some()
        || items(&grant.tools)
        || items(&grant.prompts)
        || items(&grant.resources)
}

fn session_keys(doc: &Value, errors: &mut Vec<ConfigError>) -> Vec<SessionKey> {
    let list = doc
        .pointer("/mcp/session/keys")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut keys = Vec::new();
    for (i, k) in list.iter().enumerate() {
        let at = DocPath::root().key("mcp").key("session").key("keys").index(i);
        let id = k
            .get("id")
            .and_then(Value::as_u64)
            .and_then(|n| u8::try_from(n).ok());
        let secret = k
            .get("secret")
            .and_then(Value::as_str)
            .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s.trim()).ok())
            .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok());
        match (id, secret) {
            (Some(id), Some(secret)) => keys.push(SessionKey { id, secret }),
            (_, None) => errors.push(ConfigError::new(
                &at.key("secret"),
                DetailCode::InvalidFormat,
                "must be base64 of 32 bytes",
            )),
            (None, _) => errors.push(ConfigError::new(
                &at.key("id"),
                DetailCode::OutOfRange,
                "must be an integer from 0 to 255",
            )),
        }
    }
    keys
}

/// The configuration errors of the MCP part of a document whose secrets are not resolved yet: an
/// OpenAPI document given as a secret reference is read once it is resolved.
#[must_use]
pub fn check_document(doc: &Value) -> Vec<ConfigError> {
    let mut doc = doc.clone();
    if let Some(servers) = doc.pointer_mut("/mcp/servers").and_then(Value::as_object_mut) {
        servers.retain(|_, s| {
            !s.pointer("/openapi/spec")
                .and_then(Value::as_str)
                .is_some_and(|t| t.trim_start().starts_with("${"))
        });
    }
    compile(&doc).err().unwrap_or_default()
}

/// Compiles the MCP and A2A part from a resolved document.
pub fn compile(resolved: &Value) -> Result<McpSnapshot, Vec<ConfigError>> {
    let mut errors = Vec::new();
    let mut snap = McpSnapshot {
        max_lifetime: resolved
            .pointer("/mcp/session/max_lifetime")
            .and_then(Value::as_str)
            .and_then(parse_duration)
            .unwrap_or(Duration::from_secs(24 * 3600)),
        ..McpSnapshot::default()
    };
    snap.session_keys = session_keys(resolved, &mut errors);
    for (name, v) in resolved
        .pointer("/mcp/servers")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        if let Some(s) = compile_server(name, v, &mut errors) {
            snap.servers.insert(name.clone(), Arc::new(s));
        }
    }
    for (name, v) in resolved
        .pointer("/a2a/agents")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        if let Some(a) = compile_agent(name, v, &mut errors) {
            snap.agents.insert(name.clone(), Arc::new(a));
        }
    }
    snap.grants = Grants {
        gateway: level(Some(resolved)),
        organizations: levels(resolved, "organizations"),
        teams: levels(resolved, "teams"),
        projects: levels(resolved, "projects"),
        keys: levels(resolved, "keys"),
    };
    let all_grants: Vec<&McpGrant> = std::iter::once(&snap.grants.gateway)
        .chain(snap.grants.organizations.values())
        .chain(snap.grants.teams.values())
        .chain(snap.grants.projects.values())
        .chain(snap.grants.keys.values())
        .filter_map(|l| l.mcp.as_ref())
        .collect();
    for (name, v) in resolved
        .pointer("/mcp/gateways")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let mut g = compile_gateway(name, v);
        g.restricted = all_grants.iter().any(|grant| restricts(grant, &g));
        snap.gateways.insert(name.clone(), Arc::new(g));
    }
    if errors.is_empty() { Ok(snap) } else { Err(errors) }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn defaults_and_prefix_mode() {
        let doc = json!({"mcp": {
            "servers": {"a": {"transport": "streamable_http", "url": "http://127.0.0.1:1/mcp"},
                        "b": {"transport": "sse", "url": "http://127.0.0.1:1/sse", "prefix": "bee"}},
            "gateways": {"one": {"servers": ["a"]}, "two": {"servers": ["a", "b"]}}}});
        let snap = compile(&doc).unwrap();
        assert!(!snap.gateways["one"].prefixed);
        assert!(snap.gateways["two"].prefixed);
        assert_eq!(snap.servers["b"].prefix, "bee");
        assert_eq!(snap.servers["a"].tool_cache_ttl, Duration::from_secs(60));
        assert_eq!(snap.gateways["one"].max_body, 8 << 20);
        assert!(!snap.gateways["two"].restricted);
    }

    #[test]
    fn generation_follows_url_and_credentials() {
        let one = json!({"mcp": {"servers": {"a": {"transport": "streamable_http", "url": "http://h/1"}}}});
        let two = json!({"mcp": {"servers": {"a": {"transport": "streamable_http", "url": "http://h/2"}}}});
        let g1 = compile(&one).unwrap().servers["a"].generation;
        let g2 = compile(&two).unwrap().servers["a"].generation;
        assert_ne!(g1, g2);
        assert_eq!(g1, compile(&one).unwrap().servers["a"].generation);
    }

    #[test]
    fn restricting_grants_anywhere_mark_the_gateway() {
        let doc = json!({
            "mcp": {"servers": {"a": {"transport": "streamable_http", "url": "http://h/1"},
                                "b": {"transport": "streamable_http", "url": "http://h/2"}},
                    "gateways": {"g": {"servers": ["a", "b"]}}},
            "policies": {"mcp": {"servers": ["*"]}},
            "keys": {"k": {"policies": {"mcp": {"servers": ["a"]}}}}});
        assert!(compile(&doc).unwrap().gateways["g"].restricted);
        let open = json!({
            "mcp": {"servers": {"a": {"transport": "streamable_http", "url": "http://h/1"}},
                    "gateways": {"g": {"servers": ["a"]}}},
            "policies": {"mcp": {"servers": ["a"]}}});
        assert!(!compile(&open).unwrap().gateways["g"].restricted);
    }

    #[test]
    fn session_keys_must_be_32_bytes() {
        let bad = json!({"mcp": {"session": {"keys": [{"id": 1, "secret": "c2hvcnQ="}]}}});
        let errs = compile(&bad).unwrap_err();
        assert_eq!(errs[0].path.to_string(), "mcp.session.keys[0].secret");
    }
}
