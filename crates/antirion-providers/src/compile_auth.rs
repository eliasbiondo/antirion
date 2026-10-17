//! The `auth` block of a provider: API keys per type, AWS static credentials or the default chain
//! (optionally assuming a role), GCP service accounts or application default credentials, and Azure
//! AD client credentials, workload identity or managed identity.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use antirion_config::views::ProviderView;
use antirion_config::{ConfigError, DetailCode, DocPath};
use http::header::AUTHORIZATION;
use http::{HeaderName, HeaderValue};
use serde_json::Value;

use crate::creds::{
    AZURE_AUTHORITY, AZURE_SCOPE, AssumeRole, AzureSource, DEFAULT_SCOPE, GcpSource, ServiceAccount,
};
use crate::sigv4::AwsCredentials;
use crate::{ProviderKey, ProviderType, SINGLE_KEY, UpstreamAuth};

/// The header an API key of this provider travels in, and the text before the key.
pub(crate) fn key_header(p: &ProviderView, kind: ProviderType) -> (HeaderName, String) {
    match kind {
        ProviderType::Anthropic | ProviderType::AnthropicCompatible => {
            (HeaderName::from_static("x-api-key"), String::new())
        }
        ProviderType::AzureOpenAi => (HeaderName::from_static("api-key"), String::new()),
        ProviderType::Gemini | ProviderType::Vertex => {
            (HeaderName::from_static("x-goog-api-key"), String::new())
        }
        k if k.compatible() => (
            p.auth
                .header
                .as_deref()
                .and_then(|h| HeaderName::from_bytes(h.as_bytes()).ok())
                .unwrap_or(AUTHORIZATION),
            p.auth.prefix.clone().unwrap_or_else(|| "Bearer ".to_owned()),
        ),
        _ => (AUTHORIZATION, "Bearer ".to_owned()),
    }
}

fn api_key_auth(
    p: &ProviderView,
    kind: ProviderType,
    at: &DocPath,
    errors: &mut Vec<ConfigError>,
) -> (UpstreamAuth, Vec<ProviderKey>) {
    if p.auth.api_key.is_some() && !p.auth.api_keys.is_empty() {
        errors.push(ConfigError::new(
            &at.key("auth"),
            DetailCode::Conflict,
            "set exactly one of api_key, api_keys",
        ));
    }
    let mut listed: Vec<(String, u32, String)> = p
        .auth
        .api_keys
        .iter()
        .map(|k| {
            (
                k.name.clone(),
                u32::try_from(k.weight).unwrap_or(u32::MAX),
                k.key.clone(),
            )
        })
        .collect();
    if let Some(single) = &p.auth.api_key {
        listed.insert(0, (SINGLE_KEY.to_owned(), 1, single.clone()));
    }
    let (name, prefix) = key_header(p, kind);
    let mut keys = Vec::new();
    for (i, (key_name, weight, key)) in listed.into_iter().enumerate() {
        match HeaderValue::from_str(&format!("{prefix}{key}")) {
            Ok(mut value) => {
                value.set_sensitive(true);
                keys.push(ProviderKey {
                    name: key_name,
                    weight,
                    value,
                });
            }
            Err(_) => {
                let field = if p.auth.api_key.is_some() && i == 0 {
                    at.key("auth").key("api_key")
                } else {
                    at.key("auth").key("api_keys")
                };
                errors.push(ConfigError::new(
                    &field,
                    DetailCode::InvalidFormat,
                    "the key cannot be sent in a header",
                ));
            }
        }
    }
    let auth = match keys.iter().find(|k| k.weight > 0) {
        Some(k) => UpstreamAuth::Header {
            name,
            value: k.value.clone(),
        },
        None => UpstreamAuth::None,
    };
    (auth, keys)
}

fn assume_role(a: &Value, at: &DocPath, errors: &mut Vec<ConfigError>) -> Option<AssumeRole> {
    let text = |k: &str| a.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
    let duration = match text("duration") {
        None => 3600,
        Some(d) => match antirion_config::scalar::parse_duration(d) {
            Some(d) if (900..=43_200).contains(&d.as_secs()) => d.as_secs(),
            _ => {
                errors.push(ConfigError::new(
                    &at.key("duration"),
                    DetailCode::OutOfRange,
                    "between 15m and 12h",
                ));
                3600
            }
        },
    };
    let Some(role_arn) = text("role_arn") else {
        errors.push(ConfigError::new(
            &at.key("role_arn"),
            DetailCode::Required,
            "required in assume_role",
        ));
        return None;
    };
    Some(AssumeRole {
        role_arn: role_arn.to_owned(),
        external_id: text("external_id").map(str::to_owned),
        session_name: text("session_name").unwrap_or("antirion").to_owned(),
        duration_secs: duration,
        sts_endpoint: text("sts_endpoint").map(str::to_owned),
    })
}

fn aws_auth(auth: Option<&Value>, at: &DocPath, errors: &mut Vec<ConfigError>) -> UpstreamAuth {
    let get = |k: &str| {
        auth.and_then(|a| a.get(k))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let base = match (get("access_key_id"), get("secret_access_key")) {
        (Some(id), Some(secret)) => Some(AwsCredentials {
            access_key_id: id,
            secret_access_key: antirion_core::Secret::new(secret),
            session_token: get("session_token").map(antirion_core::Secret::new),
        }),
        (None, None) => None,
        _ => {
            errors.push(ConfigError::new(
                &at.key("auth"),
                DetailCode::Conflict,
                "AWS static credentials need both access_key_id and secret_access_key",
            ));
            None
        }
    };
    match auth.and_then(|a| a.get("assume_role")).filter(|r| r.is_object()) {
        Some(r) => match assume_role(r, &at.key("auth").key("assume_role"), errors) {
            Some(role) => UpstreamAuth::AwsAssumed {
                base,
                role: Arc::new(role),
            },
            None => UpstreamAuth::Aws(base),
        },
        None => UpstreamAuth::Aws(base),
    }
}

/// `auth.method: azure_ad`: managed identity when `managed_identity` is true, client credentials
/// with a `client_secret`, else workload identity with the federated token file.
fn azure_auth(auth: Option<&Value>, at: &DocPath, errors: &mut Vec<ConfigError>) -> UpstreamAuth {
    let text = |k: &str| {
        auth.and_then(|a| a.get(k))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let client_id = text("client_id");
    if auth
        .and_then(|a| a.get("managed_identity"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        return UpstreamAuth::Azure(Arc::new(AzureSource::Managed {
            client_id,
            resource: None,
        }));
    }
    let mut need = |k: &str, v: Option<String>| {
        v.or_else(|| {
            errors.push(ConfigError::new(
                &at.key("auth").key(k),
                DetailCode::Required,
                "required for azure_ad client credentials and workload identity",
            ));
            None
        })
    };
    let (Some(tenant), Some(client_id)) =
        (need("tenant_id", text("tenant_id")), need("client_id", client_id))
    else {
        return UpstreamAuth::Unsupported("azure_ad".to_owned());
    };
    let authority = text("authority_host").unwrap_or_else(|| AZURE_AUTHORITY.to_owned());
    let scope = text("scope").unwrap_or_else(|| AZURE_SCOPE.to_owned());
    let source = match text("client_secret") {
        Some(secret) => AzureSource::ClientSecret {
            authority,
            tenant,
            client_id,
            secret,
            scope,
        },
        None => AzureSource::Workload {
            authority,
            tenant,
            client_id,
            token_file: text("token_file"),
            scope,
        },
    };
    UpstreamAuth::Azure(Arc::new(source))
}

fn gcp_auth(auth: Option<&Value>, at: &DocPath, errors: &mut Vec<ConfigError>) -> UpstreamAuth {
    let scopes = auth
        .and_then(|a| a.get("scopes"))
        .and_then(Value::as_array)
        .map(|s| s.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_SCOPE.to_owned());
    let Some(raw) = auth.and_then(|a| a.get("service_account_json")) else {
        return UpstreamAuth::Gcp(Arc::new(GcpSource::Adc { scopes }));
    };
    let text = match raw {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    match ServiceAccount::parse(&text) {
        Ok(account) => {
            let mut h = std::hash::DefaultHasher::new();
            text.hash(&mut h);
            UpstreamAuth::Gcp(Arc::new(GcpSource::ServiceAccount {
                account: Arc::new(account),
                scopes,
                fingerprint: h.finish(),
            }))
        }
        Err(e) => {
            errors.push(ConfigError::new(
                &at.key("auth").key("service_account_json"),
                DetailCode::Conflict,
                e.to_string(),
            ));
            UpstreamAuth::Unsupported("gcp".to_owned())
        }
    }
}

pub(crate) fn compile_auth(
    p: &ProviderView,
    spec: &Value,
    kind: ProviderType,
    at: &DocPath,
    errors: &mut Vec<ConfigError>,
) -> (UpstreamAuth, Vec<ProviderKey>) {
    let method = p.auth.method.clone().unwrap_or_else(|| match kind {
        ProviderType::Bedrock => "aws_sigv4".to_owned(),
        ProviderType::Vertex => "gcp".to_owned(),
        _ => "api_key".to_owned(),
    });
    if !kind.catalog().auth_methods.contains(&method.as_str()) {
        errors.push(ConfigError::new(
            &at.key("auth").key("method"),
            DetailCode::Conflict,
            format!("{method} is not valid for type {}", kind.as_str()),
        ));
        return (UpstreamAuth::Unsupported(method), Vec::new());
    }
    let auth = match method.as_str() {
        "none" => UpstreamAuth::None,
        "api_key" => return api_key_auth(p, kind, at, errors),
        "aws_sigv4" => aws_auth(spec.get("auth"), at, errors),
        "gcp" => gcp_auth(spec.get("auth"), at, errors),
        "azure_ad" => azure_auth(spec.get("auth"), at, errors),
        other => UpstreamAuth::Unsupported(other.to_owned()),
    };
    (auth, Vec::new())
}
