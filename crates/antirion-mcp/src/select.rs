//! A request's selected servers (gateway servers ∩ granted ∩ narrowed), item visibility, and the
//! upstream headers each server may receive.

use std::sync::Arc;

use antirion_core::Stage;
use antirion_error::Message;
use http::{HeaderMap, HeaderName};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::caller::Caller;
use crate::compile::{McpGrant, ServerRt};
use crate::fingerprint;
use crate::http::Req;
use crate::reasons;
use crate::rpc::Fail;

/// The kind of a listed or routed item.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum ItemKind {
    Tool,
    Prompt,
    Resource,
}

/// The servers one request may reach and what it may see on them.
#[derive(Clone, Debug)]
pub(crate) struct Selection {
    pub(crate) servers: Vec<Arc<ServerRt>>,
    /// A narrowing header was sent, so the result depends on the request.
    pub(crate) narrowed: bool,
    tools_filter: Option<Vec<String>>,
    grants: Vec<McpGrant>,
    /// The matching conditional rules, one list per level that has rules; each level needs one of
    /// its rules to grant.
    conditional: Vec<Vec<crate::when::Rule>>,
}

fn list_header(req: &Req, name: &str) -> Option<Vec<String>> {
    req.header(name).map(|v| {
        v.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    })
}

impl Selection {
    /// Computes the selection; a caller with no granted server on this gateway is refused.
    pub(crate) fn new(req: &Req) -> Result<Selection, Fail> {
        let snap = &req.env.snap;
        let grants: Vec<McpGrant> = req.caller.mcp_grants(snap).into_iter().cloned().collect();
        let refs: Vec<&McpGrant> = grants.iter().collect();
        let conditional = crate::when::matching(&grants, &req.caller);
        let granted: Vec<Arc<ServerRt>> = req
            .gw
            .servers
            .iter()
            .filter_map(|s| snap.server(s))
            .filter(|s| {
                let by_rules = conditional
                    .iter()
                    .all(|rules| rules.iter().any(|r| r.grants_server(&s.name)));
                let by_levels = Caller::server_granted(&refs, &s.name)
                    || (!conditional.is_empty() && refs.iter().all(|g| g.servers.is_none()));
                by_rules && by_levels
            })
            .cloned()
            .collect();
        let configured = req.gw.servers.iter().any(|s| snap.server(s).is_some());
        if granted.is_empty() && configured {
            return Err(Fail::gateway(
                reasons::MCP_NO_SERVER_GRANTED,
                Stage::Resolve,
                Message::Fixed("No MCP server of this gateway is granted to the caller."),
            ));
        }
        let narrow = list_header(req, "x-antirion-mcp-servers");
        let tools_filter = list_header(req, "x-antirion-mcp-tools");
        let servers = match &narrow {
            Some(names) => granted.into_iter().filter(|s| names.contains(&s.name)).collect(),
            None => granted,
        };
        Ok(Selection {
            servers,
            narrowed: narrow.is_some() || tools_filter.is_some(),
            tools_filter,
            grants,
            conditional,
        })
    }

    fn granted_item(&self, server: &str, kind: ItemKind, item: &str) -> bool {
        let by_rules = self
            .conditional
            .iter()
            .all(|rules| rules.iter().any(|r| r.grants_item(server, kind, item)));
        by_rules
            && self.grants.iter().all(|g| {
                let lists = match kind {
                    ItemKind::Tool => &g.tools,
                    ItemKind::Prompt => &g.prompts,
                    ItemKind::Resource => &g.resources,
                };
                lists
                    .get(server)
                    .is_none_or(|globs| crate::glob::any(globs, item))
            })
    }

    /// Whether the caller sees an item: static filters, item grants, the tools header (on the
    /// client-facing name) and, for pinned servers, the approved definition.
    pub(crate) fn visible(
        &self,
        state: &crate::state::McpState,
        server: &ServerRt,
        kind: ItemKind,
        item: &str,
        client_name: &str,
        definition: Option<&Value>,
    ) -> bool {
        let filter = match kind {
            ItemKind::Tool => &server.tools,
            ItemKind::Prompt => &server.prompts,
            ItemKind::Resource => &server.resources,
        };
        if !filter.admits(item) || !self.granted_item(&server.name, kind, item) {
            return false;
        }
        if kind == ItemKind::Tool {
            if let Some(globs) = &self.tools_filter
                && !crate::glob::any(globs, client_name)
            {
                return false;
            }
            if let Some(def) = definition
                && fingerprint::unapproved(state, server, def)
            {
                return false;
            }
        }
        true
    }

    /// Whether a list result depends on who asks (caching scope).
    pub(crate) fn private(&self, req: &Req) -> bool {
        self.narrowed || req.gw.restricted
    }
}

/// The headers a server receives on this request: allow-listed per-server client headers, then the
/// identity headers (which always win over client values).
pub(crate) fn upstream_headers(req: &Req, server: &ServerRt) -> HeaderMap {
    let mut out = HeaderMap::new();
    let prefix = format!("x-antirion-mcp-{}-", server.name);
    for (name, value) in &req.headers {
        let Some(rest) = name.as_str().strip_prefix(&prefix) else {
            continue;
        };
        if server.client_headers.iter().any(|h| h == rest)
            && let Ok(n) = HeaderName::from_bytes(rest.as_bytes())
        {
            out.insert(n, value.clone());
        }
    }
    req.caller.apply_identity(&server.identity_headers, &mut out);
    out
}

/// A fingerprint of the credential identity a server sees (for list caches).
pub(crate) fn credential(headers: &HeaderMap) -> u64 {
    let mut pairs: Vec<(String, Vec<u8>)> = headers
        .iter()
        .map(|(n, v)| (n.as_str().to_owned(), v.as_bytes().to_vec()))
        .collect();
    pairs.sort();
    let mut h = Sha256::new();
    for (n, v) in pairs {
        h.update(n.as_bytes());
        h.update([0]);
        h.update(&v);
        h.update([0]);
    }
    let digest = h.finalize();
    let mut first = [0u8; 8];
    first.copy_from_slice(digest.get(..8).unwrap_or(&[0; 8]));
    u64::from_be_bytes(first)
}
