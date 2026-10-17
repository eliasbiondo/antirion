//! A node's response cache: the configured backend, lookups and writes under their timeouts, the
//! invalidation epochs and the hit and miss counts. What the console reads and drops is `console`.

mod console;
mod epochs;

use antirion_redis::Topology;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use antirion_config::scalar::{parse_duration, parse_size};
use parking_lot::{Mutex, RwLock};
use serde_json::Value;

use crate::CacheError;
use crate::entry::Entry;
use crate::flights::Flights;
use crate::memory::MemoryStore;
use crate::valkey::ValkeyStore;
use crate::vectors::{DEFAULT_MAX_ENTRIES, VectorStore};

pub use epochs::Epochs;

const DEFAULT_MAX_SIZE: u64 = 256 << 20;
const DEFAULT_PREFIX: &str = "antirion:cache:";
const DEFAULT_SEMANTIC_TIMEOUT: Duration = Duration::from_millis(100);

/// Which backend holds the entries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendKind {
    Memory {
        max_size: u64,
    },
    Redis {
        url: String,
        prefix: String,
        /// A Redis Cluster: only the counter store's connection can be one.
        cluster: bool,
    },
}

/// The top-level `cache` storage settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub backend: BackendKind,
    pub lookup_timeout: Duration,
    pub write_timeout: Duration,
    /// The document leaves the backend to the node's mode: on a cluster node with a counter store,
    /// Redis at this URL (the counter store's when `None`) under this prefix.
    pub cluster_default: Option<(Option<String>, String)>,
    /// `semantic_store.memory.max_entries`.
    pub semantic_max_entries: usize,
    /// `semantic_store.lookup_timeout`: the embedding call and the vector search together.
    pub semantic_timeout: Duration,
    /// `cache.invalidation_rules`.
    pub rules: Vec<crate::rules::Rule>,
    /// The request metadata keys entries record, for the rules.
    pub metadata_keys: BTreeSet<String>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            backend: BackendKind::Memory {
                max_size: DEFAULT_MAX_SIZE,
            },
            lookup_timeout: Duration::from_millis(50),
            write_timeout: Duration::from_secs(1),
            cluster_default: Some((None, DEFAULT_PREFIX.to_owned())),
            semantic_max_entries: DEFAULT_MAX_ENTRIES,
            semantic_timeout: DEFAULT_SEMANTIC_TIMEOUT,
            rules: Vec::new(),
            metadata_keys: BTreeSet::new(),
        }
    }
}

impl Settings {
    /// The settings of a secret-resolved document. Without a `type`, a cluster node with a counter
    /// store uses that connection; every other node keeps entries in memory.
    #[must_use]
    pub fn from_doc(doc: &Value) -> Settings {
        let backend = doc.pointer("/cache/backend");
        let get = |p: &str| backend.and_then(|b| b.pointer(p));
        let duration = |p: &str, default: Duration| {
            get(p)
                .and_then(Value::as_str)
                .and_then(parse_duration)
                .unwrap_or(default)
        };
        let counters_url = (doc.pointer("/cluster/mode").and_then(Value::as_str) == Some("cluster"))
            .then(|| doc.pointer("/cluster/counters/redis/url").and_then(Value::as_str))
            .flatten();
        let counters_cluster = doc
            .pointer("/cluster/counters/redis/topology")
            .and_then(Value::as_str)
            == Some("cluster");
        let explicit_url = get("/redis/url").and_then(Value::as_str);
        let url = explicit_url.or(counters_url);
        let kind = get("/type").and_then(Value::as_str);
        let redis = match kind {
            Some("redis") => true,
            Some(_) => false,
            None => counters_url.is_some(),
        };
        let prefix = get("/redis/key_prefix")
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_PREFIX)
            .to_owned();
        let cluster_default = match (kind, url) {
            (None, _) | (Some("redis"), None) => Some((explicit_url.map(str::to_owned), prefix.clone())),
            _ => None,
        };
        let backend = match url.filter(|_| redis) {
            Some(url) => BackendKind::Redis {
                url: url.to_owned(),
                prefix,
                cluster: explicit_url.is_none() && counters_cluster,
            },
            None => BackendKind::Memory {
                max_size: get("/memory/max_size")
                    .and_then(Value::as_str)
                    .and_then(parse_size)
                    .unwrap_or(DEFAULT_MAX_SIZE),
            },
        };
        let store = |p: &str| doc.pointer("/cache/semantic_store").and_then(|s| s.pointer(p));
        let rules = crate::rules::compile(doc);
        Settings {
            metadata_keys: crate::rules::metadata_keys(&rules),
            rules,
            backend,
            lookup_timeout: duration("/lookup_timeout", Duration::from_millis(50)),
            write_timeout: duration("/write_timeout", Duration::from_secs(1)),
            cluster_default,
            semantic_max_entries: store("/memory/max_entries")
                .and_then(Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(DEFAULT_MAX_ENTRIES),
            semantic_timeout: store("/lookup_timeout")
                .and_then(Value::as_str)
                .and_then(parse_duration)
                .unwrap_or(DEFAULT_SEMANTIC_TIMEOUT),
        }
    }

    /// The settings on a node whose cluster counter store is at `counters` (cluster mode only).
    #[must_use]
    fn on_node(&self, counters: Option<&(String, Topology)>) -> Settings {
        let mut s = self.clone();
        if let (Some((url, prefix)), Some((counters, topology))) = (&self.cluster_default, counters) {
            s.backend = BackendKind::Redis {
                url: url.clone().unwrap_or_else(|| counters.clone()),
                prefix: prefix.clone(),
                cluster: url.is_none() && *topology == Topology::Cluster,
            };
        }
        s
    }
}

#[derive(Debug)]
pub(crate) enum Backend {
    Memory(Box<MemoryStore>),
    Valkey(ValkeyStore),
    /// A `redis` backend whose URL cannot be used: every call fails open.
    Broken,
}

/// What `GET /runtime/cache` reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Health {
    pub backend: &'static str,
    pub healthy: bool,
    pub entries: Option<u64>,
    pub bytes: Option<u64>,
    pub hits: u64,
    pub misses: u64,
}

/// The response cache of a node.
#[derive(Debug)]
pub struct Cache {
    /// The settings in effect on this node.
    settings: Mutex<Settings>,
    /// The settings as the document gives them.
    configured: Mutex<Settings>,
    /// The cluster's counter store connection, in cluster mode.
    counters: Mutex<Option<(String, Topology)>>,
    backend: RwLock<Arc<Backend>>,
    pub epochs: Epochs,
    /// The semantic layer's vectors.
    pub vectors: VectorStore,
    /// The leaders of identical concurrent misses.
    pub flights: Flights,
    /// The metadata values `metadata_changed` rules last saw.
    pub seen: crate::rules::Seen,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl Default for Cache {
    fn default() -> Cache {
        Cache {
            settings: Mutex::new(Settings::default()),
            configured: Mutex::new(Settings::default()),
            counters: Mutex::new(None),
            backend: RwLock::new(Arc::new(Backend::Memory(Box::new(MemoryStore::new(
                DEFAULT_MAX_SIZE,
            ))))),
            epochs: Epochs::default(),
            vectors: VectorStore::default(),
            flights: Flights::default(),
            seen: crate::rules::Seen::default(),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }
}

impl Cache {
    /// Applies new storage settings: a new memory bound evicts in place; a change of backend starts
    /// empty.
    pub fn configure(&self, configured: &Settings) {
        configured.clone_into(&mut *self.configured.lock());
        self.vectors.resize(configured.semantic_max_entries);
        let next = &configured.on_node(self.counters.lock().as_ref());
        let mut current = self.settings.lock();
        if *current == *next {
            return;
        }
        let backend = match (&next.backend, &**self.backend.read()) {
            _ if current.backend == next.backend => None,
            (BackendKind::Memory { max_size }, Backend::Memory(m)) => {
                m.resize(*max_size);
                None
            }
            (BackendKind::Memory { max_size }, _) => {
                Some(Backend::Memory(Box::new(MemoryStore::new(*max_size))))
            }
            (BackendKind::Redis { url, prefix, cluster }, _) => {
                Some(match ValkeyStore::new(url, prefix, *cluster) {
                    Ok(v) => Backend::Valkey(v),
                    Err(_) => {
                        tracing::warn!("the cache backend URL cannot be used; lookups fail open");
                        Backend::Broken
                    }
                })
            }
        };
        if let Some(b) = backend {
            *self.backend.write() = Arc::new(b);
        }
        *current = next.clone();
    }

    /// On a cluster node: the counter store connection, the backend when the document names none.
    pub fn use_cluster_counters(&self, url: &str, topology: Topology) {
        *self.counters.lock() = Some((url.to_owned(), topology));
        let configured = self.configured.lock().clone();
        self.configure(&configured);
    }

    /// Whether entries live in this node's memory.
    #[must_use]
    pub fn local(&self) -> bool {
        matches!(&**self.backend.read(), Backend::Memory(_))
    }

    fn current(&self) -> (Arc<Backend>, Settings) {
        (Arc::clone(&self.backend.read()), self.settings.lock().clone())
    }

    /// The fresh entry under `key`, within `lookup_timeout`. An entry a finer invalidation made dead
    /// is not served.
    pub async fn lookup(
        &self,
        scope: &str,
        key: &str,
        now_ms: i64,
    ) -> Result<Option<Arc<Entry>>, CacheError> {
        let (backend, settings) = self.current();
        let found = match &*backend {
            Backend::Memory(m) => m.get(key, now_ms),
            Backend::Valkey(v) => {
                let got = tokio::time::timeout(settings.lookup_timeout, v.get(scope, key))
                    .await
                    .map_err(|_| CacheError::Timeout)??;
                got.filter(|e| e.fresh(now_ms)).map(Arc::new)
            }
            Backend::Broken => return Err(CacheError::Unavailable),
        };
        match found {
            Some(e) if self.epochs.dead(&e) => {
                if let Backend::Memory(m) = &*backend {
                    m.delete(key, now_ms);
                }
                Ok(None)
            }
            other => Ok(other),
        }
    }

    /// Stores `entry` under `key`, within `write_timeout`.
    pub async fn write(&self, scope: &str, key: &str, entry: Entry, now_ms: i64) -> Result<(), CacheError> {
        let (backend, settings) = self.current();
        match &*backend {
            Backend::Memory(m) => {
                m.put(key, entry, now_ms);
                Ok(())
            }
            Backend::Valkey(v) => {
                let remaining = u64::try_from(entry.expires_at_ms().saturating_sub(now_ms)).unwrap_or(0);
                if remaining == 0 {
                    return Ok(());
                }
                tokio::time::timeout(
                    settings.write_timeout,
                    v.put(scope, key, &entry, remaining, now_ms),
                )
                .await
                .map_err(|_| CacheError::Timeout)?
            }
            Backend::Broken => Err(CacheError::Unavailable),
        }
    }

    /// How long the semantic layer may take.
    #[must_use]
    pub fn semantic_timeout(&self) -> Duration {
        self.settings.lock().semantic_timeout
    }

    /// Deletes the entry with `key` and the vectors that answer with it; whether one existed.
    pub async fn delete(&self, key: &str, now_ms: i64) -> Result<bool, CacheError> {
        self.vectors.delete_key(key);
        let (backend, settings) = self.current();
        match &*backend {
            Backend::Memory(m) => Ok(m.delete(key, now_ms)),
            Backend::Valkey(v) => tokio::time::timeout(settings.write_timeout, v.delete(key))
                .await
                .map_err(|_| CacheError::Timeout)?,
            Backend::Broken => Err(CacheError::Unavailable),
        }
    }

    /// Counts a lookup's result.
    pub fn count(&self, hit: bool) {
        let c = if hit { &self.hits } else { &self.misses };
        c.fetch_add(1, Ordering::Relaxed);
    }

    /// The backend's health and the node's counts; a shared backend counts its entries from the
    /// index.
    pub async fn health(&self, now_ms: i64) -> Health {
        let (backend, settings) = self.current();
        let hits = self.hits.load(Ordering::Relaxed);
        let misses = self.misses.load(Ordering::Relaxed);
        match &*backend {
            Backend::Memory(m) => {
                let (entries, bytes) = m.size(now_ms);
                Health {
                    backend: "memory",
                    healthy: true,
                    entries: Some(entries),
                    bytes: Some(bytes),
                    hits,
                    misses,
                }
            }
            Backend::Valkey(v) => {
                let bound = settings.lookup_timeout.max(Duration::from_millis(500));
                let healthy = matches!(tokio::time::timeout(bound, v.ping()).await, Ok(Ok(())));
                let size = if healthy {
                    tokio::time::timeout(bound, v.size(now_ms))
                        .await
                        .ok()
                        .and_then(Result::ok)
                } else {
                    None
                };
                Health {
                    backend: "redis",
                    healthy,
                    entries: size.map(|(n, _)| n),
                    bytes: size.map(|(_, b)| b),
                    hits,
                    misses,
                }
            }
            Backend::Broken => Health {
                backend: "redis",
                healthy: false,
                entries: None,
                bytes: None,
                hits,
                misses,
            },
        }
    }

    /// Drops every entry, the epochs and the counts.
    pub async fn clear(&self) {
        let (backend, settings) = self.current();
        match &*backend {
            Backend::Memory(m) => {
                m.clear();
            }
            Backend::Valkey(v) => {
                if !matches!(
                    tokio::time::timeout(settings.write_timeout, v.clear()).await,
                    Ok(Ok(()))
                ) {
                    tracing::warn!("the cache backend could not be cleared");
                }
            }
            Backend::Broken => {}
        }
        self.epochs.clear();
        self.vectors.clear();
        self.seen.clear();
        self.hits.store(0, Ordering::Relaxed);
        self.misses.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn entry() -> Entry {
        Entry {
            written_at_ms: 0,
            ttl_ms: 1_000,
            content_type: String::new(),
            body: Some("{}".into()),
            events: None,
            provider: String::new(),
            deployment: String::new(),
            model: String::new(),
            upstream_status: 200,
            upstream_dialect: None,
            usage: None,
            meta: None,
        }
    }

    #[test]
    fn settings_default_to_memory() {
        assert_eq!(Settings::from_doc(&json!({})), Settings::default());
        let s = Settings::from_doc(&json!({"cache": {"backend": {"memory": {"max_size": "64KiB"},
            "lookup_timeout": "10ms"}}}));
        assert_eq!(s.backend, BackendKind::Memory { max_size: 64 << 10 });
        assert_eq!(s.lookup_timeout, Duration::from_millis(10));
        assert_eq!(
            s.on_node(Some(&("redis://c".into(), Topology::Cluster))).backend,
            BackendKind::Redis {
                url: "redis://c".into(),
                prefix: "antirion:cache:".into(),
                cluster: true,
            }
        );
        let m = Settings::from_doc(&json!({"cache": {"backend": {"type": "memory"}}}));
        assert_eq!(
            m.on_node(Some(&("redis://c".into(), Topology::Standalone)))
                .backend,
            m.backend
        );
        let r = Settings::from_doc(
            &json!({"cluster": {"mode": "cluster", "counters": {"redis": {"url": "redis://c"}}}}),
        );
        assert_eq!(
            r.backend,
            BackendKind::Redis {
                url: "redis://c".into(),
                prefix: "antirion:cache:".into(),
                cluster: false,
            }
        );
    }

    #[tokio::test]
    async fn memory_round_trip_and_counts() {
        let c = Cache::default();
        let key = "k".repeat(64);
        c.write("s", &key, entry(), 0).await.unwrap();
        assert!(c.lookup("s", &key, 10).await.unwrap().is_some());
        assert!(c.lookup("s", &key, 1_000).await.unwrap().is_none());
        c.count(true);
        let h = c.health(10).await;
        assert_eq!((h.backend, h.healthy, h.hits, h.misses), ("memory", true, 1, 0));
        c.configure(&Settings::from_doc(
            &json!({"cache": {"backend": {"type": "redis", "redis": {"url": "not a url"}}}}),
        ));
        assert!(c.lookup("s", &key, 0).await.is_err());
        assert!(!c.health(0).await.healthy);
    }
}
