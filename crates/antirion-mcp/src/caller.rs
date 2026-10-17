//! The authenticated caller: the identity attributes an upstream may receive, and the grants of its
//! policy chain.

use http::{HeaderMap, HeaderValue};

use crate::compile::{Level, McpGrant, McpSnapshot};

/// Who is calling, as authentication established it.
#[derive(Clone, Debug, Default)]
pub struct Caller {
    /// The principal a session is bound to: the virtual key's id.
    pub principal: String,
    pub key_id: String,
    pub key_name: String,
    pub organization: String,
    pub team: String,
    pub project: String,
    pub tags: Vec<String>,
    pub end_user: Option<String>,
    /// The claims of the JWT the caller presented.
    pub jwt: Option<serde_json::Map<String, serde_json::Value>>,
}

/// The values of a JWT claim: a string, number or boolean is one value, an array its scalar items.
fn claim_values(v: &serde_json::Value) -> Vec<String> {
    use serde_json::Value;
    match v {
        Value::String(s) => vec![s.clone()],
        Value::Number(n) => vec![n.to_string()],
        Value::Bool(b) => vec![b.to_string()],
        Value::Array(items) => items
            .iter()
            .filter(|i| !i.is_array() && !i.is_object())
            .flat_map(claim_values)
            .collect(),
        _ => Vec::new(),
    }
}

impl Caller {
    /// Every value of an identity attribute (several for tags and array claims); empty when absent.
    pub(crate) fn values(&self, name: &str) -> Vec<String> {
        if let Some(claim) = name.strip_prefix("jwt.") {
            return self
                .jwt
                .as_ref()
                .and_then(|c| c.get(claim))
                .map(claim_values)
                .unwrap_or_default()
                .into_iter()
                .filter(|v| !v.is_empty())
                .collect();
        }
        if name == "tags" {
            return self.tags.clone();
        }
        self.attribute(name).into_iter().collect()
    }

    fn attribute(&self, name: &str) -> Option<String> {
        if name.starts_with("jwt.") {
            let values = self.values(name);
            return (!values.is_empty()).then(|| values.join(","));
        }
        let v = match name {
            "key_id" => self.key_id.clone(),
            "key_name" => self.key_name.clone(),
            "project" => self.project.clone(),
            "team" => self.team.clone(),
            "organization" => self.organization.clone(),
            "end_user" | "user" => self.end_user.clone()?,
            "tags" => {
                if self.tags.is_empty() {
                    return None;
                }
                self.tags.join(",")
            }
            _ => return None,
        };
        (!v.is_empty()).then_some(v)
    }

    /// Removes every header named in `mapping` and sets the ones whose attribute has a value.
    pub(crate) fn apply_identity(&self, mapping: &[(http::HeaderName, String)], headers: &mut HeaderMap) {
        for (name, attr) in mapping {
            headers.remove(name);
            if let Some(v) = self.attribute(attr)
                && let Ok(v) = HeaderValue::from_str(&percent_encode(&v))
            {
                headers.insert(name.clone(), v);
            }
        }
    }

    /// The caller's policy levels, broadest first.
    pub(crate) fn levels<'a>(&self, snap: &'a McpSnapshot) -> Vec<&'a Level> {
        let g = &snap.grants;
        [
            Some(&g.gateway),
            g.organizations.get(&self.organization),
            g.teams.get(&self.team),
            g.projects.get(&self.project),
            g.keys.get(&self.key_name),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    pub(crate) fn mcp_grants<'a>(&self, snap: &'a McpSnapshot) -> Vec<&'a McpGrant> {
        self.levels(snap)
            .into_iter()
            .filter_map(|l| l.mcp.as_ref())
            .collect()
    }

    /// Whether the caller may use MCP server `server`: some level grants servers, and every level that
    /// lists servers includes it.
    pub(crate) fn server_granted(grants: &[&McpGrant], server: &str) -> bool {
        let mut any = false;
        for g in grants {
            if let Some(list) = &g.servers {
                any = true;
                if !list.iter().any(|s| s == "*" || s == server) {
                    return false;
                }
            }
        }
        any
    }

    /// Whether the caller may use agent `agent`.
    pub(crate) fn agent_granted(&self, snap: &McpSnapshot, agent: &str) -> bool {
        let mut any = false;
        for l in self.levels(snap) {
            if let Some(list) = &l.agents {
                any = true;
                if !list.iter().any(|s| s == "*" || s == agent) {
                    return false;
                }
            }
        }
        any
    }
}

/// Values with bytes outside visible ASCII and space are percent-encoded, keeping the unreserved set.
fn percent_encode(v: &str) -> String {
    if v.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        return v.to_owned();
    }
    let mut out = String::with_capacity(v.len() * 3);
    for b in v.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use http::HeaderName;

    use super::*;

    #[test]
    fn identity_headers_replace_client_values() {
        let caller = Caller {
            project: "default".into(),
            tags: vec!["a".into(), "b".into()],
            ..Caller::default()
        };
        let mut h = HeaderMap::new();
        h.insert("x-user-id", HeaderValue::from_static("attacker"));
        let map = vec![
            (HeaderName::from_static("x-user-id"), "end_user".to_owned()),
            (HeaderName::from_static("x-proj"), "project".to_owned()),
            (HeaderName::from_static("x-tags"), "tags".to_owned()),
        ];
        caller.apply_identity(&map, &mut h);
        assert!(h.get("x-user-id").is_none());
        assert_eq!(h["x-proj"], "default");
        assert_eq!(h["x-tags"], "a,b");
    }

    #[test]
    fn server_grants_intersect() {
        let all = McpGrant {
            servers: Some(vec!["*".into()]),
            ..McpGrant::default()
        };
        let only_t = McpGrant {
            servers: Some(vec!["t".into()]),
            ..McpGrant::default()
        };
        assert!(Caller::server_granted(&[&all], "s"));
        assert!(!Caller::server_granted(&[&all, &only_t], "s"));
        assert!(Caller::server_granted(&[&all, &only_t], "t"));
        assert!(!Caller::server_granted(&[], "t"));
    }

    #[test]
    fn non_ascii_values_are_percent_encoded() {
        assert_eq!(percent_encode("ana maria"), "ana maria");
        assert_eq!(percent_encode("jos\u{e9}"), "jos%C3%A9");
    }
}
