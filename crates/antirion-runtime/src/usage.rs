//! When each key last authenticated, by key incarnation, for the key's `last_used_at`; and when each
//! credential last passed verification while the store was reachable, for the identity-store
//! outage grace.

use std::collections::{HashMap, HashSet};

use antirion_core::{ResourceUid, UtcMicros};
use parking_lot::Mutex;

/// How far a key's use may run ahead of what the store holds before it is written again.
pub const WRITE_EVERY_US: i64 = 60_000_000;
const SHARDS: usize = 16;

/// One key's last use, and the last use the store holds from this node.
#[derive(Copy, Clone, Debug)]
struct Mark {
    last: UtcMicros,
    written: Option<UtcMicros>,
}

/// The last successful authentication of each key on this node, sharded so that requests of
/// different keys rarely meet on one lock.
#[derive(Debug, Default)]
pub struct KeyUsage {
    shards: [Mutex<HashMap<ResourceUid, Mark>>; SHARDS],
}

impl KeyUsage {
    fn shard(&self, key: ResourceUid) -> &Mutex<HashMap<ResourceUid, Mark>> {
        let [first, ..] = &self.shards;
        let i = usize::try_from(key.0 % SHARDS as u128).unwrap_or(0);
        self.shards.get(i).unwrap_or(first)
    }

    pub fn record(&self, key: ResourceUid, now: UtcMicros) {
        let mut shard = self.shard(key).lock();
        let mark = shard.entry(key).or_insert(Mark {
            last: now,
            written: None,
        });
        mark.last = mark.last.max(now);
    }

    #[must_use]
    pub fn last_used(&self, key: ResourceUid) -> Option<UtcMicros> {
        self.shard(key).lock().get(&key).map(|m| m.last)
    }

    /// The uses the store should hear about: never written, or a minute or more past what was.
    #[must_use]
    pub fn unwritten(&self) -> Vec<(ResourceUid, UtcMicros)> {
        let mut out = Vec::new();
        for shard in &self.shards {
            out.extend(
                shard
                    .lock()
                    .iter()
                    .filter(|(_, m)| m.written.is_none_or(|w| m.last.0 - w.0 >= WRITE_EVERY_US))
                    .map(|(k, m)| (*k, m.last)),
            );
        }
        out
    }

    /// Notes that the store now holds these uses.
    pub fn written(&self, marks: &[(ResourceUid, UtcMicros)]) {
        for (key, at) in marks {
            if let Some(m) = self.shard(*key).lock().get_mut(key) {
                m.written = Some(m.written.map_or(*at, |w| w.max(*at)));
            }
        }
    }

    /// Forgets keys that no longer exist.
    pub fn retain(&self, live: &HashSet<ResourceUid>) {
        for shard in &self.shards {
            shard.lock().retain(|k, _| live.contains(k));
        }
    }

    pub fn clear(&self) {
        for shard in &self.shards {
            shard.lock().clear();
        }
    }
}

/// When each credential, by kid, last passed verification while the store was reachable. A node
/// that started from its local snapshot counts every credential of it as verified when the snapshot
/// was persisted.
#[derive(Debug, Default)]
pub struct Verifications {
    last: Mutex<HashMap<String, UtcMicros>>,
    floor: Mutex<Option<UtcMicros>>,
}

impl Verifications {
    pub fn record(&self, kid: &str, now: UtcMicros) {
        let mut last = self.last.lock();
        match last.get_mut(kid) {
            Some(t) => *t = now,
            None => {
                last.insert(kid.to_owned(), now);
            }
        }
    }

    /// Every credential counts as verified at `at` at least.
    pub fn assume_all(&self, at: UtcMicros) {
        let mut floor = self.floor.lock();
        *floor = Some(floor.map_or(at, |f| f.max(at)));
    }

    /// Whether `kid` passed verification less than `grace` before `now`.
    #[must_use]
    pub fn within(&self, kid: &str, now: UtcMicros, grace: std::time::Duration) -> bool {
        let last = self.last.lock().get(kid).copied();
        let floor = *self.floor.lock();
        let Some(at) = last.max(floor) else {
            return false;
        };
        let grace_us = i64::try_from(grace.as_micros()).unwrap_or(i64::MAX);
        at.0.saturating_add(grace_us) > now.0
    }

    /// Forgets credentials that no longer exist.
    pub fn retain(&self, live: impl Fn(&str) -> bool) {
        self.last.lock().retain(|k, _| live(k));
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn verification_lasts_for_the_grace_only() {
        let v = Verifications::default();
        let grace = Duration::from_secs(10);
        assert!(!v.within("k", UtcMicros(0), grace));
        v.record("k", UtcMicros(1_000_000));
        assert!(v.within("k", UtcMicros(10_999_999), grace));
        assert!(!v.within("k", UtcMicros(11_000_000), grace));
        assert!(!v.within("k", UtcMicros(1_000_000), Duration::ZERO));
        assert!(!v.within("other", UtcMicros(2_000_000), grace));
        v.retain(|_| false);
        assert!(!v.within("k", UtcMicros(2_000_000), grace));
    }

    #[test]
    fn a_snapshot_time_counts_for_every_credential() {
        let v = Verifications::default();
        v.assume_all(UtcMicros(5_000_000));
        let grace = Duration::from_secs(1);
        assert!(v.within("any", UtcMicros(5_500_000), grace));
        assert!(!v.within("any", UtcMicros(6_000_000), grace));
        v.record("k", UtcMicros(7_000_000));
        assert!(v.within("k", UtcMicros(7_500_000), grace));
    }

    #[test]
    fn records_and_forgets() {
        let u = KeyUsage::default();
        let k = ResourceUid::random();
        assert!(u.last_used(k).is_none());
        u.record(k, UtcMicros(5));
        u.record(k, UtcMicros(3));
        assert_eq!(u.last_used(k), Some(UtcMicros(5)));
        u.retain(&HashSet::new());
        assert!(u.last_used(k).is_none());
    }

    #[test]
    fn a_use_is_written_again_only_a_minute_later() {
        let u = KeyUsage::default();
        let k = ResourceUid::random();
        u.record(k, UtcMicros(1_000_000));
        let due = u.unwritten();
        assert_eq!(due, vec![(k, UtcMicros(1_000_000))]);
        u.written(&due);
        u.record(k, UtcMicros(30_000_000));
        assert!(u.unwritten().is_empty());
        u.record(k, UtcMicros(61_000_000));
        assert_eq!(u.unwritten(), vec![(k, UtcMicros(61_000_000))]);
    }
}
