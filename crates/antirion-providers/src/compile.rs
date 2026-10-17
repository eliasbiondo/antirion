//! Compiling the `providers` and `deployments` collections of a secret-resolved document.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use antirion_config::views::{DeploymentView, ProviderView, view};
use antirion_config::{ConfigError, DetailCode, DocPath};
use antirion_egress::{HttpVersion, TlsProfile, TlsSettings};
use http::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

use crate::compile_auth::{compile_auth, key_header};
use crate::{
    AnthropicSettings, BedrockApi, Byok, Capabilities, DeploymentRt, Params, ProviderRt, ProviderTable,
    ProviderType, Publisher, UpstreamAuth,
};

fn header_map(map: &BTreeMap<String, String>, at: &DocPath, errors: &mut Vec<ConfigError>) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (name, value) in map {
        match (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            (Ok(n), Ok(v)) => {
                out.insert(n, v);
            }
            _ => errors.push(ConfigError::new(
                &at.key(name),
                DetailCode::InvalidFormat,
                "not a valid header",
            )),
        }
    }
    out
}

fn str_at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter().try_fold(v, |cur, k| cur.get(*k))?.as_str()
}

/// The `http.version` of a provider, checked against its base URL's scheme.
fn http_version(
    spec: &Value,
    base: Option<&str>,
    at: &DocPath,
    errors: &mut Vec<ConfigError>,
) -> HttpVersion {
    let version = str_at(spec, &["http", "version"])
        .and_then(HttpVersion::parse)
        .unwrap_or_default();
    let scheme = base.and_then(|b| b.split_once("://")).map_or("https", |(s, _)| s);
    let clash = match version {
        HttpVersion::H2 if scheme == "http" => Some("h2 needs an https base_url; use h2c for cleartext"),
        HttpVersion::H2c if scheme == "https" => Some("h2c needs an http base_url; use h2 over TLS"),
        _ => None,
    };
    if let Some(message) = clash {
        errors.push(ConfigError::new(
            &at.key("http").key("version"),
            DetailCode::Conflict,
            message,
        ));
    }
    version
}

/// Compiles a provider's `tls` block, reading its CA file.
fn compile_tls(
    spec: Option<&Value>,
    http: HttpVersion,
    at: &DocPath,
    errors: &mut Vec<ConfigError>,
) -> Option<TlsProfile> {
    let mut s = TlsSettings {
        http,
        ..TlsSettings::default()
    };
    if let Some(t) = spec {
        s.verify = t.get("verify").and_then(Value::as_bool).unwrap_or(true);
        s.server_name = t.get("server_name").and_then(Value::as_str).map(str::to_owned);
        s.min_tls13 = t.get("min_version").and_then(Value::as_str) == Some("1.3");
        if let Some(path) = t.get("ca_file").and_then(Value::as_str) {
            match std::fs::read(path) {
                Ok(pem) => s.ca_pem = Some(pem),
                Err(_) => {
                    errors.push(ConfigError::new(
                        &at.key("ca_file"),
                        DetailCode::InvalidFormat,
                        "the CA file cannot be read",
                    ));
                    return None;
                }
            }
        }
        let file = |k: &str| t.get(k).and_then(Value::as_str);
        if let (Some(cert), Some(key)) = (file("client_cert_file"), file("client_key_file")) {
            let read = |path: &str, field: &str, errors: &mut Vec<ConfigError>| match std::fs::read(path) {
                Ok(b) => Some(b),
                Err(_) => {
                    errors.push(ConfigError::new(
                        &at.key(field),
                        DetailCode::InvalidFormat,
                        "the file cannot be read",
                    ));
                    None
                }
            };
            let chain = read(cert, "client_cert_file", errors)?;
            let key = read(key, "client_key_file", errors)?;
            s.client_cert = Some(antirion_egress::tls::ClientCertPem {
                chain,
                key: antirion_core::Secret::new(String::from_utf8(key).unwrap_or_default()),
            });
        }
    }
    match TlsProfile::build(&s) {
        Ok(p) => Some(p),
        Err(e) => {
            let field = match e {
                antirion_egress::tls::TlsConfigError::ServerName => at.key("server_name"),
                antirion_egress::tls::TlsConfigError::CaBundle => at.key("ca_file"),
                antirion_egress::tls::TlsConfigError::ClientCert => at.key("client_cert_file"),
                antirion_egress::tls::TlsConfigError::ClientKey => at.key("client_key_file"),
                antirion_egress::tls::TlsConfigError::Build => at.clone(),
            };
            errors.push(ConfigError::new(&field, DetailCode::InvalidFormat, e.to_string()));
            None
        }
    }
}

fn required_headers(spec: &Value, kind: ProviderType) -> HeaderMap {
    let mut out = HeaderMap::new();
    let mut put = |name: &'static str, value: Option<&str>| {
        if let Some(v) = value.and_then(|v| HeaderValue::from_str(v).ok()) {
            out.insert(HeaderName::from_static(name), v);
        }
    };
    match kind {
        ProviderType::OpenAi => {
            put("openai-organization", str_at(spec, &["openai", "organization"]));
            put("openai-project", str_at(spec, &["openai", "project"]));
        }
        ProviderType::Vertex => put("x-goog-user-project", str_at(spec, &["gcp", "quota_project"])),
        _ => {}
    }
    out
}

fn operations(spec: &Value, at: &DocPath, errors: &mut Vec<ConfigError>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (class, op) in spec
        .get("operations")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let Some(path) = op.get("path").and_then(Value::as_str) else {
            continue;
        };
        if !path.starts_with('/') || path.starts_with("//") {
            errors.push(ConfigError::new(
                &at.key("operations").key(class).key("path"),
                DetailCode::InvalidFormat,
                "an operation path starts with one \"/\"",
            ));
            continue;
        }
        out.insert(class.clone(), path.to_owned());
    }
    out
}

fn azure(spec: &Value, base: Option<&str>, at: &DocPath, errors: &mut Vec<ConfigError>) -> Option<String> {
    let style = str_at(spec, &["azure", "api_style"]).unwrap_or("v1");
    let version = str_at(spec, &["azure", "api_version"]);
    if let Some(base) = base {
        let path = base.split_once("://").map_or("", |(_, rest)| {
            rest.find('/').and_then(|i| rest.get(i..)).unwrap_or("")
        });
        let path = path.split('?').next().unwrap_or_default().trim_end_matches('/');
        if path.ends_with("/openai") || path.ends_with("/openai/v1") {
            errors.push(ConfigError::new(
                &at.key("base_url"),
                DetailCode::Conflict,
                "the operation paths already contain /openai; use the resource root as base_url",
            ));
        }
    }
    match (style, version) {
        ("deployments", Some(v)) => Some(v.to_owned()),
        ("deployments", None) => {
            errors.push(ConfigError::new(
                &at.key("azure").key("api_version"),
                DetailCode::Required,
                "required when api_style is deployments",
            ));
            None
        }
        (_, Some(_)) => {
            errors.push(ConfigError::new(
                &at.key("azure").key("api_version"),
                DetailCode::Conflict,
                "only allowed when api_style is deployments",
            ));
            None
        }
        _ => None,
    }
}

fn provider(
    name: &str,
    spec: &Value,
    egress: &Arc<antirion_egress::EgressPolicy>,
    catalog: &Arc<antirion_catalog::Catalog>,
    errors: &mut Vec<ConfigError>,
) -> Option<ProviderRt> {
    let at = DocPath::root().key("providers").key(name);
    let Ok(p) = view::<ProviderView>(spec) else {
        errors.push(ConfigError::new(&at, DetailCode::TypeMismatch, "not a provider"));
        return None;
    };
    let Some(kind) = ProviderType::parse(&p.kind) else {
        let (code, message) = if ProviderType::unsupported(&p.kind) {
            (
                DetailCode::NotSupportedInThisBuild,
                format!("provider type {} is not supported in this build", p.kind),
            )
        } else {
            (DetailCode::InvalidEnum, "unknown provider type".to_owned())
        };
        errors.push(ConfigError::new(&at.key("type"), code, message));
        return None;
    };
    let (auth, keys) = compile_auth(&p, spec, kind, &at, errors);
    let byok = Byok::parse(str_at(spec, &["byok"])).unwrap_or_default();
    let key_header = matches!(auth.method(), "api_key" | "none").then(|| key_header(&p, kind));
    if byok.accepts() && key_header.is_none() {
        errors.push(ConfigError::new(
            &at.key("byok"),
            DetailCode::Conflict,
            "client provider keys need auth.method api_key or none",
        ));
    }
    let http = http_version(spec, p.base_url.as_deref(), &at, errors);
    let tls = compile_tls(spec.get("tls"), http, &at.key("tls"), errors)?;
    let headers = header_map(&p.headers, &at.key("headers"), errors);
    let operations = operations(spec, &at, errors);
    let mut secrets: Vec<String> = p.auth.api_key.iter().cloned().collect();
    secrets.extend(p.auth.api_keys.iter().map(|k| k.key.clone()));
    if let Some(a) = spec.get("auth") {
        for k in ["secret_access_key", "session_token", "client_secret"] {
            if let Some(s) = a.get(k).and_then(Value::as_str) {
                secrets.push(s.to_owned());
            }
        }
    }
    secrets.retain(|k| !k.is_empty());
    let region = str_at(spec, &["aws", "region"]).map(str::to_owned);
    if kind == ProviderType::Bedrock && region.is_none() {
        errors.push(ConfigError::new(
            &at.key("aws").key("region"),
            DetailCode::Required,
            "required for type bedrock",
        ));
    }
    let project = str_at(spec, &["gcp", "project"]).map(str::to_owned);
    if kind == ProviderType::Vertex && matches!(auth, UpstreamAuth::Gcp(_)) && project.is_none() {
        errors.push(ConfigError::new(
            &at.key("gcp").key("project"),
            DetailCode::Required,
            "required with auth.method gcp",
        ));
    }
    let azure_deployments = if kind == ProviderType::AzureOpenAi {
        azure(spec, p.base_url.as_deref(), &at, errors)
    } else {
        None
    };
    let anthropic = AnthropicSettings {
        version: str_at(spec, &["anthropic", "version"])
            .unwrap_or("2023-06-01")
            .to_owned(),
        beta: spec
            .get("anthropic")
            .and_then(|a| a.get("beta"))
            .and_then(Value::as_array)
            .map(|b| b.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default(),
    };
    Some(ProviderRt {
        name: name.to_owned(),
        kind,
        base_url: p.base_url.clone(),
        enabled: p.enabled,
        auth,
        keys,
        headers,
        required: required_headers(spec, kind),
        tls,
        egress: Arc::clone(egress),
        operations,
        secrets,
        anthropic,
        azure_deployments,
        region,
        project,
        location: str_at(spec, &["gcp", "location"]).unwrap_or("global").to_owned(),
        gemini_version: str_at(spec, &["gemini", "api_version"])
            .unwrap_or("v1beta")
            .to_owned(),
        params: Params::compile(spec.get("params"), &at.key("params"), errors),
        byok,
        key_header,
        catalog: Arc::clone(catalog),
    })
}

fn deployment(
    name: &str,
    spec: &Value,
    doc: &Value,
    table: &ProviderTable,
    errors: &mut Vec<ConfigError>,
) -> Option<DeploymentRt> {
    let at = DocPath::root().key("deployments").key(name);
    let Ok(d) = view::<DeploymentView>(spec) else {
        errors.push(ConfigError::new(
            &at,
            DetailCode::TypeMismatch,
            "not a deployment",
        ));
        return None;
    };
    let provider = table.providers.get(&d.provider).cloned()?;
    let headers = header_map(&d.headers, &at.key("headers"), errors);
    let provider_caps = doc
        .get("providers")
        .and_then(|p| p.get(&d.provider))
        .and_then(|p| p.get("capabilities"));
    let publisher = match str_at(spec, &["vertex", "publisher"]) {
        Some("anthropic") => Publisher::Anthropic,
        Some(_) => Publisher::Google,
        None => publisher_of(&d.model),
    };
    if provider.kind == ProviderType::Vertex
        && publisher == Publisher::Anthropic
        && matches!(provider.auth, UpstreamAuth::Header { .. })
    {
        errors.push(ConfigError::new(
            &at.key("vertex").key("publisher"),
            DetailCode::Conflict,
            "anthropic models need auth.method gcp",
        ));
    }
    let bedrock_api = match str_at(spec, &["bedrock", "api"]) {
        Some("converse") => BedrockApi::Converse,
        Some("invoke") => BedrockApi::Invoke,
        Some("openai") => BedrockApi::OpenAi,
        _ => BedrockApi::Auto,
    };
    let byok = Byok::parse(str_at(spec, &["byok"])).unwrap_or(provider.byok);
    if byok.accepts() && provider.key_header.is_none() {
        errors.push(ConfigError::new(
            &at.key("byok"),
            DetailCode::Conflict,
            "client provider keys need a provider with auth.method api_key or none",
        ));
    }
    let mut rt = DeploymentRt {
        name: name.to_owned(),
        provider: Arc::clone(&provider),
        model: d.model.clone(),
        base_model: str_at(spec, &["base_model"]).unwrap_or(&d.model).to_owned(),
        enabled: d.enabled,
        headers,
        capabilities: Capabilities::default(),
        region: str_at(spec, &["region"])
            .map(str::to_owned)
            .or_else(|| provider.region.clone()),
        location: str_at(spec, &["location"]).map_or_else(|| provider.location.clone(), str::to_owned),
        bedrock_api,
        publisher,
        default_max_tokens: spec
            .get("default_max_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(4096),
        params: Params::compile(spec.get("params"), &at.key("params"), errors),
        byok,
    };
    rt.resolve_capabilities(provider_caps, spec.get("capabilities"));
    if rt.provider.kind == ProviderType::Bedrock && bedrock_api == BedrockApi::Invoke && !rt.anthropic_model()
    {
        errors.push(ConfigError::new(
            &at.key("bedrock").key("api"),
            DetailCode::Conflict,
            "invoke is available for Anthropic models only; use converse",
        ));
    }
    Some(rt)
}

fn publisher_of(model: &str) -> Publisher {
    if model.starts_with("claude-") {
        Publisher::Anthropic
    } else {
        Publisher::Google
    }
}

impl DeploymentRt {
    /// A deployment made on the fly for a request that addresses `provider` directly.
    #[must_use]
    pub fn direct(
        provider: &Arc<ProviderRt>,
        name: &str,
        model: &str,
        provider_caps: Option<&Value>,
    ) -> DeploymentRt {
        let mut rt = DeploymentRt {
            name: name.to_owned(),
            provider: Arc::clone(provider),
            model: model.to_owned(),
            base_model: model.to_owned(),
            enabled: true,
            headers: HeaderMap::new(),
            capabilities: Capabilities::default(),
            region: provider.region.clone(),
            location: provider.location.clone(),
            bedrock_api: BedrockApi::Auto,
            publisher: publisher_of(model),
            default_max_tokens: 4096,
            params: Params::default(),
            byok: provider.byok,
        };
        rt.resolve_capabilities(provider_caps, None);
        rt
    }

    fn resolve_capabilities(&mut self, provider_caps: Option<&Value>, own: Option<&Value>) {
        let kind = self.provider.kind;
        let dialects: Vec<&'static str> = self
            .natives()
            .iter()
            .flat_map(|n| match n {
                crate::Native::OpenAi if kind == ProviderType::Bedrock => vec!["openai_chat"],
                crate::Native::OpenAi => kind.catalog().dialects.to_vec(),
                crate::Native::AnthropicMessages => vec!["anthropic_messages"],
                crate::Native::Gemini => vec!["gemini"],
                crate::Native::BedrockConverse => vec!["bedrock_converse"],
                crate::Native::BedrockInvoke => vec!["bedrock_invoke"],
                crate::Native::Cohere => vec!["cohere_chat"],
            })
            .collect();
        let anthropic_model = self.anthropic_model();
        let entry = if kind.generic() {
            self.provider
                .catalog
                .lookup_any_type(kind.as_str(), &self.base_model)
        } else {
            self.provider.catalog.lookup(kind.as_str(), &self.base_model)
        };
        self.capabilities = Capabilities::resolve(kind, provider_caps, own)
            .with_defaults(dialects, anthropic_model)
            .with_catalog(entry.map(|(e, _)| e));
    }
}

/// Compiles providers and deployments from a document whose secrets are resolved.
pub fn compile(doc: &Value) -> Result<ProviderTable, Vec<ConfigError>> {
    let mut errors = Vec::new();
    let mut table = ProviderTable::default();
    let egress = Arc::new(antirion_egress::config::compile_policy(doc).unwrap_or_default());
    let catalog = Arc::new(antirion_catalog::Catalog::from_doc(doc));
    for (name, spec) in doc
        .get("providers")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        if let Some(rt) = provider(name, spec, &egress, &catalog, &mut errors) {
            table.providers.insert(name.clone(), Arc::new(rt));
        }
    }
    for (name, spec) in doc
        .get("deployments")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        if let Some(rt) = deployment(name, spec, doc, &table, &mut errors) {
            table.deployments.insert(name.clone(), Arc::new(rt));
        }
    }
    if errors.is_empty() { Ok(table) } else { Err(errors) }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn doc() -> Value {
        json!({
            "providers": {"p": {"type": "openai", "base_url": "http://127.0.0.1:9/v1/", "auth": {"api_key": "sk-up"},
                                "headers": {"x-escaped": "a$b"}}},
            "deployments": {"d": {"provider": "p", "model": "up-model", "headers": {"x-dep": "1"}}}
        })
    }

    #[test]
    fn tls_blocks() {
        let mut d = doc();
        d["providers"]["p"]["tls"] = json!({"verify": false, "server_name": "up.example"});
        let t = compile(&d).unwrap();
        assert!(!t.providers["p"].tls.verify);
        d["providers"]["p"]["tls"] = json!({"ca_file": "/no/such/ca.pem"});
        let errors = compile(&d).unwrap_err();
        assert_eq!(errors[0].path, "providers.p.tls.ca_file");
    }

    #[test]
    fn credentials_are_validated_at_load() {
        let cases = [
            (
                json!({"type": "bedrock", "auth": {"method": "aws_sigv4"}}),
                "providers.p.aws.region",
            ),
            (
                json!({"type": "bedrock", "aws": {"region": "us-east-1"},
                       "auth": {"method": "aws_sigv4", "access_key_id": "AKID"}}),
                "providers.p.auth",
            ),
            (
                json!({"type": "vertex", "auth": {"method": "gcp", "service_account_json": "{not json"},
                       "gcp": {"project": "x"}}),
                "providers.p.auth.service_account_json",
            ),
            (
                json!({"type": "vertex", "auth": {"method": "gcp"}}),
                "providers.p.gcp.project",
            ),
            (
                json!({"type": "anthropic", "auth": {"method": "aws_sigv4"}}),
                "providers.p.auth.method",
            ),
        ];
        for (p, path) in cases {
            let errors = compile(&json!({"providers": {"p": p}})).unwrap_err();
            assert_eq!(errors[0].path, path);
        }
        let ok = compile(&json!({"providers": {"p": {"type": "bedrock", "aws": {"region": "us-east-1"}}}}))
            .unwrap();
        assert!(matches!(ok.providers["p"].auth, UpstreamAuth::Aws(None)));
    }

    #[test]
    fn deployment_dialects_follow_publisher_and_api() {
        let d = json!({
            "providers": {
                "v": {"type": "vertex", "auth": {"method": "api_key", "api_key": "k"}},
                "b": {"type": "bedrock", "aws": {"region": "us-east-1"}},
            },
            "deployments": {
                "g": {"provider": "v", "model": "gemini-x"},
                "c": {"provider": "b", "model": "anthropic.claude-x", "bedrock": {"api": "invoke"}},
                "l": {"provider": "b", "model": "meta.llama"},
            }
        });
        let t = compile(&d).unwrap();
        assert_eq!(t.deployments["g"].capabilities.dialects(), ["gemini"]);
        assert_eq!(
            t.deployments["c"].capabilities.dialects(),
            ["anthropic_messages", "bedrock_invoke"]
        );
        assert_eq!(
            t.deployments["l"].capabilities.dialects(),
            ["bedrock_converse", "bedrock_invoke"]
        );
        for d in t.deployments.values() {
            for dialect in [
                "gemini",
                "anthropic_messages",
                "bedrock_invoke",
                "bedrock_converse",
                "openai_chat",
            ] {
                assert_eq!(
                    d.capabilities.speaks(dialect),
                    d.capabilities.dialects().iter().any(|x| x == dialect)
                );
            }
        }
        let bad = json!({"providers": d["providers"].clone(),
                         "deployments": {"x": {"provider": "v", "model": "claude-3"}}});
        assert_eq!(
            compile(&bad).unwrap_err()[0].path,
            "deployments.x.vertex.publisher"
        );
    }
}
