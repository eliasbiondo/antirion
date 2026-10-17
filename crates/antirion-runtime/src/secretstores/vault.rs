//! HashiCorp Vault: `${vault:<path>#<field>}` is `GET <address>/v1/<path>`. A KV version 2 secret
//! (a `data` object holding `data` and `metadata`) gives `field` of `data.data`; any other engine
//! gives `field` of `data`. AppRole and Kubernetes logins are kept until their lease ends and are
//! repeated once when the token is refused.

use std::sync::Arc;
use std::time::{Duration, Instant};

use antirion_core::Secret;
use bytes::Bytes;
use http::{HeaderMap, HeaderName, Method};
use parking_lot::Mutex;
use serde_json::{Value, json};

use super::fetch::{Cx, FetchError, json as answer_json, send, sensitive};
use super::settings::{StoreSettings, VaultAuth, VaultSettings};
use super::{Fetched, text_of};

const TOKEN: HeaderName = HeaderName::from_static("x-vault-token");
const NAMESPACE: HeaderName = HeaderName::from_static("x-vault-namespace");

/// A login's token: the fingerprint of the settings it was obtained with, the token and when its
/// lease ends.
struct Token {
    fingerprint: u64,
    token: Arc<Secret>,
    until: Option<Instant>,
}

/// The current login.
#[derive(Default)]
pub(crate) struct Login {
    current: Mutex<Option<Token>>,
}

impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Login")
    }
}

impl Login {
    fn valid(&self, fingerprint: u64) -> Option<Arc<Secret>> {
        let now = Instant::now();
        self.current
            .lock()
            .as_ref()
            .filter(|t| t.fingerprint == fingerprint && t.until.is_none_or(|u| now < u))
            .map(|t| Arc::clone(&t.token))
    }

    fn forget(&self) {
        *self.current.lock() = None;
    }
}

/// `<path>#<field>`, both present.
fn parse(reference: &str) -> Result<(&str, &str), FetchError> {
    match reference.rsplit_once('#') {
        Some((path, field)) if !path.trim_matches('/').is_empty() && !field.is_empty() => {
            Ok((path.trim_matches('/'), field))
        }
        _ => Err(FetchError::new("a vault reference is <path>#<field>")),
    }
}

fn headers(v: &VaultSettings, token: Option<&str>) -> Result<HeaderMap, FetchError> {
    let mut h = HeaderMap::new();
    if let Some(t) = token {
        h.insert(TOKEN, sensitive(t)?);
    }
    if let Some(ns) = &v.namespace {
        h.insert(NAMESPACE, sensitive(ns)?);
    }
    Ok(h)
}

async fn login(
    cx: &Cx,
    s: &StoreSettings,
    v: &VaultSettings,
    session: &Login,
) -> Result<Arc<Secret>, FetchError> {
    if let VaultAuth::Token(t) = &v.auth {
        return Ok(Arc::new(Secret::new(t.expose())));
    }
    if let Some(t) = session.valid(s.fingerprint) {
        return Ok(t);
    }
    let (mount, body) = match &v.auth {
        VaultAuth::AppRole {
            mount,
            role_id,
            secret_id,
        } => (
            mount,
            json!({"role_id": role_id, "secret_id": secret_id.expose()}),
        ),
        VaultAuth::Kubernetes {
            mount,
            role,
            jwt_file,
        } => {
            let jwt = tokio::fs::read_to_string(jwt_file)
                .await
                .map_err(|_| FetchError::new("the service account token file cannot be read"))?;
            (mount, json!({"role": role, "jwt": jwt.trim()}))
        }
        VaultAuth::Token(_) => return Err(FetchError::new("no login is needed")),
    };
    let url = format!("{}/v1/auth/{}/login", v.address, mount.trim_matches('/'));
    let answer = send(
        cx,
        Method::POST,
        &url,
        headers(v, None)?,
        Bytes::from(body.to_string()),
        s.timeout,
    )
    .await?;
    let reply = answer_json(&answer).map_err(|e| FetchError::new(format!("the login failed: {e}")))?;
    let auth = reply.get("auth");
    let token = auth
        .and_then(|a| a.get("client_token"))
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| FetchError::new("the login answer has no token"))?;
    let lease = auth
        .and_then(|a| a.get("lease_duration"))
        .and_then(Value::as_u64)
        .filter(|l| *l > 0)
        .map(|l| Instant::now() + Duration::from_secs(l).mul_f64(0.9));
    let token = Arc::new(Secret::new(token));
    *session.current.lock() = Some(Token {
        fingerprint: s.fingerprint,
        token: Arc::clone(&token),
        until: lease,
    });
    Ok(token)
}

/// The secret's fields: `data.data` of a KV version 2 answer, else `data`.
fn fields(reply: &Value) -> Option<&serde_json::Map<String, Value>> {
    let data = reply.get("data")?;
    match (data.get("data"), data.get("metadata")) {
        (Some(Value::Object(inner)), Some(Value::Object(_))) => Some(inner),
        _ => data.as_object(),
    }
}

async fn read(
    cx: &Cx,
    s: &StoreSettings,
    v: &VaultSettings,
    path: &str,
    token: &str,
) -> Result<(u16, Bytes), FetchError> {
    let url = format!("{}/v1/{path}", v.address);
    let answer = send(
        cx,
        Method::GET,
        &url,
        headers(v, Some(token))?,
        Bytes::new(),
        s.timeout,
    )
    .await?;
    Ok((answer.status, answer.body))
}

/// Reads one reference.
pub(crate) async fn fetch(
    cx: &Cx,
    s: &StoreSettings,
    v: &VaultSettings,
    session: &Login,
    reference: &str,
) -> Result<Fetched, FetchError> {
    let (path, field) = parse(reference)?;
    let mut token = login(cx, s, v, session).await?;
    let (mut status, mut body) = read(cx, s, v, path, token.expose()).await?;
    if status == 403 && !matches!(v.auth, VaultAuth::Token(_)) {
        session.forget();
        token = login(cx, s, v, session).await?;
        (status, body) = read(cx, s, v, path, token.expose()).await?;
    }
    let reply = answer_json(&super::fetch::Answer { status, body })?;
    let value = fields(&reply)
        .and_then(|f| f.get(field))
        .ok_or_else(|| FetchError::new(format!("the secret has no field {field}")))?;
    let lease = reply
        .get("lease_duration")
        .and_then(Value::as_u64)
        .filter(|l| *l > 0)
        .map(Duration::from_secs);
    Ok(Fetched {
        value: text_of(value).ok_or_else(|| FetchError::new(format!("the field {field} is not a string")))?,
        lease,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_name_a_path_and_a_field() {
        assert_eq!(
            parse("secret/data/gw/openai#api_key").unwrap(),
            ("secret/data/gw/openai", "api_key")
        );
        assert_eq!(parse("/kv/a#b#c").unwrap(), ("kv/a#b", "c"));
        assert!(parse("secret/data/gw").is_err());
        assert!(parse("#k").is_err());
        assert!(parse("p#").is_err());
    }

    #[test]
    fn kv2_answers_are_unwrapped() {
        let v2 = json!({"data": {"data": {"k": "v2"}, "metadata": {"version": 3}}});
        assert_eq!(fields(&v2).unwrap()["k"], "v2");
        let v1 = json!({"data": {"k": "v1", "data": "x"}});
        assert_eq!(fields(&v1).unwrap()["k"], "v1");
    }
}
