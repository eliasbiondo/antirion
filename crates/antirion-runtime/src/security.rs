//! Compiled `security` settings: browser-facing controls (CORS per plane), inbound limits, the
//! egress policy, header rules, the client IP and listener TLS.

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use antirion_config::scalar::{parse_duration, parse_size};
use antirion_config::views::{CorsPlaneView, SecurityView, view};
use antirion_config::{ConfigError, DetailCode, DocPath};
use antirion_egress::{EgressPolicy, TlsProfile, TlsSettings};
use http::HeaderName;
use ipnet::IpNet;
use serde_json::Value;

use crate::client_ip::{ClientIpRt, IpHeader};
use crate::header_rules::HeaderTable;

/// Default limit on JSON request bodies.
pub const DEFAULT_MAX_BODY: u64 = 32 << 20;
/// `security.limits.max_upload_body` when unset.
pub const DEFAULT_MAX_UPLOAD_BODY: u64 = 512 << 20;
/// `security.limits.max_upstream_response` when unset.
pub const DEFAULT_MAX_UPSTREAM_RESPONSE: u64 = 128 << 20;
/// Default limit on JSON nesting depth.
pub const DEFAULT_MAX_JSON_DEPTH: usize = 128;

/// An origin reduced to its comparable parts: lowercase scheme and host, explicit port.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Origin {
    scheme: String,
    host: String,
    port: u16,
}

fn parse_origin(s: &str) -> Option<Origin> {
    let (scheme, rest) = s.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };
    if rest.is_empty() || rest.contains(['/', '?', '#', '@']) {
        return None;
    }
    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) if !h.ends_with(']') || h.starts_with('[') => (h, p.parse::<u16>().ok()?),
        _ => (rest, default_port),
    };
    if host.is_empty() {
        return None;
    }
    Some(Origin {
        scheme,
        host: host.to_ascii_lowercase(),
        port,
    })
}

/// One compiled `security.cors.<plane>` block.
#[derive(Clone, Debug, Default)]
pub struct CorsRt {
    any: bool,
    exact: Vec<Origin>,
    /// `scheme://*.suffix` entries, stored with the leading dot of the suffix.
    wildcard: Vec<Origin>,
    pub methods: String,
    /// `None` reflects the requested headers.
    pub allow_headers: Option<String>,
    pub expose_headers: String,
    pub max_age_secs: u64,
    pub allow_credentials: bool,
}

impl CorsRt {
    fn compile(v: &CorsPlaneView) -> Option<CorsRt> {
        if v.allowed_origins.is_empty() {
            return None;
        }
        let mut rt = CorsRt {
            methods: v
                .allowed_methods
                .clone()
                .unwrap_or_else(|| ["GET", "POST", "DELETE", "OPTIONS"].map(str::to_owned).to_vec())
                .join(", "),
            allow_headers: match &v.allow_headers {
                Some(serde_json::Value::Array(list)) => Some(
                    list.iter()
                        .filter_map(serde_json::Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
                _ => None,
            },
            expose_headers: v
                .expose_headers
                .clone()
                .unwrap_or_else(|| vec!["*".to_owned()])
                .join(", "),
            max_age_secs: v
                .max_age
                .as_deref()
                .and_then(parse_duration)
                .map_or(600, |d| d.as_secs()),
            allow_credentials: v.allow_credentials,
            ..CorsRt::default()
        };
        for entry in &v.allowed_origins {
            if entry == "*" {
                rt.any = true;
            } else if let Some(o) = parse_origin(entry) {
                match o.host.strip_prefix('*') {
                    Some(suffix) => rt.wildcard.push(Origin {
                        host: suffix.to_owned(),
                        ..o
                    }),
                    None => rt.exact.push(o),
                }
            }
        }
        Some(rt)
    }

    /// Whether a request `Origin` is allowed. The `null` origin never is.
    #[must_use]
    pub fn allows(&self, origin: &str) -> bool {
        if origin == "null" {
            return false;
        }
        let Some(o) = parse_origin(origin) else {
            return false;
        };
        if self.any || self.exact.contains(&o) {
            return true;
        }
        self.wildcard.iter().any(|w| {
            w.scheme == o.scheme
                && w.port == o.port
                && o.host
                    .strip_suffix(w.host.as_str())
                    .is_some_and(|label| !label.is_empty())
        })
    }

    /// The `Access-Control-Allow-Origin` value for an allowed origin.
    #[must_use]
    pub fn allow_origin_value<'a>(&self, origin: &'a str) -> &'a str {
        if self.any && !self.allow_credentials {
            "*"
        } else {
            origin
        }
    }
}

/// The TLS settings of one listener; certificate files are re-read every reload interval.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListenerTls {
    pub cert_file: PathBuf,
    pub key_file: PathBuf,
    pub min_tls13: bool,
    pub alpn: Vec<Vec<u8>>,
    pub client_auth: ClientAuth,
    /// The CAs client certificates must chain to.
    pub client_ca_file: Option<PathBuf>,
}

/// `client_auth` of a listener: whether clients present a certificate.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum ClientAuth {
    #[default]
    None,
    /// A certificate is verified when presented; connections without one are accepted.
    Optional,
    /// Handshakes without a certificate chaining to `client_ca_file` fail.
    Required,
}

/// Default interval between certificate file reads.
pub const DEFAULT_TLS_RELOAD: Duration = Duration::from_secs(10);

/// Compiled `security` settings, and the `headers` members of every `policies` block.
#[derive(Clone, Debug)]
pub struct SecurityRt {
    pub max_body: u64,
    /// The limit of multipart and binary upload bodies, which are streamed.
    pub max_upload_body: u64,
    /// The largest non-streaming upstream response body read.
    pub max_upstream_response: u64,
    pub max_json_depth: usize,
    /// `security.limits.max_messages`: the longest message (or input item) array accepted.
    pub max_messages: usize,
    /// `security.limits.max_uri_length`, in bytes.
    pub max_uri_length: usize,
    pub cors_data: Option<CorsRt>,
    pub cors_admin: Option<CorsRt>,
    pub egress: Arc<EgressPolicy>,
    /// The TLS profile of every egress without a `tls` block of its own: the default roots plus
    /// `security.egress.tls.ca_file`.
    pub egress_tls: Option<TlsProfile>,
    pub headers: HeaderTable,
    pub client_ip: ClientIpRt,
    pub tls_data: Option<ListenerTls>,
    pub tls_admin: Option<ListenerTls>,
    pub tls_reload: Duration,
    /// The `Strict-Transport-Security` value TLS listeners send, when enabled.
    pub hsts: Option<String>,
}

impl Default for SecurityRt {
    fn default() -> SecurityRt {
        SecurityRt {
            max_body: DEFAULT_MAX_BODY,
            max_upload_body: DEFAULT_MAX_UPLOAD_BODY,
            max_upstream_response: DEFAULT_MAX_UPSTREAM_RESPONSE,
            max_json_depth: DEFAULT_MAX_JSON_DEPTH,
            max_messages: 100_000,
            max_uri_length: 16_384,
            cors_data: None,
            cors_admin: None,
            egress: Arc::new(EgressPolicy::default()),
            egress_tls: TlsProfile::default_profile().ok(),
            headers: HeaderTable::default(),
            client_ip: ClientIpRt::default(),
            tls_data: None,
            tls_admin: None,
            tls_reload: DEFAULT_TLS_RELOAD,
            hsts: None,
        }
    }
}

fn listener_tls(v: Option<&Value>) -> Option<ListenerTls> {
    let v = v?;
    let file = |k: &str| v.get(k).and_then(Value::as_str).map(PathBuf::from);
    let alpn = match v.get("alpn").and_then(Value::as_array) {
        Some(list) => list
            .iter()
            .filter_map(Value::as_str)
            .map(|p| p.as_bytes().to_vec())
            .collect(),
        None => vec![b"h2".to_vec(), b"http/1.1".to_vec()],
    };
    Some(ListenerTls {
        cert_file: file("cert_file")?,
        key_file: file("key_file")?,
        min_tls13: v.get("min_version").and_then(Value::as_str) == Some("1.3"),
        alpn,
        client_auth: match v.get("client_auth").and_then(Value::as_str) {
            Some("optional") => ClientAuth::Optional,
            Some("required") => ClientAuth::Required,
            _ => ClientAuth::None,
        },
        client_ca_file: file("client_ca_file"),
    })
}

fn client_ip(v: Option<&Value>, errors: &mut Vec<ConfigError>) -> ClientIpRt {
    let at = DocPath::root().key("security").key("client_ip");
    let mut rt = ClientIpRt::default();
    for (i, raw) in v
        .and_then(|c| c.get("trusted_proxies"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let raw = raw.as_str().unwrap_or_default();
        match raw
            .parse::<IpNet>()
            .or_else(|_| raw.parse::<IpAddr>().map(IpNet::from))
        {
            Ok(n) => rt.trusted.push(n.trunc()),
            Err(_) => errors.push(ConfigError::new(
                &at.key("trusted_proxies").index(i),
                DetailCode::InvalidFormat,
                "not a CIDR",
            )),
        }
    }
    rt.header = match v.and_then(|c| c.get("header")).and_then(Value::as_str) {
        None => IpHeader::XForwardedFor,
        Some(h) if h.eq_ignore_ascii_case("x-forwarded-for") => IpHeader::XForwardedFor,
        Some(h) if h.eq_ignore_ascii_case("forwarded") => IpHeader::Forwarded,
        Some(h) => match HeaderName::from_bytes(h.to_ascii_lowercase().as_bytes()) {
            Ok(n) => IpHeader::Single(n),
            Err(_) => {
                errors.push(ConfigError::new(
                    &at.key("header"),
                    DetailCode::InvalidFormat,
                    "not a header name",
                ));
                IpHeader::XForwardedFor
            }
        },
    };
    rt
}

fn egress_tls(security: Option<&Value>, errors: &mut Vec<ConfigError>) -> Option<TlsProfile> {
    let path = security
        .and_then(|s| s.get("egress"))
        .and_then(|e| e.get("tls"))
        .and_then(|t| t.get("ca_file"))
        .and_then(Value::as_str);
    let at = DocPath::root()
        .key("security")
        .key("egress")
        .key("tls")
        .key("ca_file");
    let mut settings = TlsSettings::default();
    if let Some(path) = path {
        match std::fs::read(path) {
            Ok(pem) => settings.ca_pem = Some(pem),
            Err(_) => {
                errors.push(ConfigError::new(
                    &at,
                    DetailCode::InvalidFormat,
                    "the CA file cannot be read",
                ));
                return None;
            }
        }
    }
    match TlsProfile::build(&settings) {
        Ok(p) => Some(p),
        Err(e) => {
            errors.push(ConfigError::new(&at, DetailCode::InvalidFormat, e.to_string()));
            None
        }
    }
}

/// The rule the schema cannot state: a listener that verifies client certificates names its CA.
#[must_use]
pub fn check_listeners(doc: &Value) -> Vec<ConfigError> {
    let listeners = doc.pointer("/security/tls/listeners");
    let mut errors = Vec::new();
    for plane in ["data", "admin"] {
        let l = listeners.and_then(|l| l.get(plane));
        let auth = l.and_then(|l| l.get("client_auth")).and_then(Value::as_str);
        let ca = l.and_then(|l| l.get("client_ca_file")).and_then(Value::as_str);
        if matches!(auth, Some("optional" | "required")) && ca.is_none() {
            errors.push(ConfigError::new(
                &DocPath::root()
                    .key("security")
                    .key("tls")
                    .key("listeners")
                    .key(plane)
                    .key("client_ca_file"),
                DetailCode::Required,
                "client_auth optional or required needs client_ca_file",
            ));
        }
    }
    errors
}

impl SecurityRt {
    /// Compiles the `security` key and the header policies of a resolved document.
    pub fn compile(doc: &Value) -> Result<SecurityRt, Vec<ConfigError>> {
        let security = doc.get("security");
        let v: SecurityView = security.map(view).and_then(Result::ok).unwrap_or_default();
        let mut errors = Vec::new();
        let egress = antirion_egress::config::compile_policy(doc).unwrap_or_else(|e| {
            errors.extend(e);
            EgressPolicy::default()
        });
        let headers = HeaderTable::compile(doc).unwrap_or_else(|e| {
            errors.extend(e);
            HeaderTable::default()
        });
        let client_ip = client_ip(security.and_then(|s| s.get("client_ip")), &mut errors);
        let egress_tls = egress_tls(security, &mut errors);
        let tls = security.and_then(|s| s.get("tls"));
        let listeners = tls.and_then(|t| t.get("listeners"));
        let hsts = security.and_then(|s| s.get("hsts"));
        let hsts = hsts
            .filter(|h| h.get("enabled").and_then(Value::as_bool) == Some(true))
            .map(|h| {
                let age = h
                    .get("max_age")
                    .and_then(Value::as_str)
                    .and_then(parse_duration)
                    .map_or(365 * 86_400, |d| d.as_secs());
                let sub = h.get("include_subdomains").and_then(Value::as_bool) == Some(true);
                if sub {
                    format!("max-age={age}; includeSubDomains")
                } else {
                    format!("max-age={age}")
                }
            });
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(SecurityRt {
            max_body: v
                .limits
                .max_body
                .as_deref()
                .and_then(parse_size)
                .unwrap_or(DEFAULT_MAX_BODY),
            max_upload_body: v
                .limits
                .max_upload_body
                .as_deref()
                .and_then(parse_size)
                .unwrap_or(DEFAULT_MAX_UPLOAD_BODY),
            max_upstream_response: v
                .limits
                .max_upstream_response
                .as_deref()
                .and_then(parse_size)
                .unwrap_or(DEFAULT_MAX_UPSTREAM_RESPONSE),
            max_json_depth: v.limits.max_json_depth.unwrap_or(DEFAULT_MAX_JSON_DEPTH),
            max_messages: v.limits.max_messages.unwrap_or(100_000),
            max_uri_length: v.limits.max_uri_length.unwrap_or(16_384),
            cors_data: v.cors.data.as_ref().and_then(CorsRt::compile),
            cors_admin: v.cors.admin.as_ref().and_then(CorsRt::compile),
            egress: Arc::new(egress),
            egress_tls,
            headers,
            client_ip,
            tls_data: listener_tls(listeners.and_then(|l| l.get("data"))),
            tls_admin: listener_tls(listeners.and_then(|l| l.get("admin"))),
            tls_reload: tls
                .and_then(|t| t.get("reload_interval"))
                .and_then(Value::as_str)
                .and_then(parse_duration)
                .unwrap_or(DEFAULT_TLS_RELOAD),
            hsts,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cors(origins: &[&str]) -> CorsRt {
        CorsRt::compile(&CorsPlaneView {
            allowed_origins: origins.iter().map(|s| (*s).to_owned()).collect(),
            ..CorsPlaneView::default()
        })
        .unwrap()
    }

    #[test]
    fn allow_list_semantics() {
        let c = cors(&["https://app.example.com", "https://*.corp.example"]);
        for ok in [
            "https://app.example.com",
            "https://app.example.com:443",
            "HTTPS://APP.EXAMPLE.COM",
            "https://x.corp.example",
            "https://a.b.corp.example",
        ] {
            assert!(c.allows(ok), "{ok}");
        }
        for bad in [
            "https://corp.example",
            "https://app.example.com.evil.io",
            "http://app.example.com",
            "https://app.example.com:8443",
            "http://x.corp.example",
            "https://x.corp.example:8443",
            "null",
            "https://evilapp.example.com",
            "https://x.corp.example.evil",
        ] {
            assert!(!c.allows(bad), "{bad}");
        }
        assert_eq!(c.max_age_secs, 600);
    }

    #[test]
    fn wildcard_list() {
        let c = cors(&["*"]);
        assert!(c.allows("https://anything.example"));
        assert!(!c.allows("null"));
        assert_eq!(c.allow_origin_value("https://anything.example"), "*");
    }

    #[test]
    fn empty_list_is_closed() {
        assert!(CorsRt::compile(&CorsPlaneView::default()).is_none());
    }
}

#[cfg(test)]
mod compile_tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn compiles_every_part() {
        let doc = json!({"security": {
            "client_ip": {"trusted_proxies": ["10.0.0.0/8", "127.0.0.1"], "header": "X-Real-IP"},
            "tls": {"listeners": {"data": {"cert_file": "/c.pem", "key_file": "/k.pem", "min_version": "1.3"}},
                    "reload_interval": "1s"},
            "hsts": {"enabled": true, "max_age": "1d", "include_subdomains": true},
            "egress": {"operator": {"deny_private": true}}
        }});
        let s = SecurityRt::compile(&doc).unwrap();
        assert_eq!(s.client_ip.trusted.len(), 2);
        assert_eq!(
            s.client_ip.header,
            IpHeader::Single(HeaderName::from_static("x-real-ip"))
        );
        let tls = s.tls_data.unwrap();
        assert!(tls.min_tls13);
        assert_eq!(tls.alpn, vec![b"h2".to_vec(), b"http/1.1".to_vec()]);
        assert!(s.tls_admin.is_none());
        assert_eq!(s.tls_reload, Duration::from_secs(1));
        assert_eq!(s.hsts.as_deref(), Some("max-age=86400; includeSubDomains"));
        assert!(s.egress.operator_deny_private);
        assert!(s.egress_tls.is_some());
        let missing = json!({"security": {"egress": {"tls": {"ca_file": "/no/such/ca.pem"}}}});
        assert_eq!(
            SecurityRt::compile(&missing).unwrap_err()[0].path,
            "security.egress.tls.ca_file"
        );
        let bad = json!({"security": {"client_ip": {"trusted_proxies": ["nope"]}}});
        assert_eq!(
            SecurityRt::compile(&bad).unwrap_err()[0].path,
            "security.client_ip.trusted_proxies[0]"
        );
    }
}
