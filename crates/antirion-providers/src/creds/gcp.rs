//! GCP access tokens: the service-account JWT-bearer flow and the application default credentials
//! (credentials file, gcloud's well-known file, the metadata server).

use std::sync::Arc;

use antirion_egress::TlsProfile;
use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use bytes::Bytes;
use http::header::CONTENT_TYPE;
use http::{HeaderMap, HeaderName, HeaderValue, Method};
use serde_json::{Value, json};

use super::http::{CredClient, form, send, send_metadata};
use super::{CredError, Token};

pub(crate) const DEFAULT_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
const JWT_BEARER: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";
const GOOGLE_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";

/// A parsed service-account key.
pub struct ServiceAccount {
    client_email: String,
    token_uri: String,
    key: RsaKeyPair,
}

impl std::fmt::Debug for ServiceAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceAccount")
            .field("client_email", &self.client_email)
            .finish_non_exhaustive()
    }
}

/// Why a service-account document is not usable.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ServiceAccountError {
    #[error("the service-account JSON does not parse")]
    Json,
    #[error("the service-account JSON lacks {0}")]
    Missing(&'static str),
    #[error("the service-account private key does not parse")]
    Key,
}

fn pem_der(pem: &str) -> Option<Vec<u8>> {
    let body: String = pem
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("-----") && !l.is_empty())
        .collect();
    STANDARD.decode(body).ok()
}

impl ServiceAccount {
    /// Parses a service-account key document.
    pub fn parse(raw: &str) -> Result<ServiceAccount, ServiceAccountError> {
        let v: Value = serde_json::from_str(raw).map_err(|_| ServiceAccountError::Json)?;
        Self::from_value(&v)
    }

    fn from_value(v: &Value) -> Result<ServiceAccount, ServiceAccountError> {
        let field = |name: &'static str| {
            v.get(name)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .ok_or(ServiceAccountError::Missing(name))
        };
        let client_email = field("client_email")?;
        let private_key = field("private_key")?;
        let token_uri = field("token_uri")?;
        let der = pem_der(&private_key).ok_or(ServiceAccountError::Key)?;
        let key = RsaKeyPair::from_pkcs8(&der)
            .or_else(|_| RsaKeyPair::from_der(&der))
            .map_err(|_| ServiceAccountError::Key)?;
        Ok(ServiceAccount {
            client_email,
            token_uri,
            key,
        })
    }

    /// The signed RS256 assertion for `scopes`, issued at `now` (Unix seconds).
    fn assertion(&self, scopes: &str, now: i64) -> Result<String, CredError> {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let claims = json!({
            "iss": self.client_email,
            "scope": scopes,
            "aud": self.token_uri,
            "iat": now,
            "exp": now + 3600,
        });
        let claims = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap_or_default());
        let input = format!("{header}.{claims}");
        let mut sig = vec![0u8; self.key.public_modulus_len()];
        self.key
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                input.as_bytes(),
                &mut sig,
            )
            .map_err(|_| CredError::Signing)?;
        Ok(format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig)))
    }
}

/// Where a Vertex provider's tokens come from.
#[derive(Debug)]
pub enum GcpSource {
    ServiceAccount {
        account: Arc<ServiceAccount>,
        scopes: String,
        /// Identifies the configuration in the token cache.
        fingerprint: u64,
    },
    /// The application default credentials.
    Adc { scopes: String },
}

impl GcpSource {
    pub(crate) fn cache_key(&self) -> String {
        match self {
            GcpSource::ServiceAccount {
                fingerprint, scopes, ..
            } => format!("gcp-sa:{fingerprint:016x}:{scopes}"),
            GcpSource::Adc { scopes } => format!("gcp-adc:{scopes}"),
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

/// Reads an OAuth token response; `issued` is the cache clock's now (Unix seconds).
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
    let lifetime = v.get("expires_in").and_then(Value::as_i64).unwrap_or(3600).max(1);
    Ok(Token {
        value: token.to_owned(),
        issued,
        expires: issued + lifetime,
    })
}

async fn service_account(
    client: &CredClient,
    tls: &TlsProfile,
    account: &ServiceAccount,
    scopes: &str,
    issued: i64,
) -> Result<Token, CredError> {
    let assertion = account.assertion(scopes, super::unix_now())?;
    let body = form(&[("grant_type", JWT_BEARER), ("assertion", &assertion)]);
    let reply = send(
        client,
        tls,
        Method::POST,
        &account.token_uri,
        form_headers(),
        body,
    )
    .await?;
    token_reply(reply.status, &reply.body, issued)
}

async fn authorized_user(
    client: &CredClient,
    tls: &TlsProfile,
    v: &Value,
    issued: i64,
) -> Result<Token, CredError> {
    let field = |n: &str| v.get(n).and_then(Value::as_str).unwrap_or_default().to_owned();
    let uri = v
        .get("token_uri")
        .and_then(Value::as_str)
        .unwrap_or(GOOGLE_TOKEN_URI)
        .to_owned();
    let body = form(&[
        ("grant_type", "refresh_token"),
        ("client_id", &field("client_id")),
        ("client_secret", &field("client_secret")),
        ("refresh_token", &field("refresh_token")),
    ]);
    let reply = send(client, tls, Method::POST, &uri, form_headers(), body).await?;
    token_reply(reply.status, &reply.body, issued)
}

async fn metadata(client: &CredClient, tls: &TlsProfile, issued: i64) -> Result<Token, CredError> {
    let host = std::env::var("GCE_METADATA_HOST")
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "metadata.google.internal".to_owned());
    let url = format!("http://{host}/computeMetadata/v1/instance/service-accounts/default/token");
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("metadata-flavor"),
        HeaderValue::from_static("Google"),
    );
    let reply = send_metadata(client, tls, Method::GET, &url, headers, Bytes::new()).await?;
    token_reply(reply.status, &reply.body, issued)
}

const TOKEN_EXCHANGE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const ACCESS_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:access_token";

/// The subject token of an `external_account` document: read from `credential_source` (a file or a
/// URL), as plain text or from a field of a JSON reply.
async fn subject_token(client: &CredClient, tls: &TlsProfile, source: &Value) -> Result<String, CredError> {
    let raw = if let Some(path) = source.get("file").and_then(Value::as_str) {
        tokio::fs::read_to_string(path)
            .await
            .map_err(|_| CredError::Unavailable)?
    } else if let Some(url) = source.get("url").and_then(Value::as_str) {
        let mut headers = HeaderMap::new();
        for (k, v) in source
            .get("headers")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            let (Ok(name), Some(Ok(value))) = (
                HeaderName::try_from(k.as_str()),
                v.as_str().map(HeaderValue::from_str),
            ) else {
                return Err(CredError::Malformed);
            };
            headers.insert(name, value);
        }
        let reply = send(client, tls, Method::GET, url, headers, Bytes::new()).await?;
        if !(200..300).contains(&reply.status) {
            return Err(CredError::Rejected(reply.status));
        }
        String::from_utf8(reply.body.to_vec()).map_err(|_| CredError::Malformed)?
    } else {
        return Err(CredError::Unsupported);
    };
    let format = source.get("format");
    let token = match format.and_then(|f| f.get("type")).and_then(Value::as_str) {
        None | Some("text") => raw.trim().to_owned(),
        Some("json") => {
            let field = format
                .and_then(|f| f.get("subject_token_field_name"))
                .and_then(Value::as_str)
                .ok_or(CredError::Malformed)?;
            let v: Value = serde_json::from_str(&raw).map_err(|_| CredError::Malformed)?;
            v.get(field)
                .and_then(Value::as_str)
                .ok_or(CredError::Malformed)?
                .to_owned()
        }
        Some(_) => return Err(CredError::Unsupported),
    };
    if token.is_empty() {
        return Err(CredError::Malformed);
    }
    Ok(token)
}

/// Workload identity federation: the subject token exchanged at `token_url`, then, when the document
/// names one, a service account impersonated with the federated token.
async fn external_account(
    client: &CredClient,
    tls: &TlsProfile,
    v: &Value,
    scopes: &str,
    issued: i64,
) -> Result<Token, CredError> {
    let field = |n: &str| v.get(n).and_then(Value::as_str).ok_or(CredError::Malformed);
    let source = v.get("credential_source").ok_or(CredError::Malformed)?;
    let subject = subject_token(client, tls, source).await?;
    let body = form(&[
        ("grant_type", TOKEN_EXCHANGE),
        ("audience", field("audience")?),
        ("scope", scopes),
        ("requested_token_type", ACCESS_TOKEN_TYPE),
        ("subject_token_type", field("subject_token_type")?),
        ("subject_token", &subject),
    ]);
    let reply = send(
        client,
        tls,
        Method::POST,
        field("token_url")?,
        form_headers(),
        body,
    )
    .await?;
    let federated = token_reply(reply.status, &reply.body, issued)?;
    let Some(url) = v.get("service_account_impersonation_url").and_then(Value::as_str) else {
        return Ok(federated);
    };
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let mut auth =
        HeaderValue::from_str(&format!("Bearer {}", federated.value)).map_err(|_| CredError::Malformed)?;
    auth.set_sensitive(true);
    headers.insert(http::header::AUTHORIZATION, auth);
    let scope: Vec<&str> = scopes.split(' ').filter(|s| !s.is_empty()).collect();
    let body = Bytes::from(json!({"scope": scope, "lifetime": "3600s"}).to_string());
    let reply = send(client, tls, Method::POST, url, headers, body).await?;
    if !(200..300).contains(&reply.status) {
        return Err(CredError::Rejected(reply.status));
    }
    let r: Value = serde_json::from_slice(&reply.body).map_err(|_| CredError::Malformed)?;
    let token = r
        .get("accessToken")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .ok_or(CredError::Malformed)?;
    let expires = r
        .get("expireTime")
        .and_then(Value::as_str)
        .and_then(|t| t.parse::<jiff::Timestamp>().ok())
        .map_or(issued + 3600, |t| {
            issued + (t.as_second() - super::unix_now()).max(1)
        });
    Ok(Token {
        value: token.to_owned(),
        issued,
        expires,
    })
}

fn well_known_file() -> Option<std::path::PathBuf> {
    if let Some(appdata) = std::env::var_os("APPDATA").filter(|_| cfg!(windows)) {
        return Some(
            std::path::PathBuf::from(appdata)
                .join("gcloud")
                .join("application_default_credentials.json"),
        );
    }
    let home = std::env::var_os("HOME")?;
    Some(
        std::path::PathBuf::from(home)
            .join(".config")
            .join("gcloud")
            .join("application_default_credentials.json"),
    )
}

async fn credentials_file(
    client: &CredClient,
    tls: &TlsProfile,
    raw: &str,
    scopes: &str,
    issued: i64,
) -> Result<Token, CredError> {
    let v: Value = serde_json::from_str(raw).map_err(|_| CredError::Malformed)?;
    match v.get("type").and_then(Value::as_str) {
        Some("service_account") => {
            let account = ServiceAccount::from_value(&v).map_err(|_| CredError::Malformed)?;
            service_account(client, tls, &account, scopes, issued).await
        }
        Some("authorized_user") => authorized_user(client, tls, &v, issued).await,
        Some("external_account") => external_account(client, tls, &v, scopes, issued).await,
        _ => Err(CredError::Unsupported),
    }
}

/// Fetches a new token for `source`.
pub(crate) async fn fetch(
    client: &CredClient,
    tls: &TlsProfile,
    source: &GcpSource,
    issued: i64,
) -> Result<Token, CredError> {
    match source {
        GcpSource::ServiceAccount { account, scopes, .. } => {
            service_account(client, tls, account, scopes, issued).await
        }
        GcpSource::Adc { scopes } => {
            if let Some(path) = std::env::var_os("GOOGLE_APPLICATION_CREDENTIALS").filter(|p| !p.is_empty()) {
                let raw = tokio::fs::read_to_string(path)
                    .await
                    .map_err(|_| CredError::Unavailable)?;
                return credentials_file(client, tls, &raw, scopes, issued).await;
            }
            if let Some(path) = well_known_file()
                && let Ok(raw) = tokio::fs::read_to_string(path).await
            {
                return credentials_file(client, tls, &raw, scopes, issued).await;
            }
            metadata(client, tls, issued).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_account_documents_are_checked() {
        assert_eq!(
            ServiceAccount::parse("{not json").unwrap_err(),
            ServiceAccountError::Json
        );
        assert_eq!(
            ServiceAccount::parse(r#"{"client_email":"a@b","token_uri":"https://t"}"#).unwrap_err(),
            ServiceAccountError::Missing("private_key")
        );
        let bad_key = r#"{"client_email":"a@b","token_uri":"https://t",
            "private_key":"-----BEGIN PRIVATE KEY-----\nnot-a-key\n-----END PRIVATE KEY-----\n"}"#;
        assert_eq!(
            ServiceAccount::parse(bad_key).unwrap_err(),
            ServiceAccountError::Key
        );
    }

    #[test]
    fn token_replies() {
        let t = token_reply(200, br#"{"access_token":"ya29.x","expires_in":100}"#, 1000).unwrap();
        assert_eq!((t.value.as_str(), t.expires), ("ya29.x", 1100));
        assert_eq!(
            token_reply(200, br#"{"error":"invalid_grant"}"#, 0).unwrap_err(),
            CredError::Malformed
        );
        assert_eq!(token_reply(500, b"", 0).unwrap_err(), CredError::Rejected(500));
    }
}
