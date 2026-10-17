//! Upstream OAuth credentials of an MCP server: a client-credentials token for the server (RFC 6749
//! §4.4, with the RFC 8707 `resource`), or the caller's JWT exchanged for a server-audience token
//! (RFC 8693). Tokens are kept until 30 s before they expire; one refresh runs at a time per
//! server, so concurrent calls share it. The cache belongs to the server's compiled configuration,
//! so a changed configuration never reuses a token.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use base64::Engine;
use bytes::Bytes;
use http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use http::{HeaderMap, HeaderValue, Method, Uri};
use http_body_util::BodyExt;
use parking_lot::Mutex;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::egress::Egress;
use crate::upstream::UpError;

/// How long before its expiry a token is refreshed.
const REFRESH_MARGIN: Duration = Duration::from_secs(30);
/// The lifetime of a token whose answer gives none.
const DEFAULT_LIFETIME: u64 = 3600;
/// The most exchanged tokens kept per server.
const MAX_EXCHANGED: usize = 10_000;
const MAX_ANSWER: usize = 64 << 10;
const TOKEN_EXCHANGE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";

/// Which grant obtains the token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Grant {
    ClientCredentials,
    TokenExchange {
        subject_token_type: String,
        requested_token_type: String,
    },
}

/// An access token and when it stops being used.
#[derive(Clone)]
struct Held {
    token: String,
    until: Instant,
}

/// One server's OAuth settings and its tokens.
pub(crate) struct UpstreamAuth {
    pub(crate) grant: Grant,
    pub(crate) token_url: Uri,
    pub(crate) client_id: String,
    client_secret: String,
    pub(crate) scopes: Vec<String>,
    pub(crate) resource: Option<String>,
    pub(crate) audience: Option<String>,
    /// Client authentication with HTTP Basic (else in the form body).
    pub(crate) basic: bool,
    held: Mutex<HashMap<[u8; 32], Held>>,
    refresh: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for UpstreamAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpstreamAuth")
            .field("grant", &self.grant)
            .field("token_url", &self.token_url)
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

/// The `application/x-www-form-urlencoded` form of `s`.
fn form_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'*' => out.push(char::from(b)),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", form_encode(k), form_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

impl UpstreamAuth {
    /// Reads a validated `auth` block of type `oauth_client_credentials` or `token_exchange`;
    /// `server_url` is the default `resource`.
    pub(crate) fn compile(v: &Value, server_url: &str) -> Option<UpstreamAuth> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        let grant = match v.get("type")?.as_str()? {
            "oauth_client_credentials" => Grant::ClientCredentials,
            "token_exchange" => Grant::TokenExchange {
                subject_token_type: s("subject_token_type")
                    .unwrap_or_else(|| "urn:ietf:params:oauth:token-type:jwt".to_owned()),
                requested_token_type: s("requested_token_type")
                    .unwrap_or_else(|| "urn:ietf:params:oauth:token-type:access_token".to_owned()),
            },
            _ => return None,
        };
        let resource = match v.get("resource") {
            Some(Value::Null) => None,
            Some(Value::String(r)) => Some(r.clone()),
            _ => Some(server_url.to_owned()),
        };
        Some(UpstreamAuth {
            grant,
            token_url: s("token_url")?.parse().ok()?,
            client_id: s("client_id")?,
            client_secret: s("client_secret").unwrap_or_default(),
            scopes: v
                .get("scopes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            resource,
            audience: s("audience"),
            basic: v.get("client_auth").and_then(Value::as_str) != Some("post"),
            held: Mutex::new(HashMap::new()),
            refresh: tokio::sync::Mutex::new(()),
        })
    }

    /// The cache slot of a request: the subject token's digest, or one slot for every caller.
    fn slot(&self, subject: Option<&str>) -> Result<[u8; 32], UpError> {
        match (&self.grant, subject) {
            (Grant::ClientCredentials, _) => Ok([0; 32]),
            (Grant::TokenExchange { .. }, Some(t)) => Ok(Sha256::digest(t.as_bytes()).into()),
            (Grant::TokenExchange { .. }, None) => Err(UpError::SubjectRequired),
        }
    }

    fn cached(&self, slot: &[u8; 32]) -> Option<String> {
        let now = Instant::now();
        self.held
            .lock()
            .get(slot)
            .filter(|h| h.until > now)
            .map(|h| h.token.clone())
    }

    /// The `Authorization` value for a call made for the caller whose JWT is `subject`.
    pub(crate) async fn authorization(
        &self,
        egress: &Egress,
        subject: Option<&str>,
        timeout: Duration,
    ) -> Result<HeaderValue, UpError> {
        let slot = self.slot(subject)?;
        let token = match self.cached(&slot) {
            Some(t) => t,
            None => {
                let _one = self.refresh.lock().await;
                match self.cached(&slot) {
                    Some(t) => t,
                    None => self.fetch(egress, slot, subject, timeout).await?,
                }
            }
        };
        HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| UpError::Rejected)
    }

    async fn fetch(
        &self,
        egress: &Egress,
        slot: [u8; 32],
        subject: Option<&str>,
        timeout: Duration,
    ) -> Result<String, UpError> {
        let scope = self.scopes.join(" ");
        let mut pairs: Vec<(&str, &str)> = match &self.grant {
            Grant::ClientCredentials => vec![("grant_type", "client_credentials")],
            Grant::TokenExchange {
                subject_token_type,
                requested_token_type,
            } => vec![
                ("grant_type", TOKEN_EXCHANGE),
                ("subject_token", subject.unwrap_or_default()),
                ("subject_token_type", subject_token_type),
                ("requested_token_type", requested_token_type),
            ],
        };
        if !scope.is_empty() {
            pairs.push(("scope", &scope));
        }
        if let Some(r) = &self.resource {
            pairs.push(("resource", r));
        }
        if let Some(a) = &self.audience {
            pairs.push(("audience", a));
        }
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        if self.basic {
            let credentials = format!(
                "{}:{}",
                form_encode(&self.client_id),
                form_encode(&self.client_secret)
            );
            let value = format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(credentials)
            );
            if let Ok(v) = HeaderValue::from_str(&value) {
                headers.insert(AUTHORIZATION, v);
            }
        } else {
            pairs.push(("client_id", &self.client_id));
            pairs.push(("client_secret", &self.client_secret));
        }
        let body = Bytes::from(form(&pairs));
        let started = Instant::now();
        let resp = egress
            .send(Method::POST, self.token_url.clone(), headers, body, timeout)
            .await
            .map_err(|_| UpError::Auth(0))?;
        let status = resp.status.as_u16();
        let bytes = tokio::time::timeout(timeout, resp.body.collect())
            .await
            .map_err(|_| UpError::Auth(status))?
            .map_err(|_| UpError::Auth(status))?
            .to_bytes();
        if !resp.status.is_success() || bytes.len() > MAX_ANSWER {
            return Err(UpError::Auth(status));
        }
        let answer: Value = serde_json::from_slice(&bytes).map_err(|_| UpError::Auth(status))?;
        let token = answer
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .ok_or(UpError::Auth(status))?
            .to_owned();
        let lifetime = match answer.get("expires_in") {
            Some(Value::Number(n)) => n.as_u64(),
            Some(Value::String(s)) => s.trim().parse().ok(),
            _ => None,
        }
        .unwrap_or(DEFAULT_LIFETIME);
        let until = started + Duration::from_secs(lifetime).saturating_sub(REFRESH_MARGIN);
        let mut held = self.held.lock();
        if held.len() >= MAX_EXCHANGED {
            let now = Instant::now();
            held.retain(|_, h| h.until > now);
            if held.len() >= MAX_EXCHANGED {
                held.clear();
            }
        }
        held.insert(
            slot,
            Held {
                token: token.clone(),
                until,
            },
        );
        Ok(token)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn settings_and_defaults() {
        let a = UpstreamAuth::compile(
            &json!({"type": "oauth_client_credentials", "token_url": "https://idp/token",
                    "client_id": "c", "client_secret": "s", "scopes": ["a", "b"]}),
            "https://mcp.example/mcp",
        )
        .unwrap();
        assert_eq!(a.grant, Grant::ClientCredentials);
        assert_eq!(a.resource.as_deref(), Some("https://mcp.example/mcp"));
        assert!(a.basic);
        assert_eq!(a.slot(None).unwrap(), [0; 32]);
        let x = UpstreamAuth::compile(
            &json!({"type": "token_exchange", "token_url": "https://idp/token", "client_id": "c",
                    "resource": null, "client_auth": "post"}),
            "https://mcp.example/mcp",
        )
        .unwrap();
        assert!(x.resource.is_none() && !x.basic);
        assert_eq!(x.slot(None), Err(UpError::SubjectRequired));
        assert_ne!(x.slot(Some("a")).unwrap(), x.slot(Some("b")).unwrap());
    }

    #[test]
    fn forms_are_encoded() {
        assert_eq!(
            form(&[("scope", "a b"), ("x", "é&=")]),
            "scope=a+b&x=%C3%A9%26%3D"
        );
    }
}
