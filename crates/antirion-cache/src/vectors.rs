//! The node's semantic store: unit vectors of embedded requests, grouped by partition key, each
//! pointing at the exact entry it answers with. Candidates are only ever compared within one
//! partition. The store holds at most `max_entries` vectors; the oldest written go first.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use parking_lot::Mutex;

/// The default bound on stored vectors.
pub const DEFAULT_MAX_ENTRIES: usize = 100_000;

/// One embedded request.
#[derive(Clone, Debug)]
pub struct Item {
    /// The scope digest and exact key of the entry it answers with.
    pub scope: String,
    pub key: String,
    /// Unit-normalised.
    pub vector: Arc<[f32]>,
    /// The polarity tokens of its embedding input.
    pub polarity: Vec<String>,
    pub written_at_ms: i64,
    pub expires_at_ms: i64,
}

/// The best candidate of a search.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub scope: String,
    pub key: String,
    pub similarity: f64,
}

/// What a search compares against.
#[derive(Clone, Copy, Debug)]
pub struct Probe<'a> {
    /// Unit-normalised.
    pub vector: &'a [f32],
    /// `None` without the polarity guard.
    pub polarity: Option<&'a [String]>,
    pub threshold: f64,
    pub now_ms: i64,
    /// Candidates written before this are too old (`max-age`).
    pub written_after_ms: Option<i64>,
}

#[derive(Debug, Default)]
struct Inner {
    max_entries: usize,
    partitions: HashMap<String, Vec<(u64, Item)>>,
    /// Write order: sequence number → partition.
    order: BTreeMap<u64, String>,
    next: u64,
}

impl Inner {
    fn remove_seq(&mut self, seq: u64) {
        let Some(partition) = self.order.remove(&seq) else {
            return;
        };
        if let Some(items) = self.partitions.get_mut(&partition) {
            items.retain(|(s, _)| *s != seq);
            if items.is_empty() {
                self.partitions.remove(&partition);
            }
        }
    }

    fn evict(&mut self) {
        while self.order.len() > self.max_entries {
            let Some((&oldest, _)) = self.order.iter().next() else {
                return;
            };
            self.remove_seq(oldest);
        }
    }
}

/// The semantic store of a node.
#[derive(Debug)]
pub struct VectorStore {
    inner: Mutex<Inner>,
}

impl Default for VectorStore {
    fn default() -> VectorStore {
        VectorStore {
            inner: Mutex::new(Inner {
                max_entries: DEFAULT_MAX_ENTRIES,
                ..Inner::default()
            }),
        }
    }
}

/// `v` scaled to unit length; `None` for an empty, zero or non-finite vector.
#[must_use]
pub fn normalised(v: &[f32]) -> Option<Arc<[f32]>> {
    let norm = v
        .iter()
        .map(|x| f64::from(*x) * f64::from(*x))
        .sum::<f64>()
        .sqrt();
    if v.is_empty() || !norm.is_finite() || norm == 0.0 {
        return None;
    }
    Some(v.iter().map(|x| (f64::from(*x) / norm) as f32).collect())
}

/// The cosine similarity of two unit vectors of the same size.
fn cosine(a: &[f32], b: &[f32]) -> Option<f64> {
    (a.len() == b.len()).then(|| a.iter().zip(b).map(|(x, y)| f64::from(*x) * f64::from(*y)).sum())
}

impl VectorStore {
    /// Applies a new bound, evicting the oldest vectors beyond it.
    pub fn resize(&self, max_entries: usize) {
        let mut inner = self.inner.lock();
        inner.max_entries = max_entries.max(1);
        inner.evict();
    }

    /// Stores `item` in `partition`, replacing the vector that answered with the same key there.
    pub fn insert(&self, partition: &str, item: Item) {
        let mut inner = self.inner.lock();
        let replaced: Vec<u64> = inner
            .partitions
            .get(partition)
            .into_iter()
            .flatten()
            .filter(|(_, i)| i.key == item.key)
            .map(|(s, _)| *s)
            .collect();
        for seq in replaced {
            inner.remove_seq(seq);
        }
        let seq = inner.next;
        inner.next += 1;
        inner.order.insert(seq, partition.to_owned());
        inner
            .partitions
            .entry(partition.to_owned())
            .or_default()
            .push((seq, item));
        inner.evict();
    }

    /// The most similar fresh candidate of `partition` at or above the threshold whose polarity
    /// matches; ties go to the most recently written. Expired vectors are dropped on the way.
    #[must_use]
    pub fn search(&self, partition: &str, probe: &Probe<'_>) -> Option<Candidate> {
        let mut inner = self.inner.lock();
        let items = inner.partitions.get(partition)?;
        let expired: Vec<u64> = items
            .iter()
            .filter(|(_, i)| i.expires_at_ms <= probe.now_ms)
            .map(|(s, _)| *s)
            .collect();
        let best = items
            .iter()
            .filter(|(_, i)| i.expires_at_ms > probe.now_ms)
            .filter(|(_, i)| probe.written_after_ms.is_none_or(|t| i.written_at_ms > t))
            .filter(|(_, i)| probe.polarity.is_none_or(|p| p == i.polarity.as_slice()))
            .filter_map(|(s, i)| Some((cosine(probe.vector, &i.vector)?, *s, i)))
            .filter(|(sim, _, _)| *sim >= probe.threshold)
            .max_by(|(a, sa, ia), (b, sb, ib)| {
                a.total_cmp(b)
                    .then(ia.written_at_ms.cmp(&ib.written_at_ms))
                    .then(sa.cmp(sb))
            })
            .map(|(similarity, _, i)| Candidate {
                scope: i.scope.clone(),
                key: i.key.clone(),
                similarity,
            });
        for seq in expired {
            inner.remove_seq(seq);
        }
        best
    }

    /// Drops every vector that answers with `key`.
    pub fn delete_key(&self, key: &str) {
        let mut inner = self.inner.lock();
        let seqs: Vec<u64> = inner
            .partitions
            .values()
            .flatten()
            .filter(|(_, i)| i.key == key)
            .map(|(s, _)| *s)
            .collect();
        for seq in seqs {
            inner.remove_seq(seq);
        }
    }

    /// The number of stored vectors.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().order.len()
    }

    /// Whether the store holds no vector.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        let mut inner = self.inner.lock();
        inner.partitions.clear();
        inner.order.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(key: &str, v: &[f32], polarity: &[&str], written: i64) -> Item {
        Item {
            scope: "s".into(),
            key: key.into(),
            vector: normalised(v).unwrap(),
            polarity: polarity.iter().map(|s| (*s).to_owned()).collect(),
            written_at_ms: written,
            expires_at_ms: written + 1_000,
        }
    }

    fn probe<'a>(v: &'a [f32], polarity: Option<&'a [String]>, threshold: f64) -> Probe<'a> {
        Probe {
            vector: v,
            polarity,
            threshold,
            now_ms: 10,
            written_after_ms: None,
        }
    }

    #[test]
    fn best_candidate_within_the_partition_and_polarity() {
        let s = VectorStore::default();
        s.insert("p", item("a", &[1.0, 0.0], &["on"], 0));
        s.insert("p", item("b", &[0.97, 0.243], &[], 1));
        s.insert("q", item("c", &[1.0, 0.0], &[], 2));
        let q = normalised(&[1.0, 0.0]).unwrap();
        let none: Vec<String> = Vec::new();
        let on = vec!["on".to_owned()];
        assert_eq!(s.search("p", &probe(&q, Some(&on), 0.9)).unwrap().key, "a");
        assert_eq!(s.search("p", &probe(&q, Some(&none), 0.9)).unwrap().key, "b");
        assert!(s.search("p", &probe(&q, Some(&none), 0.99)).is_none());
        assert_eq!(s.search("p", &probe(&q, None, 0.9)).unwrap().key, "a");
        assert!(s.search("r", &probe(&q, None, 0.0)).is_none());
    }

    #[test]
    fn ties_go_to_the_newest_and_old_entries_are_evicted() {
        let s = VectorStore::default();
        s.insert("p", item("old", &[1.0, 0.0], &[], 0));
        s.insert("p", item("new", &[1.0, 0.0], &[], 5));
        let q = normalised(&[1.0, 0.0]).unwrap();
        assert_eq!(s.search("p", &probe(&q, None, 0.5)).unwrap().key, "new");
        s.resize(1);
        assert_eq!(s.len(), 1);
        assert_eq!(s.search("p", &probe(&q, None, 0.5)).unwrap().key, "new");
        s.insert("p", item("new", &[0.0, 1.0], &[], 6));
        assert_eq!(s.len(), 1, "the same key replaces its vector");
        s.delete_key("new");
        assert!(s.is_empty());
    }

    #[test]
    fn expired_and_mismatched_vectors_never_match() {
        let s = VectorStore::default();
        s.insert("p", item("a", &[1.0, 0.0], &[], -5_000));
        s.insert("p", item("b", &[1.0, 0.0, 0.0], &[], 5));
        let q = normalised(&[1.0, 0.0]).unwrap();
        assert!(s.search("p", &probe(&q, None, 0.0)).is_none());
        assert_eq!(s.len(), 1);
        assert!(normalised(&[0.0, 0.0]).is_none());
    }
}
