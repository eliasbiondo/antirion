//! A bounded map that forgets its least recently used entry first.

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;

/// Entries by key, with the tick of their last use; `order` maps ticks back to keys.
#[derive(Debug)]
pub struct Lru<K, V> {
    map: HashMap<K, (V, u64)>,
    order: BTreeMap<u64, K>,
    tick: u64,
    capacity: usize,
}

impl<K: Eq + Hash + Clone, V> Lru<K, V> {
    #[must_use]
    pub fn new(capacity: usize) -> Lru<K, V> {
        Lru {
            map: HashMap::new(),
            order: BTreeMap::new(),
            tick: 0,
            capacity: capacity.max(1),
        }
    }

    fn touch(&mut self, key: &K) -> Option<&mut (V, u64)> {
        self.tick += 1;
        let tick = self.tick;
        let entry = self.map.get_mut(key)?;
        self.order.remove(&entry.1);
        entry.1 = tick;
        self.order.insert(tick, key.clone());
        Some(entry)
    }

    /// The value of `key`, marked as just used.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        self.touch(key).map(|e| &e.0)
    }

    /// Whether `key` is present, without marking it used.
    #[must_use]
    pub fn contains(&self, key: &K) -> bool {
        self.map.contains_key(key)
    }

    /// Inserts or replaces `key`; the least recently used entries go beyond the capacity.
    pub fn put(&mut self, key: K, value: V) {
        if let Some(e) = self.touch(&key) {
            e.0 = value;
            return;
        }
        self.tick += 1;
        self.order.insert(self.tick, key.clone());
        self.map.insert(key, (value, self.tick));
        while self.map.len() > self.capacity {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            self.map.remove(&oldest);
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_least_recently_used_entry_goes_first() {
        let mut l = Lru::new(2);
        l.put("a", 1);
        l.put("b", 2);
        assert_eq!(l.get(&"a"), Some(&1));
        l.put("c", 3);
        assert!(l.contains(&"a") && l.contains(&"c") && !l.contains(&"b"));
        l.put("a", 4);
        assert_eq!(l.len(), 2);
        assert_eq!(l.get(&"a"), Some(&4));
    }
}
