//! Egress header hygiene: the always-deny list, the forward and deny patterns of `policies.headers`,
//! static headers, and the upstream response headers a client may see.

use http::header::{ACCEPT_ENCODING, CONNECTION, CONTENT_TYPE, USER_AGENT};
use http::{HeaderMap, HeaderName, HeaderValue};

/// A header-name pattern: an exact name, a prefix ending in `*`, or `*`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Pattern {
    Exact(String),
    Prefix(String),
    Any,
}

/// Why a pattern is not accepted.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PatternError {
    #[error("a pattern has at most one `*`, as its last character or alone")]
    Star,
    #[error("a pattern must be a valid header name")]
    Name,
}

impl Pattern {
    pub fn parse(raw: &str) -> Result<Pattern, PatternError> {
        let p = raw.to_ascii_lowercase();
        if p == "*" {
            return Ok(Pattern::Any);
        }
        let stars = p.matches('*').count();
        let (body, prefix) = match p.strip_suffix('*') {
            Some(b) if stars == 1 => (b.to_owned(), true),
            None if stars == 0 => (p, false),
            _ => return Err(PatternError::Star),
        };
        if body.is_empty() || HeaderName::from_bytes(body.as_bytes()).is_err() {
            return Err(PatternError::Name);
        }
        Ok(if prefix {
            Pattern::Prefix(body)
        } else {
            Pattern::Exact(body)
        })
    }

    /// Whether the pattern matches a lowercase header name.
    #[must_use]
    pub fn matches(&self, name: &str) -> bool {
        match self {
            Pattern::Any => true,
            Pattern::Exact(n) => n == name,
            Pattern::Prefix(p) => name.starts_with(p.as_str()),
        }
    }

    /// Whether some header on the always-deny list matches this pattern, so forwarding it would
    /// try to forward a header that can never be forwarded. `*` is the one exception: it means
    /// every header that is not on the list.
    #[must_use]
    pub fn names_always_denied(&self) -> bool {
        match self {
            Pattern::Any => false,
            Pattern::Exact(n) => always_denied(n),
            Pattern::Prefix(p) => {
                always_denied(p)
                    || ALWAYS_DENY.iter().any(|d| d.starts_with(p.as_str()))
                    || DENY_PREFIXES
                        .iter()
                        .any(|d| d.starts_with(p.as_str()) || p.starts_with(d))
            }
        }
    }
}

/// Hop-by-hop, framing, credential, account-selection, gateway-namespace and client-addressing
/// headers: never forwarded from a client, whatever the configuration.
const ALWAYS_DENY: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "host",
    "content-length",
    "expect",
    "content-encoding",
    "accept-encoding",
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "api-key",
    "x-goog-api-key",
    "ocp-apim-subscription-key",
    "cookie",
    "x-goog-user-project",
    "openai-organization",
    "openai-project",
    "forwarded",
    "x-real-ip",
    "true-client-ip",
    "cf-connecting-ip",
    "x-client-ip",
    "client-cert",
    "client-cert-chain",
    "sec-websocket-protocol",
];

const DENY_PREFIXES: &[&str] = &["x-amz-", "x-antirion-", "x-forwarded-"];

/// Name endings that mark a header as a credential of some service (`x-…-api-key`, `…-auth`, a
/// virtual-key header): denied as a class, so credentials of other gateways a client talks to are
/// never passed on.
const CREDENTIAL_SUFFIXES: &[&str] = &[
    "-api-key",
    "-api-key-id",
    "-virtual-key",
    "-vk",
    "-auth",
    "-authorization",
];

/// Whether a lowercase header name is on the always-deny list.
#[must_use]
pub fn always_denied(name: &str) -> bool {
    ALWAYS_DENY.contains(&name)
        || DENY_PREFIXES.iter().any(|p| name.starts_with(p))
        || CREDENTIAL_SUFFIXES.iter().any(|s| name.ends_with(s))
}

/// Upstream response headers a client never receives, even when `response_forward` lists them.
const RESPONSE_NEVER: &[&str] = &[
    "set-cookie",
    "set-cookie2",
    "server",
    "via",
    "x-powered-by",
    "alt-svc",
    "strict-transport-security",
    "www-authenticate",
    "proxy-authenticate",
    "openai-organization",
    "openai-project",
    "connection",
    "keep-alive",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "content-length",
    "content-encoding",
    "x-request-id",
    "request-id",
];

/// Whether a lowercase upstream response header can never be passed to a client.
#[must_use]
pub fn response_never(name: &str) -> bool {
    RESPONSE_NEVER.contains(&name) || name.starts_with("access-control-") || name.starts_with("x-antirion-")
}

/// The `headers` member of one `policies` block.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeaderBlock {
    /// `None` when the level does not set it.
    pub forward: Option<Vec<Pattern>>,
    pub deny: Vec<Pattern>,
    pub set: Vec<(HeaderName, HeaderValue)>,
    pub response_forward: Option<Vec<Pattern>>,
}

/// The header rules in effect for one request, merged from every level that applies.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeaderRules {
    pub forward: Vec<Pattern>,
    pub deny: Vec<Pattern>,
    pub set: Vec<(HeaderName, HeaderValue)>,
    pub response_forward: Vec<Pattern>,
}

impl HeaderRules {
    /// Merges blocks given broadest first: `forward` and `response_forward` come from the narrowest
    /// level that sets them, `deny` accumulates, and `set` merges name by name with the narrowest
    /// value winning.
    #[must_use]
    pub fn merge<'a>(blocks: impl IntoIterator<Item = &'a HeaderBlock>) -> HeaderRules {
        let mut out = HeaderRules::default();
        for b in blocks {
            if let Some(f) = &b.forward {
                out.forward.clone_from(f);
            }
            if let Some(f) = &b.response_forward {
                out.response_forward.clone_from(f);
            }
            out.deny.extend(b.deny.iter().cloned());
            for (name, value) in &b.set {
                out.set.retain(|(n, _)| n != name);
                out.set.push((name.clone(), value.clone()));
            }
        }
        out
    }

    fn forwards(&self, name: &str) -> bool {
        !always_denied(name)
            && self.forward.iter().any(|p| p.matches(name))
            && !self.deny.iter().any(|p| p.matches(name))
    }
}

/// The settings of one upstream request that the gateway owns.
#[derive(Debug)]
pub struct Outbound<'a> {
    pub rules: &'a HeaderRules,
    pub user_agent: &'a HeaderValue,
    /// Streaming requests ask for `identity`, so events are never held back by a compressor.
    pub stream: bool,
}

/// Adds to `upstream` the client headers the rules forward, then the static `set` headers, then
/// the headers the gateway owns (`user-agent`, `accept-encoding`). Headers `upstream` already
/// carries (the credential, provider headers, `content-type`, the request id) are kept.
pub fn outbound(upstream: &mut HeaderMap, client: &HeaderMap, o: &Outbound<'_>) {
    let built: Vec<HeaderName> = upstream.keys().cloned().collect();
    let hop: Vec<String> = client
        .get_all(CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|t| t.trim().to_ascii_lowercase())
        .collect();
    for (name, value) in client {
        let n = name.as_str();
        if built.contains(name) || hop.iter().any(|h| h == n) || !o.rules.forwards(n) {
            continue;
        }
        upstream.append(name.clone(), value.clone());
    }
    for (name, value) in &o.rules.set {
        if !built.contains(name) {
            upstream.insert(name.clone(), value.clone());
        }
    }
    if !upstream.contains_key(USER_AGENT) {
        upstream.insert(USER_AGENT, o.user_agent.clone());
    }
    let coding = if o.stream {
        HeaderValue::from_static("identity")
    } else {
        HeaderValue::from_static("gzip, deflate, br, zstd")
    };
    upstream.insert(ACCEPT_ENCODING, coding);
}

/// The upstream response headers a client receives: `content-type` and what `response_forward`
/// lists, never the forbidden ones.
#[must_use]
pub fn inbound(upstream: &HeaderMap, rules: &HeaderRules) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (name, value) in upstream {
        let n = name.as_str();
        let listed = name == CONTENT_TYPE || rules.response_forward.iter().any(|p| p.matches(n));
        if listed && !response_never(n) {
            out.append(name.clone(), value.clone());
        }
    }
    out
}

/// The upstream's request id: `x-request-id`, `request-id` or `x-amzn-requestid`.
#[must_use]
pub fn upstream_request_id(upstream: &HeaderMap) -> Option<&HeaderValue> {
    ["x-request-id", "request-id", "x-amzn-requestid"]
        .iter()
        .find_map(|n| upstream.get(*n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pats(list: &[&str]) -> Vec<Pattern> {
        list.iter().map(|p| Pattern::parse(p).unwrap()).collect()
    }

    #[test]
    fn patterns() {
        for bad in ["x-*-a", "*x", "x-**", "x-a*b", "", "bad name*"] {
            assert!(Pattern::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(
            Pattern::parse("X-Custom-*").unwrap(),
            Pattern::Prefix("x-custom-".into())
        );
        assert!(Pattern::parse("x-custom-*").unwrap().matches("x-custom-a"));
        assert!(!Pattern::parse("x-custom-a").unwrap().matches("x-custom-ab"));
        for denied in [
            "cookie",
            "authorization",
            "x-amz-*",
            "x-amz-date",
            "x-antirion-*",
            "x-forwarded-for",
            "connection",
            "host",
            "openai-organization",
            "vendor-auth",
            "x-a*",
            "sec-websocket-protocol",
        ] {
            assert!(Pattern::parse(denied).unwrap().names_always_denied(), "{denied}");
        }
        for ok in ["*", "x-custom-*", "user-agent", "x-team"] {
            assert!(!Pattern::parse(ok).unwrap().names_always_denied(), "{ok}");
        }
    }

    #[test]
    fn merging_levels() {
        let gateway = HeaderBlock {
            forward: Some(pats(&["x-a", "x-b"])),
            deny: pats(&["x-b"]),
            set: vec![(HeaderName::from_static("x-s"), HeaderValue::from_static("1"))],
            response_forward: None,
        };
        let model = HeaderBlock {
            forward: Some(pats(&["x-b", "x-c"])),
            set: vec![(HeaderName::from_static("x-s"), HeaderValue::from_static("2"))],
            ..HeaderBlock::default()
        };
        let r = HeaderRules::merge([&gateway, &model]);
        assert!(!r.forwards("x-a"));
        assert!(!r.forwards("x-b"));
        assert!(r.forwards("x-c"));
        assert_eq!(
            r.set,
            vec![(HeaderName::from_static("x-s"), HeaderValue::from_static("2"))]
        );
    }

    #[test]
    fn outbound_headers() {
        let rules = HeaderRules {
            forward: pats(&["*"]),
            deny: pats(&["x-secret"]),
            set: vec![(
                HeaderName::from_static("x-team"),
                HeaderValue::from_static("search"),
            )],
            response_forward: Vec::new(),
        };
        let mut client = HeaderMap::new();
        for (k, v) in [
            ("cookie", "c"),
            ("x-vendor-api-key", "k"),
            ("x-bf-vk", "k"),
            ("x-amz-date", "d"),
            ("connection", "keep-alive, x-foo"),
            ("x-foo", "1"),
            ("x-secret", "s"),
            ("x-custom", "ok"),
            ("x-team", "spoofed"),
            ("user-agent", "client/1"),
            ("accept-encoding", "br"),
        ] {
            client.insert(HeaderName::from_static(k), HeaderValue::from_static(v));
        }
        let mut up = HeaderMap::new();
        up.insert("authorization", HeaderValue::from_static("Bearer up"));
        let ua = HeaderValue::from_static("antirion/0");
        outbound(
            &mut up,
            &client,
            &Outbound {
                rules: &rules,
                user_agent: &ua,
                stream: true,
            },
        );
        let names: Vec<&str> = up.keys().map(HeaderName::as_str).collect();
        for gone in [
            "cookie",
            "x-vendor-api-key",
            "x-bf-vk",
            "x-amz-date",
            "connection",
            "x-foo",
            "x-secret",
        ] {
            assert!(!names.contains(&gone), "{gone}: {names:?}");
        }
        assert_eq!(up["authorization"], "Bearer up");
        assert_eq!(up["x-custom"], "ok");
        assert_eq!(up["x-team"], "search");
        assert_eq!(up["user-agent"], "client/1");
        assert_eq!(up["accept-encoding"], "identity");
    }

    #[test]
    fn inbound_headers() {
        let mut up = HeaderMap::new();
        for (k, v) in [
            ("content-type", "application/json"),
            ("set-cookie", "a"),
            ("server", "nginx"),
            ("x-foo", "1"),
            ("x-bar", "2"),
            ("access-control-allow-origin", "*"),
        ] {
            up.insert(HeaderName::from_static(k), HeaderValue::from_static(v));
        }
        let rules = HeaderRules {
            response_forward: pats(&["x-foo", "set-cookie", "*"]),
            ..HeaderRules::default()
        };
        let out = inbound(&up, &rules);
        assert!(out.contains_key("x-foo") && out.contains_key("x-bar") && out.contains_key("content-type"));
        assert!(!out.contains_key("set-cookie") && !out.contains_key("server"));
        assert!(!out.contains_key("access-control-allow-origin"));
        let none = inbound(&up, &HeaderRules::default());
        assert_eq!(none.len(), 1);
    }
}
