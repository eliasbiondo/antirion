//! `security.secrets.stores`, compiled from a document whose own references to the environment and
//! files are resolved here (a store's settings never name a store).

use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use antirion_config::scalar::parse_duration;
use antirion_config::secrets::{SecretSource, expand_document, reads_host};
use antirion_config::{ConfigError, DetailCode, DocPath};
use antirion_core::Secret;
use antirion_providers::{AssumeRole, AwsCredentials, AzureSource, GcpSource};
use serde_json::Value;

const DEFAULT_TTL: Duration = Duration::from_secs(300);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const KUBERNETES_TOKEN: &str = "/var/run/secrets/kubernetes.io/serviceaccount/token";
/// Where the Azure workload identity webhook mounts the federated token.
const AZURE_TOKEN: &str = "/var/run/secrets/azure/tokens/azure-identity-token";
/// The token files a store's settings may name when they arrive through the Admin API.
pub const API_TOKEN_FILES: &[&str] = &[KUBERNETES_TOKEN, AZURE_TOKEN];
const STORES: &str = "/security/secrets/stores";
const GCP_ENDPOINT: &str = "https://secretmanager.googleapis.com";
const GCP_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
const AZURE_AUTHORITY: &str = "https://login.microsoftonline.com";
const KEY_VAULT_RESOURCE: &str = "https://vault.azure.net";

/// How Vault is logged in to.
pub(crate) enum VaultAuth {
    Token(Secret),
    AppRole {
        mount: String,
        role_id: String,
        secret_id: Secret,
    },
    Kubernetes {
        mount: String,
        role: String,
        jwt_file: String,
    },
}

pub(crate) struct VaultSettings {
    pub(crate) address: String,
    pub(crate) namespace: Option<String>,
    pub(crate) auth: VaultAuth,
}

pub(crate) struct AwsSettings {
    pub(crate) region: Option<String>,
    pub(crate) endpoint: Option<String>,
    pub(crate) credentials: Option<AwsCredentials>,
    pub(crate) assume_role: Option<AssumeRole>,
}

pub(crate) struct GcpSettings {
    pub(crate) endpoint: String,
    pub(crate) source: Arc<GcpSource>,
}

pub(crate) struct AzureSettings {
    pub(crate) vault_suffix: String,
    pub(crate) endpoint: Option<String>,
    pub(crate) source: Arc<AzureSource>,
}

/// What one store reads with.
pub(crate) enum Kind {
    Vault(VaultSettings),
    Aws(AwsSettings),
    Gcp(GcpSettings),
    Azure(AzureSettings),
}

/// One configured store.
pub(crate) struct StoreSettings {
    /// The configuration key: `vault`, `aws_sm`, `gcp_sm` or `azure_kv`.
    pub(crate) name: &'static str,
    pub(crate) ttl: Duration,
    pub(crate) timeout: Duration,
    /// Identifies the settings, so values fetched with others are fetched again.
    pub(crate) fingerprint: u64,
    pub(crate) kind: Kind,
}

impl std::fmt::Debug for StoreSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreSettings")
            .field("name", &self.name)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

/// The configuration key of the store a reference scheme names.
#[must_use]
pub(crate) fn store_of(scheme: &str) -> Option<&'static str> {
    match scheme {
        "vault" => Some("vault"),
        "aws-sm" => Some("aws_sm"),
        "gcp-sm" => Some("gcp_sm"),
        "azure-kv" => Some("azure_kv"),
        _ => None,
    }
}

fn text(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn duration(v: &Value, k: &str, default: Duration) -> Duration {
    v.get(k)
        .and_then(Value::as_str)
        .and_then(parse_duration)
        .unwrap_or(default)
}

fn vault(v: &Value) -> Option<Kind> {
    let auth = v.get("auth").cloned().unwrap_or(Value::Null);
    let method = auth.get("method").and_then(Value::as_str).unwrap_or("token");
    let mount = text(&auth, "mount").unwrap_or_else(|| method.to_owned());
    let auth = match method {
        "approle" => VaultAuth::AppRole {
            mount,
            role_id: text(&auth, "role_id")?,
            secret_id: Secret::new(text(&auth, "secret_id")?),
        },
        "kubernetes" => VaultAuth::Kubernetes {
            mount,
            role: text(&auth, "role")?,
            jwt_file: text(&auth, "jwt_file").unwrap_or_else(|| KUBERNETES_TOKEN.to_owned()),
        },
        _ => VaultAuth::Token(Secret::new(text(&auth, "token")?)),
    };
    Some(Kind::Vault(VaultSettings {
        address: text(v, "address")?.trim_end_matches('/').to_owned(),
        namespace: text(v, "namespace"),
        auth,
    }))
}

fn aws(v: &Value) -> Kind {
    let credentials = match (text(v, "access_key_id"), text(v, "secret_access_key")) {
        (Some(id), Some(secret)) => Some(AwsCredentials {
            access_key_id: id,
            secret_access_key: Secret::new(secret),
            session_token: text(v, "session_token").map(Secret::new),
        }),
        _ => None,
    };
    let assume_role = v.get("assume_role").filter(|r| r.is_object()).and_then(|r| {
        Some(AssumeRole {
            role_arn: text(r, "role_arn")?,
            external_id: text(r, "external_id"),
            session_name: text(r, "session_name").unwrap_or_else(|| "antirion".to_owned()),
            duration_secs: duration(r, "duration", Duration::from_secs(3600)).as_secs(),
            sts_endpoint: text(r, "sts_endpoint"),
        })
    });
    Kind::Aws(AwsSettings {
        region: text(v, "region"),
        endpoint: text(v, "endpoint").map(|e| e.trim_end_matches('/').to_owned()),
        credentials,
        assume_role,
    })
}

fn gcp(v: &Value, fingerprint: u64) -> Option<Kind> {
    let source = match v.get("service_account_json") {
        Some(Value::String(raw)) => GcpSource::ServiceAccount {
            account: Arc::new(antirion_providers::creds::ServiceAccount::parse(raw).ok()?),
            scopes: GCP_SCOPE.to_owned(),
            fingerprint,
        },
        _ => GcpSource::Adc {
            scopes: GCP_SCOPE.to_owned(),
        },
    };
    Some(Kind::Gcp(GcpSettings {
        endpoint: text(v, "endpoint")
            .unwrap_or_else(|| GCP_ENDPOINT.to_owned())
            .trim_end_matches('/')
            .to_owned(),
        source: Arc::new(source),
    }))
}

fn azure(v: &Value) -> Option<Kind> {
    let scope = format!("{KEY_VAULT_RESOURCE}/.default");
    let source = if v.get("managed_identity").and_then(Value::as_bool) == Some(true) {
        AzureSource::Managed {
            client_id: text(v, "client_id"),
            resource: Some(KEY_VAULT_RESOURCE.to_owned()),
        }
    } else {
        let authority = text(v, "authority_host").unwrap_or_else(|| AZURE_AUTHORITY.to_owned());
        let (tenant, client_id) = (text(v, "tenant_id")?, text(v, "client_id")?);
        match text(v, "client_secret") {
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
                token_file: text(v, "token_file"),
                scope,
            },
        }
    };
    Some(Kind::Azure(AzureSettings {
        vault_suffix: text(v, "vault_suffix").unwrap_or_else(|| "vault.azure.net".to_owned()),
        endpoint: text(v, "endpoint").map(|e| e.trim_end_matches('/').to_owned()),
        source: Arc::new(source),
    }))
}

/// What a write through the Admin API may not put in `security.secrets.stores`: a value that reads
/// the node's environment or a file, and a token file other than [`API_TOKEN_FILES`]. Store
/// settings are sent to the store, so such a value would carry what the node holds to wherever the
/// writer points it; only the configuration file may use them. Values `previous` already holds at
/// the same place are kept as they are.
#[must_use]
pub fn check_api_write(previous: &Value, candidate: &Value) -> Vec<ConfigError> {
    fn walk(v: &Value, pointer: &mut String, at: &DocPath, previous: &Value, out: &mut Vec<ConfigError>) {
        match v {
            Value::String(s) => {
                if previous.pointer(pointer).and_then(Value::as_str) == Some(s.as_str()) {
                    return;
                }
                let field = pointer.rsplit('/').next().unwrap_or_default();
                if reads_host(s) {
                    out.push(ConfigError::new(
                        at,
                        DetailCode::InvalidFormat,
                        "secret store settings written through the Admin API cannot read the gateway's environment \
                         or files; set this value in the configuration file",
                    ));
                } else if matches!(field, "jwt_file" | "token_file") && !API_TOKEN_FILES.contains(&s.as_str())
                {
                    out.push(ConfigError::new(
                        at,
                        DetailCode::InvalidFormat,
                        format!(
                            "through the Admin API a token file is one of {}; set another path in the configuration file",
                            API_TOKEN_FILES.join(", ")
                        ),
                    ));
                }
            }
            Value::Object(map) => {
                for (k, child) in map {
                    let len = pointer.len();
                    pointer.push('/');
                    pointer.push_str(&k.replace('~', "~0").replace('/', "~1"));
                    walk(child, pointer, &at.key(k), previous, out);
                    pointer.truncate(len);
                }
            }
            Value::Array(items) => {
                for (i, child) in items.iter().enumerate() {
                    let len = pointer.len();
                    pointer.push('/');
                    pointer.push_str(&i.to_string());
                    walk(child, pointer, &at.index(i), previous, out);
                    pointer.truncate(len);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    if let Some(stores) = candidate.pointer(STORES) {
        let at = DocPath::root().key("security").key("secrets").key("stores");
        walk(stores, &mut STORES.to_owned(), &at, previous, &mut out);
    }
    out
}

/// The stores of `doc`, with their own references resolved through `env_and_files`. A store whose
/// settings cannot be resolved or are incomplete is left out: its references then fail with a
/// reason, and validation names the field.
#[must_use]
pub(crate) fn compile(doc: &Value, env_and_files: &dyn SecretSource) -> Vec<Arc<StoreSettings>> {
    let Some(raw) = doc.pointer("/security/secrets/stores").filter(|s| s.is_object()) else {
        return Vec::new();
    };
    let Ok(stores) = expand_document(raw, env_and_files) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for name in ["vault", "aws_sm", "gcp_sm", "azure_kv"] {
        let Some(v) = stores.get(name).filter(|v| v.is_object()) else {
            continue;
        };
        let mut h = std::hash::DefaultHasher::new();
        v.to_string().hash(&mut h);
        let fingerprint = h.finish();
        let kind = match name {
            "vault" => vault(v),
            "aws_sm" => Some(aws(v)),
            "gcp_sm" => gcp(v, fingerprint),
            _ => azure(v),
        };
        if let Some(kind) = kind {
            out.push(Arc::new(StoreSettings {
                name,
                ttl: duration(v, "ttl", DEFAULT_TTL),
                timeout: duration(v, "timeout", DEFAULT_TIMEOUT),
                fingerprint,
                kind,
            }));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[derive(Debug)]
    struct Env;

    impl SecretSource for Env {
        fn env(&self, name: &str) -> Option<String> {
            (name == "VAULT_TOKEN").then(|| "hvs.token".to_owned())
        }
        fn file(&self, _: &str) -> std::io::Result<String> {
            Err(std::io::Error::other("no file"))
        }
    }

    #[test]
    fn stores_compile_with_their_references_resolved() {
        let doc = json!({"security": {"secrets": {"stores": {
            "vault": {"address": "https://vault.internal:8200/", "auth": {"token": "${env:VAULT_TOKEN}"}, "ttl": "2s"},
            "aws_sm": {"region": "eu-west-1"},
            "azure_kv": {"managed_identity": true}
        }}}});
        let stores = compile(&doc, &Env);
        assert_eq!(stores.len(), 3);
        let vault = &stores[0];
        assert_eq!((vault.name, vault.ttl), ("vault", Duration::from_secs(2)));
        let Kind::Vault(v) = &vault.kind else {
            panic!("not vault")
        };
        assert_eq!(v.address, "https://vault.internal:8200");
        assert!(matches!(&v.auth, VaultAuth::Token(t) if t.expose() == "hvs.token"));
        assert_eq!(stores[1].timeout, DEFAULT_TIMEOUT);
        assert!(matches!(&stores[2].kind, Kind::Azure(a) if a.vault_suffix == "vault.azure.net"));
    }

    #[test]
    fn the_admin_api_cannot_point_store_settings_at_the_nodes_environment_or_files() {
        let previous = json!({"security": {"secrets": {"stores": {
            "vault": {"address": "https://v", "auth": {"token": "${env:VAULT_TOKEN}"}}}}}});
        assert!(
            check_api_write(&previous, &previous).is_empty(),
            "what the file set is kept"
        );
        let leak = json!({"security": {"secrets": {"stores": {
            "vault": {"address": "http://attacker", "auth": {"token": "${env:ANTIRION_BOOTSTRAP_TOKEN}"}}}}}});
        let errors = check_api_write(&previous, &leak);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].path, "security.secrets.stores.vault.auth.token");
        let file = json!({"security": {"secrets": {"stores": {
            "vault": {"address": "https://v", "auth": {"method": "kubernetes", "role": "r", "jwt_file": "/etc/shadow"}},
            "azure_kv": {"tenant_id": "t", "client_id": "c", "token_file": AZURE_TOKEN}}}}});
        let errors = check_api_write(&json!({}), &file);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].path, "security.secrets.stores.vault.auth.jwt_file");
        let plain = json!({"security": {"secrets": {"stores": {
            "vault": {"address": "https://v", "auth": {"token": "hvs.literal"}}}}}});
        assert!(check_api_write(&json!({}), &plain).is_empty());
    }

    #[test]
    fn unresolvable_settings_leave_the_stores_out() {
        let doc = json!({"security": {"secrets": {"stores": {
            "vault": {"address": "https://v", "auth": {"token": "${env:MISSING}"}}}}}});
        assert!(compile(&doc, &Env).is_empty());
        let other = json!({"security": {"secrets": {"stores": {
            "vault": {"address": "https://v", "auth": {"token": "a"}}}}}});
        let changed = json!({"security": {"secrets": {"stores": {
            "vault": {"address": "https://w", "auth": {"token": "a"}}}}}});
        assert_ne!(
            compile(&other, &Env)[0].fingerprint,
            compile(&changed, &Env)[0].fingerprint
        );
        assert_eq!(store_of("aws-sm"), Some("aws_sm"));
    }
}
