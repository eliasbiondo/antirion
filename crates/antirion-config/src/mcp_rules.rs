//! Rules for the `mcp` section that the schema cannot express: server references of gateways, unique
//! prefixes per gateway, `server_info: upstream` on single-server gateways only, unique session key ids
//! and OAuth resource URLs that name the gateway they protect.

use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::{ConfigError, DetailCode, DocPath};

fn gateways(doc: &Value) -> impl Iterator<Item = (&String, &Value)> {
    doc.pointer("/mcp/gateways")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(Map::iter)
}

fn server_names(gw: &Value) -> impl Iterator<Item = (usize, &str)> {
    gw.get("servers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(i, s)| Some((i, s.as_str()?)))
}

/// The servers each gateway references: `(path, server name)`.
pub(crate) fn references(doc: &Value) -> Vec<(DocPath, String)> {
    let mut out = Vec::new();
    for (name, gw) in gateways(doc) {
        let at = DocPath::root()
            .key("mcp")
            .key("gateways")
            .key(name)
            .key("servers");
        for (i, server) in server_names(gw) {
            out.push((at.index(i), server.to_owned()));
        }
    }
    out
}

fn oauth_resource_path(resource: &str) -> Option<&str> {
    let rest = resource.split_once("://")?.1;
    let path = rest.find('/').map_or("", |i| rest.get(i..).unwrap_or_default());
    Some(
        path.split(['?', '#'])
            .next()
            .unwrap_or_default()
            .trim_end_matches('/'),
    )
}

/// A server whose `auth` obtains tokens may not also send a static `authorization` header.
fn auth_conflicts(doc: &Value, errors: &mut Vec<ConfigError>) {
    for (name, s) in doc
        .pointer("/mcp/servers")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let typed = s
            .pointer("/auth/type")
            .and_then(Value::as_str)
            .is_some_and(|t| t != "none");
        let header = s
            .get("headers")
            .and_then(Value::as_object)
            .is_some_and(|h| h.keys().any(|k| k.eq_ignore_ascii_case("authorization")));
        if typed && header {
            errors.push(ConfigError::new(
                &DocPath::root().key("mcp").key("servers").key(name).key("auth"),
                DetailCode::Conflict,
                "auth and an authorization header cannot both be set",
            ));
        }
    }
}

/// A `stdio` server's command must be one of `mcp.stdio.allowed_commands`.
fn stdio_commands(doc: &Value, errors: &mut Vec<ConfigError>) {
    let allowed: Vec<&str> = doc
        .pointer("/mcp/stdio/allowed_commands")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    for (name, s) in doc
        .pointer("/mcp/servers")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        if s.get("transport").and_then(Value::as_str) != Some("stdio") {
            continue;
        }
        let command = s.get("command").and_then(Value::as_str).unwrap_or_default();
        if !allowed.contains(&command) {
            errors.push(ConfigError::new(
                &DocPath::root().key("mcp").key("servers").key(name).key("command"),
                DetailCode::Conflict,
                "the command is not in mcp.stdio.allowed_commands",
            ));
        }
    }
}

pub(crate) fn check(doc: &Value, errors: &mut Vec<ConfigError>) {
    auth_conflicts(doc, errors);
    stdio_commands(doc, errors);
    let servers = doc.pointer("/mcp/servers").and_then(Value::as_object);
    for (name, gw) in gateways(doc) {
        let at = DocPath::root().key("mcp").key("gateways").key(name);
        let mut prefixes = HashSet::new();
        let mut count = 0usize;
        for (i, server) in server_names(gw) {
            count += 1;
            let prefix = servers
                .and_then(|s| s.get(server))
                .and_then(|s| s.get("prefix"))
                .and_then(Value::as_str)
                .unwrap_or(server);
            if !prefixes.insert(prefix.to_owned()) {
                errors.push(ConfigError::new(
                    &at.key("servers").index(i),
                    DetailCode::Conflict,
                    format!("prefix {prefix:?} is used by another server of this gateway"),
                ));
            }
        }
        if gw.get("server_info").and_then(Value::as_str) == Some("upstream") && count != 1 {
            errors.push(ConfigError::new(
                &at.key("server_info"),
                DetailCode::Conflict,
                "server_info: upstream needs a gateway with exactly one server",
            ));
        }
        if let Some(resource) = gw.pointer("/oauth/resource").and_then(Value::as_str) {
            let wanted = format!("/mcp/{name}");
            let path = oauth_resource_path(resource).unwrap_or_default();
            if path != wanted && !(name == "default" && path == "/mcp") {
                errors.push(ConfigError::new(
                    &at.key("oauth").key("resource"),
                    DetailCode::InvalidFormat,
                    format!("the resource path must be {wanted}"),
                ));
            }
        }
    }
    let keys = doc
        .pointer("/mcp/session/keys")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    let mut ids = HashSet::new();
    for (i, k) in keys.enumerate() {
        if let Some(id) = k.get("id").and_then(Value::as_u64)
            && !ids.insert(id)
        {
            errors.push(ConfigError::new(
                &DocPath::root()
                    .key("mcp")
                    .key("session")
                    .key("keys")
                    .index(i)
                    .key("id"),
                DetailCode::DuplicateKey,
                "session key ids must be unique",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn rules() {
        let doc = json!({"mcp": {
            "servers": {"a": {"prefix": "x"}, "b": {"prefix": "x"}},
            "gateways": {
                "g": {"servers": ["a", "b"], "server_info": "upstream",
                      "oauth": {"resource": "https://h/mcp/other", "authorization_servers": ["https://i"]}},
                "default": {"servers": ["a"], "oauth": {"resource": "https://h/mcp", "authorization_servers": ["https://i"]}}},
            "session": {"keys": [{"id": 1}, {"id": 1}]}}});
        let mut errors = Vec::new();
        check(&doc, &mut errors);
        let paths: Vec<&str> = errors.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"mcp.gateways.g.servers[1]"), "{paths:?}");
        assert!(paths.contains(&"mcp.gateways.g.server_info"));
        assert!(paths.contains(&"mcp.gateways.g.oauth.resource"));
        assert!(paths.contains(&"mcp.session.keys[1].id"));
        assert_eq!(errors.len(), 4, "{paths:?}");
        assert_eq!(references(&doc).len(), 3);
    }
}
