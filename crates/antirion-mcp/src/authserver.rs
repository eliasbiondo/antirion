//! Authorization-server helpers for clients that look for them on the resource origin: the configured
//! issuer's RFC 8414 metadata, fetched and served at `/.well-known/oauth-authorization-server`, and,
//! for gateways with `oauth.proxy_registration`, RFC 7591 dynamic client registration relayed to the
//! issuer. The gateway never issues tokens itself.

use std::time::Duration;

use antirion_core::Stage;
use antirion_error::{Code, GatewayError, Message};
use bytes::Bytes;
use http::header::{ACCEPT, CONTENT_TYPE};
use http::{HeaderMap, HeaderValue, Method, Response, StatusCode, Uri};
use http_body_util::BodyExt;
use serde_json::Value;

use crate::compile::GatewayRt;
use crate::http::Env;
use crate::{Body, respond};

/// How long fetched issuer metadata is reused.
const METADATA_TTL: Duration = Duration::from_secs(300);
const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_DOCUMENT: usize = 256 << 10;

/// What a helper answers.
#[derive(Debug)]
pub enum Helped {
    /// The answer to send.
    Answer(Response<Body>),
    /// No gateway offers the helper at this path.
    NotFound,
    Failed(GatewayError),
}

/// The gateway a metadata path addresses: `/mcp/<name>`, `/mcp` for `default`, or the bare path for
/// `default`, or the only gateway with `oauth`.
fn addressed<'a>(env: &'a Env, suffix: &str) -> Option<&'a GatewayRt> {
    let name = match suffix.trim_end_matches('/') {
        "" => {
            let protected: Vec<&GatewayRt> = env
                .snap
                .gateways
                .values()
                .filter(|g| g.enabled && g.oauth.is_some())
                .map(AsRef::as_ref)
                .collect();
            return match env.snap.gateway("default").filter(|g| g.oauth.is_some()) {
                Some(d) => Some(d),
                None if protected.len() == 1 => protected.first().copied(),
                None => None,
            };
        }
        "/mcp" => "default",
        other => other.strip_prefix("/mcp/")?,
    };
    env.snap
        .gateway(name)
        .filter(|g| g.oauth.is_some())
        .map(AsRef::as_ref)
}

/// The RFC 8414 and OpenID Connect metadata URLs of an issuer, in the order they are tried.
fn metadata_urls(issuer: &str) -> Vec<String> {
    let Ok(uri) = issuer.parse::<Uri>() else {
        return Vec::new();
    };
    let (Some(scheme), Some(authority)) = (uri.scheme_str(), uri.authority()) else {
        return Vec::new();
    };
    let path = uri.path().trim_end_matches('/');
    vec![
        format!("{scheme}://{authority}/.well-known/oauth-authorization-server{path}"),
        format!("{scheme}://{authority}{path}/.well-known/openid-configuration"),
    ]
}

fn unavailable(message: &'static str) -> GatewayError {
    GatewayError::new(Code::UpstreamUnavailable, Stage::Attempt, Message::Fixed(message))
}

async fn get_json(env: &Env, url: &str) -> Option<Value> {
    let uri: Uri = url.parse().ok()?;
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    let resp = env
        .egress
        .send(Method::GET, uri, headers, Bytes::new(), TIMEOUT)
        .await
        .ok()?;
    if !resp.status.is_success() {
        return None;
    }
    let bytes = tokio::time::timeout(TIMEOUT, resp.body.collect())
        .await
        .ok()?
        .ok()?
        .to_bytes();
    if bytes.len() > MAX_DOCUMENT {
        return None;
    }
    serde_json::from_slice::<Value>(&bytes)
        .ok()
        .filter(Value::is_object)
}

/// The issuer's metadata, from the node's cache or fetched now.
async fn issuer_metadata(env: &Env, issuer: &str) -> Option<Value> {
    let now = env.now;
    if let Some((at, doc)) = env.state.issuers.lock().get(issuer).cloned()
        && now >= at
        && now.since(at) < METADATA_TTL
    {
        return Some(doc);
    }
    for url in metadata_urls(issuer) {
        if let Some(doc) = get_json(env, &url).await {
            env.state
                .issuers
                .lock()
                .insert(issuer.to_owned(), (now, doc.clone()));
            return Some(doc);
        }
    }
    None
}

/// `GET /.well-known/oauth-authorization-server<suffix>`.
pub async fn authorization_server(env: &Env, suffix: &str) -> Helped {
    let Some(gw) = addressed(env, suffix) else {
        return Helped::NotFound;
    };
    let Some(issuer) = gw.oauth.as_ref().and_then(|o| o.authorization_servers.first()) else {
        return Helped::NotFound;
    };
    let Some(mut doc) = issuer_metadata(env, issuer).await else {
        return Helped::Failed(unavailable(
            "The authorization server's metadata could not be read.",
        ));
    };
    let relays = gw.oauth.as_ref().is_some_and(|o| o.proxy_registration);
    if let Some(obj) = doc.as_object_mut()
        && relays
        && obj.contains_key("registration_endpoint")
    {
        obj.insert(
            "registration_endpoint".into(),
            Value::from(format!("{}/mcp/{}/register", env.public_base, gw.name)),
        );
    }
    Helped::Answer(respond::json(200, &doc, "upstream"))
}

/// `POST /mcp/<gateway>/register`: the registration request relayed to the issuer's registration
/// endpoint, and its answer returned as it is.
pub async fn register(env: &Env, gateway: &str, body: Bytes) -> Helped {
    let Some(gw) = env
        .snap
        .gateway(gateway)
        .filter(|g| g.oauth.as_ref().is_some_and(|o| o.proxy_registration))
    else {
        return Helped::NotFound;
    };
    let Some(issuer) = gw.oauth.as_ref().and_then(|o| o.authorization_servers.first()) else {
        return Helped::NotFound;
    };
    let endpoint = issuer_metadata(env, issuer)
        .await
        .and_then(|d| d.get("registration_endpoint")?.as_str().map(str::to_owned))
        .and_then(|u| u.parse::<Uri>().ok());
    let Some(endpoint) = endpoint else {
        return Helped::Failed(unavailable(
            "The authorization server offers no client registration.",
        ));
    };
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    let Ok(resp) = env
        .egress
        .send(Method::POST, endpoint, headers, body, TIMEOUT)
        .await
    else {
        return Helped::Failed(unavailable("The authorization server could not be reached."));
    };
    let status = resp.status;
    let ctype = resp.headers.get(CONTENT_TYPE).cloned();
    let Ok(Ok(collected)) = tokio::time::timeout(TIMEOUT, resp.body.collect()).await else {
        return Helped::Failed(unavailable("The authorization server's answer was cut."));
    };
    let bytes = collected.to_bytes();
    if bytes.len() > MAX_DOCUMENT {
        return Helped::Failed(unavailable("The authorization server's answer is too large."));
    }
    let mut out = Response::new(
        http_body_util::Full::new(bytes)
            .map_err(|never| match never {})
            .boxed(),
    );
    *out.status_mut() = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    if let Some(c) = ctype {
        out.headers_mut().insert(CONTENT_TYPE, c);
    }
    out.headers_mut()
        .insert(http::header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    out.headers_mut().insert(
        http::HeaderName::from_static(antirion_core::headers::RESPONSE_PATH),
        HeaderValue::from_static("upstream"),
    );
    Helped::Answer(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issuer_metadata_urls_follow_rfc_8414_then_openid() {
        assert_eq!(
            metadata_urls("https://idp.example"),
            [
                "https://idp.example/.well-known/oauth-authorization-server",
                "https://idp.example/.well-known/openid-configuration"
            ]
        );
        assert_eq!(
            metadata_urls("https://idp.example/tenant/"),
            [
                "https://idp.example/.well-known/oauth-authorization-server/tenant",
                "https://idp.example/tenant/.well-known/openid-configuration"
            ]
        );
        assert!(metadata_urls("not a url").is_empty());
    }
}
