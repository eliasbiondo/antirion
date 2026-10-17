//! Compiling the egress settings of a document, and the checks the schema cannot express: allow-list
//! entries that would lift an always-denied range, and operator URLs against the egress policy.

use std::net::IpAddr;

use antirion_config::{ConfigError, DetailCode, DocPath};
use http::{HeaderName, HeaderValue};
use ipnet::IpNet;
use serde_json::Value;

use crate::headers::{HeaderBlock, Pattern};
use crate::ssrf::{ALWAYS_DENIED, AllowList, EgressPolicy, TrustClass, validate_url};

fn strings(v: Option<&Value>) -> impl Iterator<Item = (usize, &str)> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(i, s)| s.as_str().map(|s| (i, s)))
}

fn allow_list(block: Option<&Value>, at: &DocPath, errors: &mut Vec<ConfigError>) -> AllowList {
    let mut list = AllowList::default();
    for (i, raw) in strings(block.and_then(|b| b.get("allow_cidrs"))) {
        let path = at.key("allow_cidrs").index(i);
        match raw
            .parse::<IpNet>()
            .or_else(|_| raw.parse::<IpAddr>().map(IpNet::from))
        {
            Ok(net) if ALWAYS_DENIED.iter().any(|d| d.contains(&net)) => {
                errors.push(ConfigError::new(
                    &path,
                    DetailCode::Conflict,
                    "the range is always denied (link-local, metadata, multicast or reserved addresses)",
                ));
            }
            Ok(net) => list.cidrs.push(net.trunc()),
            Err(_) => errors.push(ConfigError::new(&path, DetailCode::InvalidFormat, "not a CIDR")),
        }
    }
    list.hosts = strings(block.and_then(|b| b.get("allow_hosts")))
        .map(|(_, h)| h.to_ascii_lowercase())
        .collect();
    list
}

/// Compiles `security.egress`, reporting allow-list entries that can never apply.
pub fn compile_policy(doc: &Value) -> Result<EgressPolicy, Vec<ConfigError>> {
    let at = DocPath::root().key("security").key("egress");
    let egress = doc.get("security").and_then(|s| s.get("egress"));
    let mut errors = Vec::new();
    let operator = egress.and_then(|e| e.get("operator"));
    let client = egress.and_then(|e| e.get("client_influenced"));
    let mut policy = EgressPolicy {
        operator_deny_private: operator
            .and_then(|o| o.get("deny_private"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        operator_allow: allow_list(operator, &at.key("operator"), &mut errors),
        client_allow: allow_list(client, &at.key("client_influenced"), &mut errors),
        ..EgressPolicy::default()
    };
    if let Some(ports) = client
        .and_then(|c| c.get("allowed_ports"))
        .and_then(Value::as_array)
    {
        policy.client_ports = ports
            .iter()
            .filter_map(Value::as_u64)
            .filter_map(|p| u16::try_from(p).ok())
            .collect();
    }
    for (_, extra) in strings(egress.and_then(|e| e.get("metadata_hosts_extra"))) {
        let bare = extra.trim_start_matches('[').trim_end_matches(']');
        match bare.parse::<IpAddr>() {
            Ok(ip) => policy.metadata_addrs.push(ip),
            Err(_) => policy.metadata_names.push(extra.to_ascii_lowercase()),
        }
    }
    if let Some(p) = egress.and_then(|e| e.get("proxy")).filter(|p| !p.is_null()) {
        let no_proxy = strings(p.get("no_proxy"))
            .filter_map(|(_, e)| crate::proxy::NoProxy::parse(e))
            .collect();
        match crate::proxy::Proxy::parse(p.get("url").and_then(Value::as_str).unwrap_or_default(), no_proxy) {
            Ok(proxy) => policy.proxy = Some(proxy),
            Err(why) => errors.push(ConfigError::new(
                &at.key("proxy").key("url"),
                DetailCode::InvalidFormat,
                why,
            )),
        }
    }
    if errors.is_empty() {
        Ok(policy)
    } else {
        Err(errors)
    }
}

/// The egress checks of a whole document: the policy itself, and every operator destination
/// (provider base URLs, MCP server URLs, agent card and endpoint URLs, guardrail and extension
/// webhooks) against the URL rules for operator destinations.
#[must_use]
pub fn check_document(doc: &Value) -> Vec<ConfigError> {
    let policy = match compile_policy(doc) {
        Ok(p) => p,
        Err(errors) => return errors,
    };
    let mut errors = Vec::new();
    let destinations: [(&str, &str, &[&str]); 3] = [
        ("providers", "", &["base_url"]),
        ("mcp", "servers", &["url", "auth.token_url"]),
        ("a2a", "agents", &["card_url", "url"]),
    ];
    for (top, collection, fields) in destinations {
        let mut at = DocPath::root().key(top);
        let mut members = doc.get(top);
        if !collection.is_empty() {
            at = at.key(collection);
            members = members.and_then(|m| m.get(collection));
        }
        for (name, item) in members.and_then(Value::as_object).into_iter().flatten() {
            for field in fields {
                let Some(url) = field
                    .split('.')
                    .try_fold(item, |v, k| v.get(k))
                    .and_then(Value::as_str)
                else {
                    continue;
                };
                if url.contains('{') {
                    continue;
                }
                if let Err(denied) = validate_url(url, TrustClass::Operator, &policy) {
                    let path = field.split('.').fold(at.key(name), |p, k| p.key(k));
                    errors.push(ConfigError::new(
                        &path,
                        DetailCode::InvalidFormat,
                        format!("the URL is not allowed: {denied}"),
                    ));
                }
            }
        }
    }
    let hooks: [(&str, &str); 2] = [("guardrails", "definitions"), ("plugins", "")];
    for (top, collection) in hooks {
        let mut at = DocPath::root().key(top);
        let mut members = doc.get(top);
        if !collection.is_empty() {
            at = at.key(collection);
            members = members.and_then(|m| m.get(collection));
        }
        for (name, item) in members.and_then(Value::as_object).into_iter().flatten() {
            let Some(url) = item
                .get("webhook")
                .and_then(|w| w.get("url"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            if let Err(denied) = validate_url(url, TrustClass::Operator, &policy) {
                errors.push(ConfigError::new(
                    &at.key(name).key("webhook").key("url"),
                    DetailCode::InvalidFormat,
                    format!("the URL is not allowed: {denied}"),
                ));
            }
        }
    }
    crate::route_urls::check_document(doc, &policy, &mut errors);
    for (at, block) in antirion_config::semantic::policy_blocks(doc) {
        if let Some(h) = block.get("headers")
            && let Err(e) = header_block(h, &at.key("headers"))
        {
            errors.extend(e);
        }
    }
    errors
}

fn patterns(
    v: Option<&Value>,
    at: &DocPath,
    errors: &mut Vec<ConfigError>,
    forwarding: bool,
) -> Vec<Pattern> {
    let mut out = Vec::new();
    for (i, raw) in strings(v) {
        match Pattern::parse(raw) {
            Ok(p) if forwarding && p.names_always_denied() => errors.push(ConfigError::new(
                &at.index(i),
                DetailCode::Conflict,
                format!("{raw:?} is on the always-deny list and is never forwarded"),
            )),
            Ok(p) => out.push(p),
            Err(e) => errors.push(ConfigError::new(
                &at.index(i),
                DetailCode::InvalidFormat,
                e.to_string(),
            )),
        }
    }
    out
}

/// Compiles the `headers` member of one `policies` block.
pub fn header_block(h: &Value, at: &DocPath) -> Result<HeaderBlock, Vec<ConfigError>> {
    let mut errors = Vec::new();
    let forward = h
        .get("forward")
        .map(|v| patterns(Some(v), &at.key("forward"), &mut errors, true));
    let deny = patterns(h.get("deny"), &at.key("deny"), &mut errors, false);
    let response_forward = h
        .get("response_forward")
        .map(|v| patterns(Some(v), &at.key("response_forward"), &mut errors, true));
    let mut set = Vec::new();
    for (name, value) in h.get("set").and_then(Value::as_object).into_iter().flatten() {
        let path = at.key("set").key(name);
        let lower = name.to_ascii_lowercase();
        if crate::headers::always_denied(&lower) {
            errors.push(ConfigError::new(
                &path,
                DetailCode::Conflict,
                "the header is on the always-deny list",
            ));
            continue;
        }
        let parsed = (
            HeaderName::from_bytes(lower.as_bytes()),
            value.as_str().map(HeaderValue::from_str),
        );
        match parsed {
            (Ok(n), Some(Ok(v))) => set.push((n, v)),
            _ => errors.push(ConfigError::new(
                &path,
                DetailCode::InvalidFormat,
                "not a valid header",
            )),
        }
    }
    if errors.is_empty() {
        Ok(HeaderBlock {
            forward,
            deny,
            set,
            response_forward,
        })
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn paths(doc: &Value) -> Vec<String> {
        check_document(doc).into_iter().map(|e| e.path).collect()
    }

    #[test]
    fn allow_lists_cannot_lift_always_denied_ranges() {
        let doc = json!({"security": {"egress": {"client_influenced": {"allow_cidrs": ["169.254.0.0/16", "10.0.0.0/8"]},
                                                  "operator": {"allow_cidrs": ["0.0.0.0/0", "169.254.169.254/32"]}}}});
        assert_eq!(
            paths(&doc),
            vec![
                "security.egress.operator.allow_cidrs[1]",
                "security.egress.client_influenced.allow_cidrs[0]"
            ]
        );
        let ok = json!({"security": {"egress": {"client_influenced": {"allow_cidrs": ["10.0.0.0/8"], "allowed_ports": [443, 8443]},
                                                 "metadata_hosts_extra": ["meta.example", "10.9.9.9"]}}});
        let p = compile_policy(&ok).unwrap();
        assert_eq!(p.client_ports, vec![443, 8443]);
        assert_eq!(p.metadata_names, vec!["meta.example".to_owned()]);
        assert_eq!(p.metadata_addrs, vec!["10.9.9.9".parse::<IpAddr>().unwrap()]);
    }

    #[test]
    fn operator_urls_and_header_rules() {
        let doc = json!({
            "security": {"egress": {"metadata_hosts_extra": ["meta.example"]}},
            "providers": {"a": {"base_url": "http://127.1:9100"}, "b": {"base_url": "https://meta.example/v1"},
                          "c": {"base_url": "http://127.0.0.1:9100/v1"}, "d": {"base_url": "https://{region}.example"}},
            "mcp": {"servers": {"s": {"transport": "streamable_http", "url": "http://169.254.169.254/mcp"},
                                "t": {"transport": "streamable_http", "url": "http://127.0.0.1:9100/mcp"}}},
            "a2a": {"agents": {"w": {"card_url": "http://metadata.google.internal/card", "url": "http://127.0.0.1:9/a"}}},
            "policies": {"headers": {"forward": ["cookie", "x-custom-*"], "set": {"authorization": "x", "x-team": "a"},
                                     "response_forward": ["authorization", "set-cookie"], "deny": ["x-*-a"]}},
            "models": {"m": {"policies": {"headers": {"forward": ["x-amz-*"]}}}}
        });
        assert_eq!(
            paths(&doc),
            vec![
                "providers.a.base_url",
                "providers.b.base_url",
                "mcp.servers.s.url",
                "a2a.agents.w.card_url",
                "policies.headers.forward[0]",
                "policies.headers.deny[0]",
                "policies.headers.response_forward[0]",
                "policies.headers.set.authorization",
                "models.m.policies.headers.forward[0]",
            ]
        );
    }

    #[test]
    fn header_blocks_compile() {
        let b = header_block(
            &json!({"forward": ["x-custom-*"], "deny": ["x-custom-secret"], "set": {"X-Team": "search"}}),
            &DocPath::root(),
        )
        .unwrap();
        assert_eq!(b.forward, Some(vec![Pattern::Prefix("x-custom-".into())]));
        assert_eq!(b.set.len(), 1);
        assert!(b.response_forward.is_none());
    }
}
