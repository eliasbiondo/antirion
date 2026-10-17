//! Azure AD (Entra ID) access tokens for Azure OpenAI: client credentials, workload identity (a
//! federated token read from a file) and managed identity (the instance metadata service).

use antirion_egress::TlsProfile;
use bytes::Bytes;
use http::header::CONTENT_TYPE;
use http::{HeaderMap, HeaderName, HeaderValue, Method};
use serde_json::Value;

use super::http::{CredClient, form, form_encode, send, send_metadata};
use super::{CredError, Token};

pub(crate) const DEFAULT_AUTHORITY: &str = "https://login.microsoftonline.com";
pub(crate) const DEFAULT_SCOPE: &str = "https://cognitiveservices.azure.com/.default";
const IMDS_TOKEN: &str = "http://169.254.169.254/metadata/identity/oauth2/token";
const MI_RESOURCE: &str = "https://cognitiveservices.azure.com";
const CLIENT_ASSERTION_TYPE: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";
/// The environment variable naming the federated token file when `token_file` is not set.
const FEDERATED_TOKEN_FILE: &str = "AZURE_FEDERATED_TOKEN_FILE";

/// Where an Azure OpenAI provider's tokens come from.
pub enum AzureSource {
    ClientSecret {
        authority: String,
        tenant: String,
        client_id: String,
        secret: String,
        scope: String,
    },
    /// Workload identity: the client assertion is the federated token in `token_file`, or in the
    /// file `AZURE_FEDERATED_TOKEN_FILE` names.
    Workload {
        authority: String,
        tenant: String,
        client_id: String,
        token_file: Option<String>,
        scope: String,
    },
    /// Managed identity, optionally a user-assigned one, for `resource` (default the Azure OpenAI
    /// resource).
    Managed {
        client_id: Option<String>,
        resource: Option<String>,
    },
}

impl std::fmt::Debug for AzureSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            AzureSource::ClientSecret { .. } => "AzureSource::ClientSecret",
            AzureSource::Workload { .. } => "AzureSource::Workload",
            AzureSource::Managed { .. } => "AzureSource::Managed",
        })
    }
}

impl AzureSource {
    pub(crate) fn cache_key(&self) -> String {
        match self {
            AzureSource::ClientSecret {
                authority,
                tenant,
                client_id,
                secret,
                scope,
            } => {
                use std::hash::{Hash, Hasher};
                let mut h = std::hash::DefaultHasher::new();
                secret.hash(&mut h);
                format!(
                    "azure-cc:{authority}:{tenant}:{client_id}:{scope}:{:016x}",
                    h.finish()
                )
            }
            AzureSource::Workload {
                authority,
                tenant,
                client_id,
                token_file,
                scope,
            } => format!(
                "azure-wi:{authority}:{tenant}:{client_id}:{}:{scope}",
                token_file.as_deref().unwrap_or_default()
            ),
            AzureSource::Managed { client_id, resource } => format!(
                "azure-mi:{}:{}",
                client_id.as_deref().unwrap_or_default(),
                resource.as_deref().unwrap_or(MI_RESOURCE)
            ),
        }
    }
}

fn form_headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/x-www-form-urlencoded"),
    );
    h
}

/// `expires_in` is a number from the token endpoint and a string from the metadata service.
fn lifetime(v: &Value) -> i64 {
    match v.get("expires_in") {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    }
    .unwrap_or(3600)
    .max(1)
}

fn token_reply(status: u16, body: &[u8], issued: i64) -> Result<Token, CredError> {
    if !(200..300).contains(&status) {
        return Err(CredError::Rejected(status));
    }
    let v: Value = serde_json::from_slice(body).map_err(|_| CredError::Malformed)?;
    let token = v
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .ok_or(CredError::Malformed)?;
    Ok(Token {
        value: token.to_owned(),
        issued,
        expires: issued + lifetime(&v),
    })
}

fn token_url(authority: &str, tenant: &str) -> String {
    format!("{}/{}/oauth2/v2.0/token", authority.trim_end_matches('/'), tenant)
}

pub(crate) async fn fetch(
    client: &CredClient,
    tls: &TlsProfile,
    source: &AzureSource,
    issued: i64,
) -> Result<Token, CredError> {
    let (url, body, headers, method, metadata) = match source {
        AzureSource::ClientSecret {
            authority,
            tenant,
            client_id,
            secret,
            scope,
        } => (
            token_url(authority, tenant),
            form(&[
                ("grant_type", "client_credentials"),
                ("client_id", client_id),
                ("client_secret", secret),
                ("scope", scope),
            ]),
            form_headers(),
            Method::POST,
            false,
        ),
        AzureSource::Workload {
            authority,
            tenant,
            client_id,
            token_file,
            scope,
        } => {
            let path = token_file
                .clone()
                .or_else(|| std::env::var(FEDERATED_TOKEN_FILE).ok().filter(|p| !p.is_empty()))
                .ok_or(CredError::Unavailable)?;
            let assertion = tokio::fs::read_to_string(&path)
                .await
                .map_err(|_| CredError::Unavailable)?;
            let assertion = assertion.trim();
            if assertion.is_empty() {
                return Err(CredError::Unavailable);
            }
            (
                token_url(authority, tenant),
                form(&[
                    ("grant_type", "client_credentials"),
                    ("client_id", client_id),
                    ("client_assertion_type", CLIENT_ASSERTION_TYPE),
                    ("client_assertion", assertion),
                    ("scope", scope),
                ]),
                form_headers(),
                Method::POST,
                false,
            )
        }
        AzureSource::Managed { client_id, resource } => {
            let mut url = format!(
                "{IMDS_TOKEN}?api-version=2018-02-01&resource={}",
                form_encode(resource.as_deref().unwrap_or(MI_RESOURCE))
            );
            if let Some(id) = client_id {
                url.push_str("&client_id=");
                url.push_str(&form_encode(id));
            }
            let mut h = HeaderMap::new();
            h.insert(
                HeaderName::from_static("metadata"),
                HeaderValue::from_static("true"),
            );
            (url, Bytes::new(), h, Method::GET, true)
        }
    };
    let reply = if metadata {
        send_metadata(client, tls, method, &url, headers, body).await?
    } else {
        send(client, tls, method, &url, headers, body).await?
    };
    token_reply(reply.status, &reply.body, issued)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_replies_take_either_lifetime_form() {
        let t = token_reply(200, br#"{"access_token":"a","expires_in":"120"}"#, 10).unwrap();
        assert_eq!((t.value.as_str(), t.expires), ("a", 130));
        let t = token_reply(200, br#"{"access_token":"b","expires_in":60}"#, 0).unwrap();
        assert_eq!(t.expires, 60);
        assert_eq!(token_reply(401, b"{}", 0).unwrap_err(), CredError::Rejected(401));
        assert_eq!(
            token_reply(200, br#"{"token_type":"Bearer"}"#, 0).unwrap_err(),
            CredError::Malformed
        );
    }

    #[test]
    fn the_token_endpoint_is_under_the_tenant() {
        assert_eq!(
            token_url("https://login.example/", "t-1"),
            "https://login.example/t-1/oauth2/v2.0/token"
        );
    }

    #[test]
    fn cache_keys_differ_by_configuration() {
        let a = AzureSource::Managed {
            client_id: None,
            resource: None,
        };
        let b = AzureSource::Managed {
            client_id: Some("c".into()),
            resource: None,
        };
        let c = AzureSource::Managed {
            client_id: None,
            resource: Some("https://vault.azure.net".into()),
        };
        assert_ne!(a.cache_key(), b.cache_key());
        assert_ne!(a.cache_key(), c.cache_key());
    }
}
