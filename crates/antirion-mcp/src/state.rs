//! Node-local MCP state that outlives requests and generations: the generated session key, list,
//! discovery and card caches, the revocation set, held HTTP+SSE sessions, requests in flight, and tool
//! fingerprints with their approvals. Nothing here is needed to resume a session on another node.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use antirion_core::UtcMicros;
use parking_lot::{Mutex, RwLock};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::compile::SessionKey;
use crate::ssetransport::SseConn;

/// Caching hints one server attached to a modern result.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Hints {
    pub(crate) ttl_ms: i64,
    pub(crate) public: bool,
}

impl Hints {
    pub(crate) fn of(result: &Value) -> Hints {
        Hints {
            ttl_ms: result.get("ttlMs").and_then(Value::as_i64).unwrap_or(0).max(0),
            public: result.get("cacheScope").and_then(Value::as_str) == Some("public"),
        }
    }
}

/// One server's complete list of one kind, read across all upstream pages.
#[derive(Clone, Debug)]
pub(crate) struct ListEntry {
    pub(crate) items: Arc<Vec<Value>>,
    /// Size of the first upstream page, when the server paged its list.
    pub(crate) page: Option<usize>,
    pub(crate) hints: Hints,
    pub(crate) at: UtcMicros,
}

/// Cache key of a list: the server's generation, the credential identity and the method.
pub(crate) type ListKey = (u64, u64, &'static str);

/// What discovery learned about a server.
#[derive(Clone, Debug)]
pub(crate) struct Discovered {
    pub(crate) modern: bool,
    pub(crate) at: UtcMicros,
}

/// Per-server facts for the status endpoint.
#[derive(Clone, Debug, Default)]
pub(crate) struct ServerFacts {
    pub(crate) last_discovery_at: Option<UtcMicros>,
    pub(crate) last_error: Option<String>,
    pub(crate) modern: Option<bool>,
    /// The last attempt to initialize the server or read its tools, and why it failed.
    pub(crate) last_contact_at: Option<UtcMicros>,
    pub(crate) last_contact_error: Option<String>,
}

/// Approved tool fingerprints of one server.
#[derive(Clone, Debug)]
pub(crate) struct Approval {
    pub(crate) tools: BTreeMap<String, String>,
    pub(crate) approved_at: UtcMicros,
}

/// A tool definition that changed upstream since the server's previous list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefinitionChange {
    pub server: String,
    pub tool: String,
    /// `added`, `removed` or `changed`.
    pub change: &'static str,
    pub old_fingerprint: Option<String>,
    pub new_fingerprint: Option<String>,
}

/// Session counters of one gateway.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct GatewayCounts {
    pub(crate) issued: u64,
    pub(crate) rejected: u64,
}

/// The MCP state of one node.
#[derive(Debug, Default)]
pub struct McpState {
    generated_key: RwLock<Option<[u8; 32]>>,
    pub(crate) lists: Mutex<HashMap<ListKey, ListEntry>>,
    pub(crate) discovery: Mutex<HashMap<u64, Discovered>>,
    pub(crate) facts: Mutex<HashMap<String, ServerFacts>>,
    pub(crate) cards: Mutex<HashMap<u64, (UtcMicros, Value)>>,
    revoked: Mutex<HashMap<[u8; 32], i64>>,
    inflight: Mutex<HashMap<(String, String), String>>,
    pub(crate) sse: Mutex<HashMap<String, Arc<SseConn>>>,
    pub(crate) fingerprints: Mutex<HashMap<String, BTreeMap<String, String>>>,
    pub(crate) approvals: Mutex<HashMap<String, Approval>>,
    pub(crate) definition_changes: Mutex<BTreeMap<(String, &'static str), u64>>,
    /// Changes not yet reported as metrics and alert events.
    pub(crate) unreported: Mutex<Vec<DefinitionChange>>,
    pub(crate) counts: Mutex<HashMap<String, GatewayCounts>>,
    /// Client-facing HTTP+SSE streams open on this node, by handle.
    pub(crate) client_streams: Mutex<HashMap<String, Arc<crate::clientsse::ClientStream>>>,
    /// Authorization-server metadata by issuer, with when it was read.
    pub(crate) issuers: Mutex<HashMap<String, (UtcMicros, Value)>>,
    /// The process pools of `stdio` servers, by server generation.
    stdio: Mutex<HashMap<u64, Arc<crate::stdio::StdioServer>>>,
}

/// The id byte of the node's generated session key.
const GENERATED_KEY_ID: u8 = 0;

impl McpState {
    /// Records the outcome of an attempt to initialize `server` or read its tools.
    pub(crate) fn record_contact(&self, server: &str, at: UtcMicros, error: Option<String>) {
        let mut facts = self.facts.lock();
        let f = facts.entry(server.to_owned()).or_default();
        f.last_contact_at = Some(at);
        f.last_contact_error = error;
    }

    /// Installs the session key generated for this deployment (read or created in the store at start).
    pub fn set_generated_key(&self, key: [u8; 32]) {
        *self.generated_key.write() = Some(key);
    }

    /// The keys of a request: the configured ones, else the generated one.
    pub(crate) fn keys(&self, configured: &[SessionKey]) -> Vec<SessionKey> {
        if !configured.is_empty() {
            return configured.to_vec();
        }
        self.generated_key
            .read()
            .map(|secret| {
                vec![SessionKey {
                    id: GENERATED_KEY_ID,
                    secret,
                }]
            })
            .unwrap_or_default()
    }

    /// Drops the list, discovery and agent-card caches, and what tool fingerprints were seen and approved.
    pub fn clear_caches(&self) {
        self.issuers.lock().clear();
        self.fingerprints.lock().clear();
        self.unreported.lock().clear();
        self.approvals.lock().clear();
        self.lists.lock().clear();
        self.discovery.lock().clear();
        self.cards.lock().clear();
    }

    /// Closes held HTTP+SSE upstream sessions and forgets revocations and session counters.
    pub fn clear_sessions(&self) {
        for conn in self.sse.lock().drain().map(|(_, c)| c) {
            conn.close();
        }
        self.revoked.lock().clear();
        self.counts.lock().clear();
    }

    /// The tool definition changes seen since the last call.
    pub fn take_definition_changes(&self) -> Vec<DefinitionChange> {
        std::mem::take(&mut *self.unreported.lock())
    }

    /// The process pool of a `stdio` server. Pools of generations no server of `snap` has any more
    /// are dropped, which stops their processes.
    pub(crate) fn stdio_pool(
        &self,
        snap: &crate::compile::McpSnapshot,
        server: &crate::compile::ServerRt,
        spec: &crate::compile::StdioSpec,
    ) -> Arc<crate::stdio::StdioServer> {
        let mut pools = self.stdio.lock();
        if let Some(p) = pools.get(&server.generation) {
            return Arc::clone(p);
        }
        pools.retain(|g, _| {
            snap.servers
                .values()
                .any(|s| s.generation == *g && s.stdio.is_some())
        });
        let pool = Arc::new(crate::stdio::StdioServer::new(
            spec.command.clone(),
            spec.args.clone(),
            spec.env.clone(),
            spec.processes,
        ));
        pools.insert(server.generation, Arc::clone(&pool));
        pool
    }

    /// How many processes the pool of the `stdio` server with `generation` has started.
    pub(crate) fn stdio_started(&self, generation: u64) -> Option<(u64, usize)> {
        let pools = self.stdio.lock();
        let p = pools.get(&generation)?;
        Some((p.started.load(std::sync::atomic::Ordering::Relaxed), p.running()))
    }

    pub(crate) fn revoke(&self, token: &str, exp: i64, now: UtcMicros) {
        let mut set = self.revoked.lock();
        let now_s = now.0 / 1_000_000;
        set.retain(|_, e| *e > now_s);
        set.insert(digest(token), exp);
    }

    pub(crate) fn is_revoked(&self, token: &str) -> bool {
        self.revoked.lock().contains_key(&digest(token))
    }

    /// Every unexpired revocation (the token's digest and its expiry in seconds), to share with
    /// other nodes.
    #[must_use]
    pub fn revocations(&self) -> Vec<([u8; 32], i64)> {
        self.revoked.lock().iter().map(|(d, e)| (*d, *e)).collect()
    }

    /// The digest a session token is revoked under, and the revocation's expiry when this node
    /// knows it.
    #[must_use]
    pub fn revocation_of(&self, token: &str) -> ([u8; 32], Option<i64>) {
        let d = digest(token);
        let exp = self.revoked.lock().get(&d).copied();
        (d, exp)
    }

    /// Adds revocations another node made.
    pub fn add_revocations(&self, items: impl IntoIterator<Item = ([u8; 32], i64)>) {
        self.revoked.lock().extend(items);
    }

    pub(crate) fn revoked_count(&self) -> usize {
        self.revoked.lock().len()
    }

    /// Records that a client request (by session and id) is executing on `server`.
    pub(crate) fn begin(self: &Arc<Self>, session: &str, id: &Value, server: &str) -> InFlight {
        let key = (hex(&digest(session)), id.to_string());
        self.inflight.lock().insert(key.clone(), server.to_owned());
        InFlight {
            state: Arc::clone(self),
            key,
        }
    }

    pub(crate) fn executing(&self, session: &str, id: &Value) -> Option<String> {
        self.inflight
            .lock()
            .get(&(hex(&digest(session)), id.to_string()))
            .cloned()
    }

    pub(crate) fn list(&self, key: &ListKey, now: UtcMicros, ttl: Duration) -> Option<ListEntry> {
        let lists = self.lists.lock();
        let e = lists.get(key)?;
        (now.since(e.at) < ttl && now >= e.at).then(|| e.clone())
    }

    pub(crate) fn invalidate(&self, generation: u64, method: &'static str) {
        self.lists
            .lock()
            .retain(|(g, _, m), _| !(*g == generation && *m == method));
    }

    pub(crate) fn count(&self, gateway: &str, issued: bool) {
        let mut counts = self.counts.lock();
        let c = counts.entry(gateway.to_owned()).or_default();
        if issued {
            c.issued += 1;
        } else {
            c.rejected += 1;
        }
    }
}

/// Removes its in-flight record when the request ends, however it ends.
#[derive(Debug)]
pub(crate) struct InFlight {
    state: Arc<McpState>,
    key: (String, String),
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.state.inflight.lock().remove(&self.key);
    }
}

pub(crate) fn digest(s: &str) -> [u8; 32] {
    Sha256::digest(s.as_bytes()).into()
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revocation_and_inflight() {
        let s = Arc::new(McpState::default());
        s.revoke("tok", 100, UtcMicros(0));
        assert!(s.is_revoked("tok") && !s.is_revoked("other"));
        {
            let _g = s.begin("tok", &Value::from(77), "a");
            assert_eq!(s.executing("tok", &Value::from(77)).as_deref(), Some("a"));
        }
        assert!(s.executing("tok", &Value::from(77)).is_none());
    }

    #[test]
    fn generated_key_is_used_only_without_configured_keys() {
        let s = McpState::default();
        assert!(s.keys(&[]).is_empty());
        s.set_generated_key([9; 32]);
        assert_eq!(s.keys(&[])[0].id, 0);
        let configured = [SessionKey {
            id: 4,
            secret: [1; 32],
        }];
        assert_eq!(s.keys(&configured)[0].id, 4);
    }
}
