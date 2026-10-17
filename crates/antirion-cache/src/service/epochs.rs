//! The invalidation epochs of the levels in the key, and beside them the cutoff times of the finer
//! invalidations (`crate::invalidate`). Both only ever grow, so the store keeps the largest value
//! any node wrote and every node raises its own to it.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use serde_json::Value;

use crate::entry::Entry;
use crate::invalidate;

/// The invalidation epoch of each level that was ever bumped (absent ones are 0), and the cutoffs.
#[derive(Debug, Default)]
pub struct Epochs {
    map: Mutex<BTreeMap<String, u64>>,
    /// Whether any cutoff exists, so lookups without one never take the lock.
    cutoffs: AtomicBool,
}

fn epoch_key(level: &str, name: &str) -> String {
    format!("{level}/{name}")
}

fn is_cutoff(key: &str) -> bool {
    invalidate::Target::of_cutoff_key(key).is_some()
}

impl Epochs {
    #[must_use]
    pub fn get(&self, level: &str, name: &str) -> u64 {
        self.map.lock().get(&epoch_key(level, name)).copied().unwrap_or(0)
    }

    /// Increments the epoch of a level; its new value.
    pub fn bump(&self, level: &str, name: &str) -> u64 {
        let mut map = self.map.lock();
        let e = map.entry(epoch_key(level, name)).or_insert(0);
        *e += 1;
        *e
    }

    /// Raises the value stored under `key` (as `export` names it) to at least `value`; whether it
    /// grew.
    pub fn raise(&self, key: &str, value: u64) -> bool {
        let mut map = self.map.lock();
        let e = map.entry(key.to_owned()).or_insert(0);
        let grew = value > *e;
        *e = (*e).max(value);
        if grew && is_cutoff(key) {
            self.cutoffs.store(true, Ordering::Relaxed);
        }
        grew
    }

    /// The cutoff time stored under `key`, milliseconds.
    #[must_use]
    pub fn cutoff(&self, key: &str) -> Option<i64> {
        if !self.cutoffs.load(Ordering::Relaxed) {
            return None;
        }
        self.map.lock().get(key).and_then(|v| i64::try_from(*v).ok())
    }

    /// Whether a cutoff makes `entry` dead.
    #[must_use]
    pub fn dead(&self, entry: &Entry) -> bool {
        if !self.cutoffs.load(Ordering::Relaxed) {
            return false;
        }
        let map = self.map.lock();
        invalidate::dead(entry, |k| map.get(k).and_then(|v| i64::try_from(*v).ok()))
    }

    /// Every bumped epoch and cutoff, for the store.
    #[must_use]
    pub fn export(&self) -> Value {
        Value::Object(
            self.map
                .lock()
                .iter()
                .map(|(k, v)| (k.clone(), Value::from(*v)))
                .collect(),
        )
    }

    /// Restores what `export` produced.
    pub fn restore(&self, v: &Value) {
        for (k, e) in v.as_object().into_iter().flatten() {
            if let Some(e) = e.as_u64() {
                self.raise(k, e);
            }
        }
    }

    pub fn clear(&self) {
        self.map.lock().clear();
        self.cutoffs.store(false, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epochs_bump_and_round_trip() {
        let e = Epochs::default();
        assert_eq!(e.get("project", "p"), 0);
        assert_eq!(e.bump("project", "p"), 1);
        assert_eq!(e.bump("project", "p"), 2);
        let other = Epochs::default();
        other.restore(&e.export());
        assert_eq!(other.get("project", "p"), 2);
    }

    #[test]
    fn cutoffs_only_grow() {
        let e = Epochs::default();
        assert_eq!(e.cutoff("cut:model/m"), None);
        assert!(e.raise("cut:model/m", 10));
        assert!(!e.raise("cut:model/m", 5));
        assert_eq!(e.cutoff("cut:model/m"), Some(10));
        e.clear();
        assert_eq!(e.cutoff("cut:model/m"), None);
    }
}
