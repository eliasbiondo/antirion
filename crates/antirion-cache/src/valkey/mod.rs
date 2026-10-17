//! The shared backend on Valkey (or Redis), or a Redis Cluster: one string per entry under
//! `<prefix><scope digest>:<key>`, written with an expiry equal to the entry's remaining TTL, and
//! the entry index under `<prefix>{<prefix>}ix:` (`index`). On a cluster the entries spread over
//! the slots while the index stays in one; a delete of an entry the index does not know scans
//! every primary.

mod index;
mod scripts;

use std::sync::{Arc, LazyLock};

use antirion_redis::{RedisConn, Topology};
use redis::{AsyncCommands, Value};

use crate::CacheError;
use crate::entry::Entry;

static SCRIPTS: LazyLock<scripts::Scripts> = LazyLock::new(scripts::Scripts::new);

/// Entries on a Valkey or Redis server.
#[derive(Clone)]
pub struct ValkeyStore {
    conn: RedisConn,
    prefix: String,
    /// Where the index keys start: the prefix, then itself as a hash tag.
    base: Arc<str>,
}

impl std::fmt::Debug for ValkeyStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValkeyStore")
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

fn err(e: &redis::RedisError) -> CacheError {
    if e.is_timeout() {
        CacheError::Timeout
    } else if e.is_io_error() || e.is_connection_dropped() || e.is_unrecoverable_error() {
        CacheError::Unavailable
    } else {
        CacheError::Protocol
    }
}

/// `s` with the glob metacharacters of `SCAN MATCH` escaped.
fn glob_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

impl ValkeyStore {
    /// A store on `url`. No connection is made until the first call, so a node starts while the
    /// server is down.
    pub fn new(url: &str, prefix: &str, cluster: bool) -> Result<ValkeyStore, CacheError> {
        let topology = if cluster {
            Topology::Cluster
        } else {
            Topology::Standalone
        };
        let conn = RedisConn::open(url, topology, 0).map_err(|e| err(&e))?;
        Ok(ValkeyStore {
            conn,
            prefix: prefix.to_owned(),
            base: Arc::from(format!("{prefix}{{{prefix}}}ix:")),
        })
    }

    fn full(&self, scope: &str, key: &str) -> String {
        format!("{}{scope}:{key}", self.prefix)
    }

    pub async fn get(&self, scope: &str, key: &str) -> Result<Option<Entry>, CacheError> {
        let mut conn = self.conn.clone();
        let text: Option<String> = conn.get(self.full(scope, key)).await.map_err(|e| err(&e))?;
        Ok(text.and_then(|t| serde_json::from_str(&t).ok()))
    }

    /// Writes `entry` with a backend expiry of `px_ms` milliseconds and indexes it; the entries a
    /// cap evicted are deleted.
    pub async fn put(
        &self,
        scope: &str,
        key: &str,
        entry: &Entry,
        px_ms: u64,
        now_ms: i64,
    ) -> Result<(), CacheError> {
        let text = serde_json::to_string(entry).map_err(|_| CacheError::Protocol)?;
        let mut conn = self.conn.clone();
        redis::cmd("SET")
            .arg(self.full(scope, key))
            .arg(text)
            .arg("PX")
            .arg(px_ms.max(1))
            .query_async::<Value>(&mut conn)
            .await
            .map_err(|e| err(&e))?;
        let evicted = self.index_put(scope, key, entry, now_ms).await?;
        self.delete_pairs(&evicted).await
    }

    /// Deletes entries named `digest:key`, one command each so a cluster routes them by slot.
    async fn delete_pairs(&self, pairs: &[String]) -> Result<(), CacheError> {
        let mut conn = self.conn.clone();
        for pair in pairs {
            conn.del::<_, u64>(format!("{}{pair}", self.prefix))
                .await
                .map_err(|e| err(&e))?;
        }
        Ok(())
    }

    /// The server keys matching `pattern`.
    async fn scan(&self, pattern: &str) -> Result<Vec<String>, CacheError> {
        self.conn.scan(pattern).await.map_err(|e| err(&e))
    }

    /// Deletes the entry with `key` whatever its scope; whether one existed.
    pub async fn delete(&self, key: &str) -> Result<bool, CacheError> {
        if let Some(digest) = self.index_remove(key).await? {
            let mut conn = self.conn.clone();
            let n: u64 = conn.del(self.full(&digest, key)).await.map_err(|e| err(&e))?;
            return Ok(n > 0);
        }
        let pattern = format!("{}*:{key}", glob_escape(&self.prefix));
        let keys = self.scan(&pattern).await?;
        if keys.is_empty() {
            return Ok(false);
        }
        let mut conn = self.conn.clone();
        let n: u64 = conn.del(keys).await.map_err(|e| err(&e))?;
        Ok(n > 0)
    }

    /// Deletes every entry under the prefix, and the index.
    pub async fn clear(&self) -> Result<(), CacheError> {
        let keys = self.scan(&format!("{}*", glob_escape(&self.prefix))).await?;
        if keys.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.clone();
        conn.del::<_, u64>(keys).await.map(|_| ()).map_err(|e| err(&e))
    }

    pub async fn ping(&self) -> Result<(), CacheError> {
        let mut conn = self.conn.clone();
        redis::cmd("PING")
            .query_async::<Value>(&mut conn)
            .await
            .map(|_| ())
            .map_err(|e| err(&e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(ttl_ms: u64) -> Entry {
        Entry {
            written_at_ms: 0,
            ttl_ms,
            content_type: "application/json".into(),
            body: Some("{\"a\":1}".into()),
            events: None,
            provider: "p".into(),
            deployment: "d".into(),
            model: "m".into(),
            upstream_status: 200,
            upstream_dialect: None,
            usage: None,
            meta: None,
        }
    }

    #[test]
    fn match_patterns_escape_globs() {
        assert_eq!(glob_escape("a*b?[c]\\"), "a\\*b\\?\\[c\\]\\\\");
    }

    /// Runs against a server when `ANTIRION_TEST_VALKEY_URL` is set.
    #[tokio::test]
    async fn round_trip_on_a_server() {
        let Ok(url) = std::env::var("ANTIRION_TEST_VALKEY_URL") else {
            return;
        };
        let s = ValkeyStore::new(&url, "antirion:cachetest:", false).unwrap();
        s.clear().await.unwrap();
        let key = "a".repeat(64);
        s.put("0123456789abcdef", &key, &entry(60_000), 60_000, 0)
            .await
            .unwrap();
        assert_eq!(
            s.get("0123456789abcdef", &key).await.unwrap(),
            Some(entry(60_000))
        );
        assert!(s.get("fedcba9876543210", &key).await.unwrap().is_none());
        let mut conn = s.conn.clone();
        let pttl: i64 = redis::cmd("PTTL")
            .arg(s.full("0123456789abcdef", &key))
            .query_async(&mut conn)
            .await
            .unwrap();
        assert!(pttl > 0 && pttl <= 60_000);
        assert!(s.delete(&key).await.unwrap());
        assert!(!s.delete(&key).await.unwrap());
        s.ping().await.unwrap();
    }

    /// Runs against a Redis Cluster when `ANTIRION_TEST_VALKEY_CLUSTER_URL` names one of its nodes:
    /// entries of many scopes land on every primary, and a delete and a clear find them all.
    #[tokio::test]
    async fn entries_spread_over_a_cluster() {
        let Ok(url) = std::env::var("ANTIRION_TEST_VALKEY_CLUSTER_URL") else {
            return;
        };
        let s = ValkeyStore::new(&url, "antirion:cachetest:", true).unwrap();
        s.clear().await.unwrap();
        let key = "b".repeat(64);
        let scopes: Vec<String> = (0..32).map(|i| format!("{i:016x}")).collect();
        for scope in &scopes {
            s.put(scope, &key, &entry(60_000), 60_000, 0).await.unwrap();
        }
        for scope in &scopes {
            assert_eq!(s.get(scope, &key).await.unwrap(), Some(entry(60_000)));
        }
        assert!(s.delete(&key).await.unwrap());
        for scope in &scopes {
            assert!(s.get(scope, &key).await.unwrap().is_none());
        }
        s.put("0000000000000000", &key, &entry(60_000), 60_000, 0)
            .await
            .unwrap();
        s.clear().await.unwrap();
        assert!(s.get("0000000000000000", &key).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn an_unreachable_server_is_an_error() {
        let s = ValkeyStore::new("redis://127.0.0.1:1/", "p:", false).unwrap();
        assert!(s.get("s", "k").await.is_err());
    }
}
