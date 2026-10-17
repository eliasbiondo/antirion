//! The node-local backend: entries bounded by their total weight, least recently used evicted first,
//! each entry also counted under the per-scope caps it was written with (a cap evicts that scope's
//! least recently used entry). The index beside the entries answers listings, totals and drops
//! without visiting entries a selection excludes.

mod index;
mod slab;

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::entry::Entry;
use crate::listing::{self, ListQuery, Listed, Listing, Position, SortKey, Totals};
use crate::meta::EntryMeta;
use index::{Index, Values};
use slab::{GLOBAL, Id, Slab, Slot};

/// Expired entries removed per write, so the index never grows with dead entries.
const PRUNE_PER_WRITE: usize = 16;

#[derive(Debug, Default)]
struct Inner {
    max_bytes: u64,
    slab: Slab,
    ids: HashMap<Arc<str>, Id>,
    /// Cap scope → its list.
    caps: HashMap<String, u32>,
    index: Index,
    bytes: u64,
    hits: u64,
}

fn values(s: &Slot) -> Values {
    let e = &s.entry;
    [
        listing::last_use_value(e.written_at_ms, s.last_hit_ms),
        e.written_at_ms,
        e.expires_at_ms(),
        i64::try_from(s.hits).unwrap_or(i64::MAX),
        i64::try_from(s.weight).unwrap_or(i64::MAX),
    ]
}

fn dims(e: &Entry) -> Vec<String> {
    e.meta.as_ref().map(|m| m.dims(&e.deployment)).unwrap_or_default()
}

fn listed(s: &Slot) -> Listed {
    let e = &s.entry;
    Listed {
        key: s.key.to_string(),
        meta: e.meta.clone().unwrap_or_default(),
        deployment: e.deployment.clone(),
        provider: e.provider.clone(),
        size_bytes: s.weight,
        hits: s.hits,
        created_ms: e.written_at_ms,
        expires_ms: e.expires_at_ms(),
        last_hit_ms: s.last_hit_ms,
    }
}

impl Inner {
    fn take(&mut self, key: &str) -> Option<Slot> {
        let id = self.ids.remove(key)?;
        let slot = self.slab.remove(id)?;
        self.index.remove(&slot.key, &values(&slot), &dims(&slot.entry));
        self.bytes = self.bytes.saturating_sub(slot.weight);
        self.hits = self.hits.saturating_sub(slot.hits);
        Some(slot)
    }

    fn take_id(&mut self, id: Id) -> Option<Slot> {
        let key = Arc::clone(&self.slab.get(id)?.key);
        self.take(&key)
    }

    fn evict_to(&mut self, max: u64, room: u64) {
        while self.bytes.saturating_add(room) > max {
            let Some(id) = self.slab.front(GLOBAL) else {
                break;
            };
            self.take_id(id);
        }
    }

    fn prune(&mut self, now_ms: i64, limit: usize) {
        for key in self.index.up_to(SortKey::Expires, now_ms, limit) {
            self.take(&key);
        }
    }

    fn cap_list(&mut self, scope: &str) -> u32 {
        if let Some(l) = self.caps.get(scope) {
            return *l;
        }
        let l = self.slab.new_list();
        self.caps.insert(scope.to_owned(), l);
        l
    }

    fn get_live(&self, key: &str, now_ms: i64) -> Option<&Slot> {
        let slot = self.slab.get(*self.ids.get(key)?)?;
        slot.entry.fresh(now_ms).then_some(slot)
    }

    /// The candidates of a filtered listing, unsorted.
    fn candidates(&self, q: &ListQuery) -> Option<Vec<Arc<str>>> {
        let mut sets: Vec<Vec<Arc<str>>> = q.all_of.iter().map(|d| self.index.with(d)).collect();
        if let Some(any) = &q.any_of {
            let mut union: Vec<Arc<str>> = any.iter().flat_map(|d| self.index.with(d)).collect();
            union.sort_unstable();
            union.dedup();
            sets.push(union);
        }
        if let Some(p) = &q.prefix {
            sets.push(self.index.prefixed(p));
        }
        let smallest = sets
            .iter()
            .enumerate()
            .min_by_key(|(_, s)| s.len())
            .map(|(i, _)| i)?;
        let base = sets.swap_remove(smallest);
        Some(base)
    }

    fn matches(&self, key: &str, q: &ListQuery) -> bool {
        q.all_of.iter().all(|d| self.index.has(d, key))
            && q.any_of
                .as_ref()
                .is_none_or(|any| any.iter().any(|d| self.index.has(d, key)))
            && q.prefix.as_ref().is_none_or(|p| key.starts_with(p.as_str()))
    }
}

/// Entries in node memory, never more than `max_bytes` of them.
#[derive(Debug)]
pub struct MemoryStore {
    inner: Mutex<Inner>,
}

impl MemoryStore {
    #[must_use]
    pub fn new(max_bytes: u64) -> MemoryStore {
        MemoryStore {
            inner: Mutex::new(Inner {
                max_bytes,
                ..Inner::default()
            }),
        }
    }

    /// Changes the bound, evicting what no longer fits.
    pub fn resize(&self, max_bytes: u64) {
        let mut inner = self.inner.lock();
        inner.max_bytes = max_bytes;
        inner.evict_to(max_bytes, 0);
    }

    /// The fresh entry under `key`, which becomes the most recently used. An expired entry is
    /// dropped.
    #[must_use]
    pub fn get(&self, key: &str, now_ms: i64) -> Option<Arc<Entry>> {
        let mut inner = self.inner.lock();
        let id = *inner.ids.get(key)?;
        let fresh = inner.slab.get(id)?.entry.fresh(now_ms);
        if !fresh {
            inner.take(key);
            return None;
        }
        inner.slab.touch(id);
        inner.slab.get(id).map(|s| Arc::clone(&s.entry))
    }

    /// Counts a hit served from the entry under `key`.
    pub fn hit(&self, key: &str, now_ms: i64) {
        let mut inner = self.inner.lock();
        let Some(id) = inner.ids.get(key).copied() else {
            return;
        };
        let Some(slot) = inner.slab.get_mut(id) else {
            return;
        };
        let before = values(slot);
        slot.hits += 1;
        slot.last_hit_ms = Some(now_ms);
        let after = values(slot);
        let k = Arc::clone(&slot.key);
        inner.hits += 1;
        for sort in [SortKey::Hits, SortKey::LastHit] {
            let (old, new) = (index::value(&before, sort), index::value(&after, sort));
            inner.index.reorder(&k, sort, old, new);
        }
        inner.slab.touch(id);
    }

    /// Stores `entry`, replacing any under `key`; an entry larger than the bound is not kept. A cap
    /// the entry counts under evicts that scope's least recently used entries beyond it.
    pub fn put(&self, key: &str, entry: Entry, now_ms: i64) {
        let weight = entry.weight();
        let caps = entry.meta.as_ref().map(|m| m.caps.clone()).unwrap_or_default();
        let mut inner = self.inner.lock();
        let max = inner.max_bytes;
        inner.take(key);
        inner.prune(now_ms, PRUNE_PER_WRITE);
        if weight > max {
            return;
        }
        inner.evict_to(max, weight);
        let k: Arc<str> = Arc::from(key);
        let entry = Arc::new(entry);
        let ds = dims(&entry);
        let id = inner.slab.insert(Slot::new(Arc::clone(&k), entry, weight));
        inner.ids.insert(Arc::clone(&k), id);
        inner.bytes = inner.bytes.saturating_add(weight);
        if let Some(slot) = inner.slab.get(id) {
            let v = values(slot);
            inner.index.add(&k, &v, &ds);
        }
        for cap in caps {
            let list = inner.cap_list(&cap.scope);
            inner.slab.push_back(list, id);
            while inner.slab.len(list) as u64 > cap.max {
                let Some(oldest) = inner.slab.front(list).filter(|o| *o != id) else {
                    break;
                };
                inner.take_id(oldest);
            }
        }
    }

    /// Removes the entry under `key`; whether one was there (expired ones count as absent).
    pub fn delete(&self, key: &str, now_ms: i64) -> bool {
        self.inner.lock().take(key).is_some_and(|s| s.entry.fresh(now_ms))
    }

    /// The live entry under `key` as a listing reports it, with the entry.
    #[must_use]
    pub fn read(&self, key: &str, now_ms: i64) -> Option<(Listed, Arc<Entry>)> {
        let inner = self.inner.lock();
        let slot = inner.get_live(key, now_ms)?;
        Some((listed(slot), Arc::clone(&slot.entry)))
    }

    /// Live entry count and total weight.
    #[must_use]
    pub fn size(&self, now_ms: i64) -> (u64, u64) {
        let mut inner = self.inner.lock();
        inner.prune(now_ms, usize::MAX);
        (u64::try_from(inner.ids.len()).unwrap_or(u64::MAX), inner.bytes)
    }

    /// How many live entries carry `dim`.
    #[must_use]
    pub fn count(&self, dim: &str, now_ms: i64) -> u64 {
        let mut inner = self.inner.lock();
        inner.prune(now_ms, usize::MAX);
        inner.index.count(dim)
    }

    /// A page of live entries.
    #[must_use]
    pub fn list(&self, q: &ListQuery) -> Listing {
        let mut inner = self.inner.lock();
        inner.prune(q.now_ms, usize::MAX);
        let inner = &*inner;
        let item = |k: &str| inner.ids.get(k).and_then(|id| inner.slab.get(*id)).map(listed);
        let filtered = inner.candidates(q);
        let totals = q.totals.then(|| match &filtered {
            None => Totals {
                count: u64::try_from(inner.index.len()).unwrap_or(u64::MAX),
                bytes: inner.bytes,
                hits: inner.hits,
            },
            Some(keys) => keys
                .iter()
                .filter(|k| inner.matches(k, q))
                .filter_map(|k| item(k))
                .fold(Totals::default(), |t, i| Totals {
                    count: t.count + 1,
                    bytes: t.bytes + i.size_bytes,
                    hits: t.hits + i.hits,
                }),
        });
        let (items, next) = match filtered {
            Some(keys) => {
                let selected: Vec<Listed> = keys
                    .iter()
                    .filter(|k| inner.matches(k, q))
                    .filter_map(|k| item(k))
                    .collect();
                listing::page(selected, q)
            }
            None => {
                let mut items = Vec::with_capacity(q.limit.min(1024));
                let after = q.after.as_ref().map(|p| (p.value, p.key.as_str()));
                inner.index.walk(q.sort, q.descending, after, |k| {
                    if let Some(i) = item(k) {
                        items.push(i);
                    }
                    items.len() <= q.limit
                });
                let more = items.len() > q.limit;
                items.truncate(q.limit);
                let next = more.then(|| items.last()).flatten().map(|i| Position {
                    value: i.sort_value(q.sort),
                    key: i.key.clone(),
                });
                (items, next)
            }
        };
        Listing { items, next, totals }
    }

    /// Removes the entries carrying `dim` (only those of scope digest `digest`, when given) written
    /// at or before `written_le`; how many.
    pub fn drop_dim(&self, dim: &str, digest: Option<&str>, written_le: i64) -> u64 {
        let mut inner = self.inner.lock();
        let keys: Vec<Arc<str>> = inner
            .index
            .with(dim)
            .into_iter()
            .filter(|k| {
                inner
                    .ids
                    .get(k)
                    .and_then(|id| inner.slab.get(*id))
                    .is_some_and(|s| {
                        s.entry.written_at_ms <= written_le
                            && digest.is_none_or(|d| s.entry.meta.as_ref().is_some_and(|m| m.digest == d))
                    })
            })
            .collect();
        let mut n = 0;
        for k in keys {
            n += u64::from(inner.take(&k).is_some());
        }
        n
    }

    /// Removes the entries written at or before `written_le`; how many.
    pub fn drop_older(&self, written_le: i64) -> u64 {
        let mut inner = self.inner.lock();
        let keys = inner.index.up_to(SortKey::Created, written_le, usize::MAX);
        let mut n = 0;
        for k in keys {
            n += u64::from(inner.take(&k).is_some());
        }
        n
    }

    /// Removes every entry; how many there were.
    pub fn clear(&self) -> u64 {
        let mut inner = self.inner.lock();
        let n = u64::try_from(inner.ids.len()).unwrap_or(u64::MAX);
        let max = inner.max_bytes;
        *inner = Inner {
            max_bytes: max,
            ..Inner::default()
        };
        inner.index.clear();
        n
    }

    /// The meta of the live entry under `key`.
    #[must_use]
    pub fn meta(&self, key: &str, now_ms: i64) -> Option<EntryMeta> {
        let inner = self.inner.lock();
        inner.get_live(key, now_ms)?.entry.meta.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::{Cap, EntryMeta, Mode};

    fn entry(body: usize, ttl_ms: u64) -> Entry {
        Entry {
            written_at_ms: 0,
            ttl_ms,
            content_type: String::new(),
            body: Some("x".repeat(body)),
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

    fn written(at: i64, model: &str, caps: &[(&str, u64)]) -> Entry {
        let mut e = entry(10, 1_000_000);
        e.written_at_ms = at;
        e.meta = Some(EntryMeta {
            mode: Mode::Exact,
            digest: "d".into(),
            model: model.into(),
            project: "p".into(),
            caps: caps
                .iter()
                .map(|(s, m)| Cap {
                    scope: (*s).into(),
                    max: *m,
                })
                .collect(),
            ..EntryMeta::default()
        });
        e
    }

    #[test]
    fn evicts_least_recently_used_within_the_bound() {
        let weight = entry(1000, 10).weight();
        let m = MemoryStore::new(weight * 3);
        for k in ["a", "b", "c"] {
            m.put(k, entry(1000, 10), 0);
        }
        assert!(m.get("a", 1).is_some());
        m.put("d", entry(1000, 10), 0);
        assert!(m.get("b", 1).is_none(), "b was the least recently used");
        assert!(m.get("a", 1).is_some() && m.get("c", 1).is_some() && m.get("d", 1).is_some());
        assert_eq!(m.size(1), (3, weight * 3));
        m.put("huge", entry(10_000, 10), 0);
        assert!(m.get("huge", 1).is_none());
    }

    #[test]
    fn expired_entries_are_not_served() {
        let m = MemoryStore::new(1 << 20);
        m.put("a", entry(1, 10), 0);
        assert!(m.get("a", 10).is_none());
        assert_eq!(m.size(10).0, 0);
        m.put("b", entry(1, 10), 0);
        assert!(m.delete("b", 5));
        assert!(!m.delete("b", 5));
    }

    #[test]
    fn a_cap_evicts_its_scope_least_recently_used() {
        let m = MemoryStore::new(1 << 20);
        m.put("a", written(1, "m", &[("model/m", 2)]), 1);
        m.put("b", written(2, "m", &[("model/m", 2)]), 2);
        m.put("x", written(3, "other", &[]), 3);
        m.hit("a", 4);
        m.put("c", written(5, "m", &[("model/m", 2)]), 5);
        assert!(
            m.get("b", 6).is_none(),
            "b was the least recently used of the scope"
        );
        assert!(m.get("a", 6).is_some() && m.get("c", 6).is_some() && m.get("x", 6).is_some());
    }

    #[test]
    fn listings_filter_sort_page_and_total() {
        let m = MemoryStore::new(1 << 20);
        m.put("aa", written(1, "m", &[]), 1);
        m.put("ab", written(2, "m", &[]), 2);
        m.put("ba", written(3, "n", &[]), 3);
        m.hit("aa", 4);
        let q = ListQuery {
            descending: true,
            limit: 2,
            totals: true,
            now_ms: 5,
            ..ListQuery::default()
        };
        let page = m.list(&q);
        let keys: Vec<&str> = page.items.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(keys, ["aa", "ba"]);
        assert_eq!(page.totals.map(|t| (t.count, t.hits)), Some((3, 1)));
        let rest = m.list(&ListQuery {
            after: page.next,
            ..q.clone()
        });
        assert_eq!(rest.items.len(), 1);
        assert!(rest.next.is_none());
        let only_m = m.list(&ListQuery {
            all_of: vec!["model/m".into()],
            prefix: Some("a".into()),
            sort: SortKey::Created,
            ..q.clone()
        });
        let keys: Vec<&str> = only_m.items.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(keys, ["ab", "aa"]);
        assert_eq!(only_m.totals.map(|t| t.count), Some(2));
        let hidden = m.list(&ListQuery {
            any_of: Some(vec!["project/other".into()]),
            ..q
        });
        assert!(hidden.items.is_empty());
    }

    #[test]
    fn drops_select_by_dimension_and_age() {
        let m = MemoryStore::new(1 << 20);
        m.put("a", written(1, "m", &[]), 1);
        m.put("b", written(5, "m", &[]), 5);
        m.put("c", written(9, "n", &[]), 9);
        assert_eq!(m.drop_dim("model/m", None, 4), 1);
        assert_eq!(m.drop_dim("model/m", Some("other"), 10), 0);
        assert_eq!(m.drop_older(6), 1);
        assert_eq!(m.size(10).0, 1);
        assert_eq!(m.count("model/n", 10), 1);
        assert_eq!(m.clear(), 1);
    }
}
