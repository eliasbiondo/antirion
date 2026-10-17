//! Attempts in flight per deployment, shared by the nodes of a cluster for `least_busy` nodes of
//! cluster scope: each node writes its own counts under its id with the time it wrote them, and
//! reads every node's.

use redis::Value;

use super::{ValkeyCounters, items, text};
use crate::store::StoreError;

impl ValkeyCounters {
    fn inflight_key(&self) -> String {
        format!("{}{{i}}inflight", self.prefix)
    }

    /// Writes this node's counts (a JSON object of deployment → attempts in flight, with the
    /// write time in `at_ms`).
    pub async fn inflight_put(&self, node: &str, counts: &str) -> Result<(), StoreError> {
        let mut set = redis::cmd("HSET");
        set.arg(self.inflight_key()).arg(node).arg(counts);
        self.run(&set).await.map(|_| ())
    }

    /// Every node's counts: (node, JSON).
    pub async fn inflight_all(&self) -> Result<Vec<(String, String)>, StoreError> {
        let mut all = redis::cmd("HGETALL");
        all.arg(self.inflight_key());
        let reply = self.run(&all).await?;
        let flat: Vec<String> = match &reply {
            Value::Map(pairs) => pairs
                .iter()
                .flat_map(|(k, v)| [text(k), text(v)])
                .flatten()
                .collect(),
            other => items(other).iter().filter_map(text).collect(),
        };
        Ok(flat
            .as_chunks::<2>()
            .0
            .iter()
            .map(|[n, v]| (n.clone(), v.clone()))
            .collect())
    }
}
