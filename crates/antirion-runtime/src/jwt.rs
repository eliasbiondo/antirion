//! JWT providers (`auth.jwt`) and the JWKS cache: each provider's keys are fetched when first
//! needed, refreshed after `jwks_refresh_interval`, refetched on an unknown `kid` at most once per
//! 30 s, and the last good set is used for up to `jwks_max_stale` while the issuer is unreachable.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use antirion_config::scalar::parse_duration;
use antirion_config::{ConfigError, DetailCode, DocPath};
use antirion_core::UtcMicros;
use antirion_egress::{EgressClient, EgressPolicy, OutboundRequest, TlsProfile, TrustClass};
use antirion_identity::jwt::{Alg, Jwk, Rules, parse_jwks};
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use http::{HeaderMap, Method, Uri};
use http_body_util::BodyExt;
use parking_lot::Mutex;
use serde_json::{Map, Value};

use crate::identity::KeyRt;

/// One compiled JWT provider.
#[derive(Clone, Debug)]
pub struct JwtProviderRt {
    pub name: String,
    pub rules: Rules,
    pub jwks_uri: Option<String>,
    pub inline: Option<Vec<Jwk>>,
    pub refresh: Duration,
    pub max_stale: Duration,
    pub end_user_claim: Option<String>,
    /// Ordered `(claims to match, key name)` rules.
    pub mappings: Vec<(Map<String, Value>, String)>,
    /// The operator lets the JWKS come over plain http or an unverified TLS connection.
    pub allow_insecure_jwks: bool,
}

fn dur(v: Option<&Value>, default: Duration) -> Duration {
    v.and_then(Value::as_str)
        .and_then(parse_duration)
        .unwrap_or(default)
}

/// Compiles `auth.jwt`; every `mappings[].key` must name a `kind: jwt` key.
pub fn compile(
    doc: &Value,
    jwt_keys: &HashMap<String, Arc<KeyRt>>,
) -> Result<Vec<Arc<JwtProviderRt>>, Vec<ConfigError>> {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    let providers = doc
        .get("auth")
        .and_then(|a| a.get("jwt"))
        .and_then(Value::as_object);
    for (name, p) in providers.into_iter().flatten() {
        let at = DocPath::root().key("auth").key("jwt").key(name);
        let str_list = |k: &str| -> Vec<String> {
            p.get(k)
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                .unwrap_or_default()
        };
        let algorithms = match p.get("algorithms").and_then(Value::as_array) {
            Some(list) => list
                .iter()
                .filter_map(Value::as_str)
                .filter_map(Alg::parse)
                .collect(),
            None => Alg::DEFAULT.to_vec(),
        };
        let mut mappings = Vec::new();
        for (i, m) in p
            .get("mappings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let key = m
                .get("key")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if !jwt_keys.contains_key(&key) {
                errors.push(ConfigError::new(
                    &at.key("mappings").index(i).key("key"),
                    DetailCode::UnresolvedReference,
                    "must name a key with kind jwt",
                ));
            }
            let claims = m
                .get("match")
                .and_then(|m| m.get("claims"))
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            mappings.push((claims, key));
        }
        let rules = Rules {
            issuer: p
                .get("issuer")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            audiences: str_list("audiences"),
            algorithms,
            hmac_secret: p
                .get("hmac_secret")
                .and_then(Value::as_str)
                .map(|s| s.as_bytes().to_vec()),
            clock_skew: i64::try_from(dur(p.get("clock_skew"), Duration::from_secs(60)).as_secs())
                .unwrap_or(60),
            max_lifetime: p
                .get("max_token_lifetime")
                .and_then(Value::as_str)
                .and_then(parse_duration)
                .and_then(|d| i64::try_from(d.as_secs()).ok()),
        };
        out.push(Arc::new(JwtProviderRt {
            name: name.clone(),
            rules,
            jwks_uri: p.get("jwks_uri").and_then(Value::as_str).map(str::to_owned),
            inline: p.get("jwks").filter(|j| j.is_object()).map(parse_jwks),
            refresh: dur(p.get("jwks_refresh_interval"), Duration::from_secs(300)),
            max_stale: dur(p.get("jwks_max_stale"), Duration::from_secs(86_400)),
            end_user_claim: p.get("end_user_claim").and_then(Value::as_str).map(str::to_owned),
            mappings,
            allow_insecure_jwks: p.get("allow_insecure_jwks").and_then(Value::as_bool) == Some(true),
        }));
    }
    if errors.is_empty() { Ok(out) } else { Err(errors) }
}

/// One single sign-on provider of `auth.admin.sso.oidc`.
#[derive(Clone, Debug, Default)]
pub struct OidcProviderRt {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    pub scopes: Vec<String>,
    pub allowed_email_domains: Option<Vec<String>>,
    pub jit_provisioning: bool,
    /// `(claim, value, role, scope)` rules; every match adds a binding.
    pub role_mappings: Vec<(String, String, String, String)>,
    /// What the login page calls the provider.
    pub display_name: Option<String>,
    /// `(role, scope)` given to a provisioned user no role mapping matched.
    pub default_role_bindings: Vec<(String, String)>,
    /// The operator lets discovery and the JWKS come over plain http or an unverified TLS
    /// connection.
    pub allow_insecure_discovery: bool,
}

/// Compiles `auth.admin.sso.oidc`.
#[must_use]
pub fn compile_oidc(doc: &Value) -> HashMap<String, OidcProviderRt> {
    let mut out = HashMap::new();
    let providers = doc.pointer("/auth/admin/sso/oidc").and_then(Value::as_object);
    for (name, p) in providers.into_iter().flatten() {
        let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
        let list = |k: &str| -> Option<Vec<String>> {
            p.get(k)
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        };
        let role_mappings = p
            .get("role_mappings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|m| {
                let f = |k: &str| m.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
                (f("claim"), f("value"), f("role"), f("scope"))
            })
            .collect();
        out.insert(
            name.clone(),
            OidcProviderRt {
                issuer: s("issuer"),
                client_id: s("client_id"),
                client_secret: s("client_secret"),
                scopes: list("scopes").unwrap_or_else(|| {
                    ["openid", "email", "profile", "groups"]
                        .map(str::to_owned)
                        .to_vec()
                }),
                allowed_email_domains: list("allowed_email_domains"),
                jit_provisioning: p.get("jit_provisioning").and_then(Value::as_bool).unwrap_or(true),
                role_mappings,
                display_name: p.get("display_name").and_then(Value::as_str).map(str::to_owned),
                default_role_bindings: pairs(p.get("default_role_bindings"), "role", "scope"),
                allow_insecure_discovery: p.get("allow_insecure_discovery").and_then(Value::as_bool)
                    == Some(true),
            },
        );
    }
    out
}

fn pairs(v: Option<&Value>, a: &str, b: &str) -> Vec<(String, String)> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let f = |k: &str| m.get(k).and_then(Value::as_str).map(str::to_owned);
            Some((f(a)?, f(b)?))
        })
        .collect()
}

/// A GitHub sign-in provider (`auth.admin.sso.oauth.<name>`).
#[derive(Clone)]
pub struct OauthProviderRt {
    pub client_id: String,
    pub client_secret: String,
    pub display_name: Option<String>,
    /// The REST API, `https://api.github.com` by default.
    pub api_base_url: String,
    /// Where the authorization and token endpoints are: `https://github.com` for the public API, the
    /// host of a GitHub Enterprise Server API (`https://<host>/api/v3`).
    pub web_base_url: String,
    pub allowed_organizations: Vec<String>,
    pub jit_provisioning: bool,
    /// `(organization, team, role, scope)`; without a team the organization's members match.
    pub role_mappings: Vec<(String, Option<String>, String, String)>,
}

impl std::fmt::Debug for OauthProviderRt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OauthProviderRt")
            .field("client_id", &self.client_id)
            .field("api_base_url", &self.api_base_url)
            .finish_non_exhaustive()
    }
}

/// The site a GitHub API base URL belongs to.
fn web_base(api: &str) -> String {
    let api = api.trim_end_matches('/');
    if api == "https://api.github.com" {
        return "https://github.com".to_owned();
    }
    api.strip_suffix("/api/v3").unwrap_or(api).to_owned()
}

/// Compiles `auth.admin.sso.oauth`.
#[must_use]
pub fn compile_oauth(doc: &Value) -> HashMap<String, OauthProviderRt> {
    let mut out = HashMap::new();
    let providers = doc.pointer("/auth/admin/sso/oauth").and_then(Value::as_object);
    for (name, p) in providers.into_iter().flatten() {
        let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
        let api = p
            .get("api_base_url")
            .and_then(Value::as_str)
            .unwrap_or("https://api.github.com")
            .trim_end_matches('/')
            .to_owned();
        let role_mappings = p
            .get("role_mappings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|m| {
                let f = |k: &str| m.get(k).and_then(Value::as_str).map(str::to_owned);
                (
                    f("organization").unwrap_or_default(),
                    f("team"),
                    f("role").unwrap_or_default(),
                    f("scope").unwrap_or_default(),
                )
            })
            .collect();
        out.insert(
            name.clone(),
            OauthProviderRt {
                client_id: s("client_id"),
                client_secret: s("client_secret"),
                display_name: p.get("display_name").and_then(Value::as_str).map(str::to_owned),
                web_base_url: web_base(&api),
                api_base_url: api,
                allowed_organizations: p
                    .get("allowed_organizations")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                    .unwrap_or_default(),
                jit_provisioning: p.get("jit_provisioning").and_then(Value::as_bool).unwrap_or(true),
                role_mappings,
            },
        );
    }
    out
}

/// Why no key set can be used.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Unavailable;

#[derive(Debug, Default)]
struct Entry {
    keys: Option<Arc<Vec<Jwk>>>,
    /// When the keys were last fetched successfully (gateway clock).
    fetched: Option<UtcMicros>,
    /// When a fetch was last attempted (gateway clock).
    attempted: Option<UtcMicros>,
    /// When an unknown `kid` last caused a refetch (gateway clock).
    unknown_kid: Option<UtcMicros>,
}

/// The JWKS of every provider, keyed by provider name and URI.
#[derive(Debug, Default)]
pub struct JwksCache {
    entries: Mutex<HashMap<String, Arc<tokio::sync::Mutex<Entry>>>>,
}

const UNKNOWN_KID_INTERVAL: Duration = Duration::from_secs(30);
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_JWKS: usize = 1 << 20;

/// What a JWKS fetch is sent with.
#[derive(Debug)]
pub struct FetchCx<'a> {
    pub client: &'a EgressClient,
    pub policy: &'a Arc<EgressPolicy>,
    pub tls: &'a TlsProfile,
    pub now: UtcMicros,
    /// Refuse plain http and a TLS profile that does not verify servers: for documents that carry
    /// the keys tokens are checked against (discovery, a JWKS).
    pub secure: bool,
}

/// Whether fetching `uri` with `tls` could be tampered with on the way: plain http, or TLS that does
/// not verify the server.
#[must_use]
pub fn insecure_transport(uri: &str, tls: &TlsProfile) -> bool {
    antirion_identity::saml::is_plaintext(uri) || !tls.verify
}

/// Signing-key sources that would be fetched over plain http outside test mode: an OIDC
/// provider's `issuer` (its discovery and JWKS follow it) and an `auth.jwt` provider's `jwks_uri`,
/// unless the provider sets `allow_insecure_discovery` or `allow_insecure_jwks`.
#[must_use]
pub fn check_transport(doc: &Value, test_mode: bool) -> Vec<ConfigError> {
    if test_mode {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut each = |pointer: &str, base: DocPath, field: &str, opt_in: &str| {
        for (name, p) in doc
            .pointer(pointer)
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            let plain = p
                .get(field)
                .and_then(Value::as_str)
                .is_some_and(antirion_identity::saml::is_plaintext);
            if plain && p.get(opt_in).and_then(Value::as_bool) != Some(true) {
                out.push(ConfigError::new(
                    &base.key(name).key(field),
                    DetailCode::InvalidFormat,
                    format!(
                        "must be an https URL: the keys tokens are checked against come from it (set {opt_in} to \
                         fetch them over http)"
                    ),
                ));
            }
        }
    };
    each(
        "/auth/admin/sso/oidc",
        DocPath::root().key("auth").key("admin").key("sso").key("oidc"),
        "issuer",
        "allow_insecure_discovery",
    );
    each(
        "/auth/jwt",
        DocPath::root().key("auth").key("jwt"),
        "jwks_uri",
        "allow_insecure_jwks",
    );
    out
}

/// Sends one request to an identity provider (operator trust, the egress rules) and reads a JSON
/// object answer; `None` on any failure or a non-2xx status.
pub async fn http_json(cx: &FetchCx<'_>, method: Method, uri: &str, form: Option<String>) -> Option<Value> {
    fetch_json(cx, method, uri, form, None)
        .await
        .filter(Value::is_object)
}

/// A `GET` with a bearer token whose answer is any JSON value (an API listing answers an array).
pub async fn get_json(cx: &FetchCx<'_>, uri: &str, bearer: &str) -> Option<Value> {
    fetch_json(cx, Method::GET, uri, None, Some(bearer)).await
}

async fn fetch_json(
    cx: &FetchCx<'_>,
    method: Method,
    uri: &str,
    form: Option<String>,
    bearer: Option<&str>,
) -> Option<Value> {
    if cx.secure && insecure_transport(uri, cx.tls) {
        tracing::warn!(
            uri,
            "an identity document was not fetched: plain http or an unverified TLS profile"
        );
        return None;
    }
    let uri: Uri = uri.parse().ok()?;
    let mut headers = HeaderMap::new();
    if let Some(token) = bearer {
        headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_str(&format!("Bearer {token}")).ok()?,
        );
    }
    headers.insert(
        http::header::USER_AGENT,
        http::HeaderValue::from_static("antirion"),
    );
    if form.is_some() {
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
    }
    headers.insert(
        http::header::ACCEPT,
        http::HeaderValue::from_static("application/json"),
    );
    let request = OutboundRequest {
        method,
        uri,
        headers,
        body: form.map(Bytes::from).unwrap_or_default().into(),
        response_timeout: FETCH_TIMEOUT,
        connect_timeout: None,
        fresh_connection: false,
        trust: TrustClass::Operator,
        tls: cx.tls.clone(),
        policy: Arc::clone(cx.policy),
        resend_when_closed: false,
    };
    let read = async {
        let resp = cx.client.send(request).await.ok()?;
        if !resp.status.is_success() {
            return None;
        }
        let mut body = resp.body.into_data_stream();
        let mut out = BytesMut::new();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.ok()?;
            if out.len() + chunk.len() > MAX_JWKS {
                return None;
            }
            out.extend_from_slice(&chunk);
        }
        serde_json::from_slice::<Value>(&out).ok()
    };
    tokio::time::timeout(FETCH_TIMEOUT, read).await.ok().flatten()
}

/// Fetches and parses a JWKS document.
pub async fn fetch(cx: &FetchCx<'_>, uri: &str) -> Option<Vec<Jwk>> {
    let doc = http_json(cx, Method::GET, uri, None).await?;
    doc.get("keys")
        .is_some_and(Value::is_array)
        .then(|| parse_jwks(&doc))
}

impl JwksCache {
    fn entry(&self, p: &JwtProviderRt, uri: &str) -> Arc<tokio::sync::Mutex<Entry>> {
        let id = format!("{}\u{0}{uri}", p.name);
        Arc::clone(self.entries.lock().entry(id).or_default())
    }

    /// The keys to verify a token of `p` with; `kid` is the token's key id, when it has one. A stale
    /// set is refreshed first; a `kid` the set lacks causes one refetch per 30 s.
    pub async fn keys(
        &self,
        p: &JwtProviderRt,
        kid: Option<&str>,
        cx: &FetchCx<'_>,
    ) -> Result<Arc<Vec<Jwk>>, Unavailable> {
        let Some(uri) = p.jwks_uri.as_deref() else {
            return Ok(Arc::new(p.inline.clone().unwrap_or_default()));
        };
        let entry = self.entry(p, uri);
        let mut e = entry.lock().await;
        let age = |t: Option<UtcMicros>| t.map(|t| cx.now.since(t));
        let due = match age(e.fetched) {
            None => age(e.attempted).is_none_or(|a| a >= Duration::from_secs(1)),
            Some(a) => a >= p.refresh,
        };
        if due {
            e.attempted = Some(cx.now);
            if let Some(keys) = fetch(cx, uri).await {
                e.keys = Some(Arc::new(keys));
                e.fetched = Some(cx.now);
            }
        }
        let missing = |keys: &Option<Arc<Vec<Jwk>>>| {
            kid.is_some_and(|k| {
                keys.as_ref()
                    .is_none_or(|ks| !ks.iter().any(|j| j.kid() == Some(k)))
            })
        };
        if !due && missing(&e.keys) && age(e.unknown_kid).is_none_or(|a| a >= UNKNOWN_KID_INTERVAL) {
            e.unknown_kid = Some(cx.now);
            if let Some(keys) = fetch(cx, uri).await {
                e.keys = Some(Arc::new(keys));
                e.fetched = Some(cx.now);
            }
        }
        match (&e.keys, age(e.fetched)) {
            (Some(keys), Some(a)) if a <= p.max_stale => Ok(Arc::clone(keys)),
            _ => Err(Unavailable),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn signing_key_sources_need_https_outside_test_mode() {
        let doc = json!({"auth": {
            "admin": {"sso": {"oidc": {
                "corp": {"issuer": "http://idp.internal"},
                "lab": {"issuer": "http://idp.lab", "allow_insecure_discovery": true},
                "ok": {"issuer": "https://idp.example"}}}},
            "jwt": {"svc": {"issuer": "x", "jwks_uri": "http://jwks.internal/keys"}}}});
        let paths: Vec<String> = check_transport(&doc, false).into_iter().map(|e| e.path).collect();
        assert_eq!(
            paths,
            ["auth.admin.sso.oidc.corp.issuer", "auth.jwt.svc.jwks_uri"]
        );
        assert!(check_transport(&doc, true).is_empty());
    }

    #[test]
    fn plain_http_and_unverified_tls_are_insecure() {
        let Ok(tls) = TlsProfile::default_profile() else {
            return;
        };
        assert!(insecure_transport(
            "http://idp/.well-known/openid-configuration",
            &tls
        ));
        assert_eq!(insecure_transport("https://idp/keys", &tls), !tls.verify);
    }
}
