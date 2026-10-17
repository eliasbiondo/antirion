//! The reasoning replay store: signed and encrypted reasoning that translated responses attached to
//! tool calls, kept per project and tool call id for `api.reasoning_replay.ttl`, so a client that
//! drops it from its history gets it back. Entries live in node memory, bounded in number; the
//! oldest go first.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use antirion_core::UtcMicros;
use antirion_dialects::xlate::replay::Saved;
use parking_lot::Mutex;
use serde_json::Value;

/// The most entries a node keeps.
const MAX_ENTRIES: usize = 100_000;

/// `api.reasoning_replay` of a document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplaySettings {
    pub ttl: Duration,
    pub max_entry_bytes: usize,
}

impl ReplaySettings {
    /// The settings, or `None` when replay is off.
    #[must_use]
    pub fn of(doc: &Value) -> Option<ReplaySettings> {
        let block = doc.pointer("/api/reasoning_replay");
        let get = |k: &str| block.and_then(|b| b.get(k));
        if get("enabled") == Some(&Value::Bool(false)) {
            return None;
        }
        Some(ReplaySettings {
            ttl: get("ttl")
                .and_then(Value::as_str)
                .and_then(antirion_config::scalar::parse_duration)
                .unwrap_or(Duration::from_secs(86_400)),
            max_entry_bytes: get("max_entry_bytes")
                .and_then(Value::as_str)
                .and_then(antirion_config::scalar::parse_size)
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(256 * 1024),
        })
    }
}

type Key = (String, String);

#[derive(Debug, Default)]
struct Inner {
    map: HashMap<Key, (Arc<Saved>, UtcMicros)>,
    order: VecDeque<Key>,
}

/// Every replay entry of the node.
#[derive(Debug, Default)]
pub struct ReplayStore {
    inner: Mutex<Inner>,
}

impl ReplayStore {
    /// Keeps the entries of one response, each at most `max_entry_bytes`, until `now + ttl`.
    pub fn keep(&self, project: &str, entries: &[(String, Saved)], now: UtcMicros, s: ReplaySettings) {
        let expires = now.saturating_add(s.ttl);
        let mut inner = self.inner.lock();
        for (id, saved) in entries.iter().filter(|(_, v)| v.bytes() <= s.max_entry_bytes) {
            let key = (project.to_owned(), id.clone());
            if inner
                .map
                .insert(key.clone(), (Arc::new(saved.clone()), expires))
                .is_none()
            {
                inner.order.push_back(key);
            }
        }
        while inner.order.len() > MAX_ENTRIES {
            if let Some(old) = inner.order.pop_front() {
                inner.map.remove(&old);
            }
        }
    }

    /// The entry of a tool call id in `project`, while it lives.
    #[must_use]
    pub fn get(&self, project: &str, id: &str, now: UtcMicros) -> Option<Arc<Saved>> {
        let key = (project.to_owned(), id.to_owned());
        let mut inner = self.inner.lock();
        match inner.map.get(&key) {
            Some((v, expires)) if *expires > now => Some(Arc::clone(v)),
            Some(_) => {
                inner.map.remove(&key);
                None
            }
            None => None,
        }
    }

    pub fn clear(&self) {
        let mut inner = self.inner.lock();
        inner.map.clear();
        inner.order.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(n: usize) -> Saved {
        Saved {
            blocks: Vec::new(),
            call_signature: Some("s".repeat(n)),
        }
    }

    #[test]
    fn entries_are_scoped_sized_and_expire() {
        let store = ReplayStore::default();
        let now = UtcMicros(1_000_000);
        let s = ReplaySettings {
            ttl: Duration::from_secs(60),
            max_entry_bytes: 10,
        };
        store.keep("p", &[("a".into(), saved(5)), ("big".into(), saved(11))], now, s);
        assert!(store.get("p", "a", now).is_some());
        assert!(store.get("other", "a", now).is_none());
        assert!(store.get("p", "big", now).is_none());
        assert!(
            store
                .get("p", "a", now.saturating_add(Duration::from_secs(61)))
                .is_none()
        );
        let off = serde_json::json!({"api": {"reasoning_replay": {"enabled": false}}});
        assert_eq!(ReplaySettings::of(&off), None);
        let on = serde_json::json!({"api": {"reasoning_replay": {"ttl": "1m", "max_entry_bytes": "2KiB"}}});
        assert_eq!(
            ReplaySettings::of(&on),
            Some(ReplaySettings {
                ttl: Duration::from_secs(60),
                max_entry_bytes: 2048
            })
        );
    }
}
