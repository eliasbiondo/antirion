//! OAuth protected-resource behaviour of MCP gateways: one RFC 9728 metadata document per URL a
//! gateway is served at, at the well-known URL derived from it, and the `WWW-Authenticate` challenge
//! that points clients there.

use http::{HeaderValue, Response, Uri};
use serde_json::json;

use crate::compile::{GatewayRt, McpSnapshot};
use crate::{Body, respond};

const WELL_KNOWN: &str = "/.well-known/oauth-protected-resource";

/// Every resource URL of a gateway: the configured one, else each URL it is served at.
fn resources(gw: &GatewayRt, base: &str) -> Vec<String> {
    let Some(oauth) = &gw.oauth else {
        return Vec::new();
    };
    if let Some(r) = &oauth.resource {
        return vec![r.clone()];
    }
    let mut out = vec![format!("{base}/mcp/{}", gw.name)];
    if gw.name == "default" {
        out.push(format!("{base}/mcp"));
    }
    out
}

fn path_of(resource: &str) -> Option<String> {
    let uri: Uri = resource.parse().ok()?;
    Some(uri.path().trim_end_matches('/').to_owned())
}

fn metadata_url(resource: &str) -> Option<String> {
    let uri: Uri = resource.parse().ok()?;
    let origin = format!("{}://{}", uri.scheme_str()?, uri.authority()?);
    Some(format!(
        "{origin}{WELL_KNOWN}{}",
        uri.path().trim_end_matches('/')
    ))
}

/// The metadata document served at `/.well-known/oauth-protected-resource<suffix>`, if any gateway
/// has a resource whose path is `suffix`.
#[must_use]
pub fn metadata(snap: &McpSnapshot, suffix: &str, base: &str) -> Option<Response<Body>> {
    let suffix = suffix.trim_end_matches('/');
    if suffix.is_empty() {
        return None;
    }
    for gw in snap.gateways.values().filter(|g| g.enabled) {
        let Some(oauth) = &gw.oauth else {
            continue;
        };
        for resource in resources(gw, base) {
            if path_of(&resource).as_deref() != Some(suffix) {
                continue;
            }
            let mut doc = json!({
                "resource": resource,
                "authorization_servers": oauth.authorization_servers,
                "bearer_methods_supported": ["header"],
            });
            if !oauth.scopes_supported.is_empty()
                && let Some(obj) = doc.as_object_mut()
            {
                obj.insert("scopes_supported".into(), json!(oauth.scopes_supported));
            }
            return Some(respond::json(200, &doc, "upstream"));
        }
    }
    None
}

/// The audiences a JWT presented to `gateway` must name, when the gateway has `oauth`:
/// `oauth.audiences`, else every resource URL of the gateway.
#[must_use]
pub fn audiences(snap: &McpSnapshot, gateway: &str, base: &str) -> Option<Vec<String>> {
    let gw = snap.gateway(gateway)?;
    let oauth = gw.oauth.as_ref()?;
    Some(oauth.audiences.clone().unwrap_or_else(|| resources(gw, base)))
}

/// The `WWW-Authenticate` value of a 401 on `request_path` (`/mcp` or `/mcp/<name>`) when the gateway
/// has `oauth`.
#[must_use]
pub fn challenge(snap: &McpSnapshot, gateway: &str, request_path: &str, base: &str) -> Option<HeaderValue> {
    let gw = snap.gateway(gateway)?;
    let oauth = gw.oauth.as_ref()?;
    let resource = match &oauth.resource {
        Some(r) => r.clone(),
        None => format!("{base}{}", request_path.trim_end_matches('/')),
    };
    let mut value = format!("Bearer resource_metadata=\"{}\"", metadata_url(&resource)?);
    if !oauth.scopes_supported.is_empty() {
        value.push_str(&format!(", scope=\"{}\"", oauth.scopes_supported.join(" ")));
    }
    HeaderValue::from_str(&value).ok()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::compile::compile;

    fn snap() -> McpSnapshot {
        compile(&json!({"mcp": {
            "servers": {"s": {"transport": "streamable_http", "url": "http://h/mcp"}},
            "gateways": {
                "g": {"servers": ["s"], "oauth": {"resource": "https://mcp.example/mcp/g",
                       "authorization_servers": ["https://idp.example"], "scopes_supported": ["mcp:tools", "mcp:read"]}},
                "default": {"servers": ["s"], "oauth": {"authorization_servers": ["https://idp.example"]}}}}}))
        .unwrap()
    }

    #[test]
    fn documents_are_served_at_path_derived_urls() {
        let s = snap();
        assert!(metadata(&s, "/mcp/g", "http://gw:1").is_some());
        assert!(metadata(&s, "/mcp/default", "http://gw:1").is_some());
        assert!(metadata(&s, "/mcp", "http://gw:1").is_some());
        assert!(metadata(&s, "", "http://gw:1").is_none());
        assert!(metadata(&s, "/mcp/other", "http://gw:1").is_none());
    }

    #[test]
    fn audiences_default_to_the_resource_urls() {
        let s = snap();
        assert_eq!(
            audiences(&s, "g", "http://gw:1").unwrap(),
            ["https://mcp.example/mcp/g"]
        );
        assert_eq!(
            audiences(&s, "default", "http://gw:1").unwrap(),
            ["http://gw:1/mcp/default", "http://gw:1/mcp"]
        );
        let configured = compile(&json!({"mcp": {
            "servers": {"s": {"transport": "streamable_http", "url": "http://h/mcp"}},
            "gateways": {"g": {"servers": ["s"], "oauth": {"authorization_servers": ["https://idp.example"],
                "audiences": ["api://mcp"]}}}}}))
        .unwrap();
        assert_eq!(audiences(&configured, "g", "http://gw:1").unwrap(), ["api://mcp"]);
        assert!(audiences(&s, "missing", "http://gw:1").is_none());
    }

    #[test]
    fn challenge_names_the_metadata_and_scopes() {
        let s = snap();
        let v = challenge(&s, "g", "/mcp/g", "http://gw:1").unwrap();
        assert_eq!(
            v,
            "Bearer resource_metadata=\"https://mcp.example/.well-known/oauth-protected-resource/mcp/g\", scope=\"mcp:tools mcp:read\""
        );
        let d = challenge(&s, "default", "/mcp", "http://gw:1").unwrap();
        assert_eq!(
            d,
            "Bearer resource_metadata=\"http://gw:1/.well-known/oauth-protected-resource/mcp\""
        );
    }
}
