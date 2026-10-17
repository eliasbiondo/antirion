//! The memory backend's entry index: each sort order as an ordered set of `(value, key)`, the keys
//! in order (for prefix search), and the keys carrying each dimension value. A listing walks one
//! ordered set from its position, or sorts the candidates its filters select; it never visits the
//! entries a filter excludes.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::Bound;
use std::sync::Arc;

use crate::listing::SortKey;

fn slot(sort: SortKey) -> usize {
    match sort {
        SortKey::LastHit => 0,
        SortKey::Created => 1,
        SortKey::Expires => 2,
        SortKey::Hits => 3,
        SortKey::Size => 4,
    }
}

/// An entry's value under each sort order: last use, created, expires, hits, size.
pub(super) type Values = [i64; 5];

/// The value of `sort` among `values`.
pub(super) fn value(values: &Values, sort: SortKey) -> i64 {
    values.get(slot(sort)).copied().unwrap_or(0)
}

#[derive(Debug, Default)]
pub(super) struct Index {
    sorted: [BTreeSet<(i64, Arc<str>)>; 5],
    keys: BTreeSet<Arc<str>>,
    dims: HashMap<String, HashSet<Arc<str>>>,
}

impl Index {
    pub(super) fn add(&mut self, key: &Arc<str>, values: &Values, dims: &[String]) {
        for (set, v) in self.sorted.iter_mut().zip(values) {
            set.insert((*v, Arc::clone(key)));
        }
        self.keys.insert(Arc::clone(key));
        for d in dims {
            self.dims.entry(d.clone()).or_default().insert(Arc::clone(key));
        }
    }

    pub(super) fn remove(&mut self, key: &Arc<str>, values: &Values, dims: &[String]) {
        for (set, v) in self.sorted.iter_mut().zip(values) {
            set.remove(&(*v, Arc::clone(key)));
        }
        self.keys.remove(key);
        for d in dims {
            if let Some(set) = self.dims.get_mut(d) {
                set.remove(key);
                if set.is_empty() {
                    self.dims.remove(d);
                }
            }
        }
    }

    /// Moves `key` within the order of `sort`.
    pub(super) fn reorder(&mut self, key: &Arc<str>, sort: SortKey, old: i64, new: i64) {
        if let Some(set) = self.sorted.get_mut(slot(sort)) {
            set.remove(&(old, Arc::clone(key)));
            set.insert((new, Arc::clone(key)));
        }
    }

    pub(super) fn clear(&mut self) {
        *self = Index::default();
    }

    /// The keys carrying `dim`.
    pub(super) fn with(&self, dim: &str) -> Vec<Arc<str>> {
        self.dims
            .get(dim)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// How many entries carry `dim`.
    pub(super) fn count(&self, dim: &str) -> u64 {
        self.dims
            .get(dim)
            .map_or(0, |s| u64::try_from(s.len()).unwrap_or(u64::MAX))
    }

    pub(super) fn has(&self, dim: &str, key: &str) -> bool {
        self.dims.get(dim).is_some_and(|s| s.contains(key))
    }

    /// Keys by `sort` value at or below `max` (ascending), at most `limit` of them.
    pub(super) fn up_to(&self, sort: SortKey, max: i64, limit: usize) -> Vec<Arc<str>> {
        self.sorted
            .get(slot(sort))
            .into_iter()
            .flatten()
            .take_while(|(v, _)| *v <= max)
            .take(limit)
            .map(|(_, k)| Arc::clone(k))
            .collect()
    }

    /// Keys starting with `prefix`.
    pub(super) fn prefixed(&self, prefix: &str) -> Vec<Arc<str>> {
        self.keys
            .range::<str, _>((Bound::Included(prefix), Bound::Unbounded))
            .take_while(|k| k.starts_with(prefix))
            .cloned()
            .collect()
    }

    /// Keys in `sort` order after `(value, key)` when given, for as long as `take` wants more.
    pub(super) fn walk(
        &self,
        sort: SortKey,
        descending: bool,
        after: Option<(i64, &str)>,
        mut take: impl FnMut(&Arc<str>) -> bool,
    ) {
        let Some(set) = self.sorted.get(slot(sort)) else {
            return;
        };
        let after = after.map(|(v, k)| (v, Arc::<str>::from(k)));
        let iter: Box<dyn Iterator<Item = &(i64, Arc<str>)>> = match (&after, descending) {
            (None, false) => Box::new(set.iter()),
            (None, true) => Box::new(set.iter().rev()),
            (Some(p), false) => Box::new(set.range((Bound::Excluded(p.clone()), Bound::Unbounded))),
            (Some(p), true) => Box::new(set.range((Bound::Unbounded, Bound::Excluded(p.clone()))).rev()),
        };
        for (_, k) in iter {
            if !take(k) {
                return;
            }
        }
    }

    /// The number of indexed entries.
    pub(super) fn len(&self) -> usize {
        self.keys.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_and_selects() {
        let mut ix = Index::default();
        let a: Arc<str> = Arc::from("aa");
        let b: Arc<str> = Arc::from("ab");
        ix.add(&a, &[1, 1, 1, 1, 1], &["model/m".into()]);
        ix.add(&b, &[2, 2, 2, 2, 2], &["model/m".into(), "tag/x".into()]);
        let mut seen = Vec::new();
        ix.walk(SortKey::Created, true, None, |k| {
            seen.push(k.to_string());
            true
        });
        assert_eq!(seen, ["ab", "aa"]);
        seen.clear();
        ix.walk(SortKey::Created, true, Some((2, "ab")), |k| {
            seen.push(k.to_string());
            true
        });
        assert_eq!(seen, ["aa"]);
        assert_eq!(ix.count("model/m"), 2);
        assert_eq!(ix.prefixed("ab").len(), 1);
        assert_eq!(ix.up_to(SortKey::Created, 1, 10).len(), 1);
        ix.reorder(&a, SortKey::Hits, 1, 5);
        seen.clear();
        ix.walk(SortKey::Hits, true, None, |k| {
            seen.push(k.to_string());
            false
        });
        assert_eq!(seen, ["aa"]);
        ix.remove(&b, &[2, 2, 2, 2, 2], &["model/m".into(), "tag/x".into()]);
        assert_eq!(ix.count("tag/x"), 0);
        assert_eq!(ix.len(), 1);
    }
}
