//! The embeddings of embedding classifiers on this replica: one bounded cache per classifier node,
//! keyed by a hash of the tenant scope and the text (examples use an empty scope).

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::lru::Lru;

/// The vectors of one classifier node, by text key.
type Vectors = Lru<u128, Arc<[f32]>>;

/// Embedded texts by classifier node.
#[derive(Debug, Default)]
pub struct EmbeddingCache {
    nodes: Mutex<HashMap<String, Vectors>>,
}

/// The cache key of `text` embedded through `model` for `scope`.
#[must_use]
pub fn key(model: &str, scope: &str, text: &str) -> u128 {
    let mut buf = Vec::with_capacity(model.len() + scope.len() + text.len() + 16);
    for part in [model, scope, text] {
        buf.extend_from_slice(&(part.len() as u64).to_be_bytes());
        buf.extend_from_slice(part.as_bytes());
    }
    xxhash_rust::xxh3::xxh3_128(&buf)
}

impl EmbeddingCache {
    /// The cached vector of `key` under node `node`.
    #[must_use]
    pub fn get(&self, node: &str, key: u128) -> Option<Arc<[f32]>> {
        self.nodes.lock().get_mut(node)?.get(&key).cloned()
    }

    /// Keeps a vector under node `node`, whose cache holds at most `capacity` entries.
    pub fn put(&self, node: &str, key: u128, v: Arc<[f32]>, capacity: usize) {
        let mut all = self.nodes.lock();
        let lru = match all.get_mut(node) {
            Some(l) => l,
            None => all.entry(node.to_owned()).or_insert_with(|| Lru::new(capacity)),
        };
        lru.put(key, v);
    }

    pub fn clear(&self) {
        self.nodes.lock().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_are_kept_per_node_and_scope() {
        let c = EmbeddingCache::default();
        let k = key("m", "org/p", "hello");
        assert_ne!(k, key("m", "org/q", "hello"));
        c.put("n", k, Arc::from(vec![1.0f32]), 2);
        assert_eq!(c.get("n", k).as_deref(), Some(&[1.0f32][..]));
        assert!(c.get("other", k).is_none());
    }
}
