//! Session affinity bindings of this node: a session's tree position, kept with a sliding TTL on
//! the gateway clock, bounded in number, and keyed by tenant scope, route and session value.

use std::collections::HashMap;
use std::time::Duration;

use antirion_core::UtcMicros;
use parking_lot::Mutex;

/// Where a session is bound.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub route: String,
    /// The route path of the bound position.
    pub route_path: String,
    pub deployment: String,
    pub expires_at: UtcMicros,
    touched: u64,
}

/// The binding key: scope ids, route, source kind and value.
#[must_use]
pub fn binding_key(scope: &[&str], route: &str, source: &str, value: &str) -> String {
    let mut k = scope.join("\u{1f}");
    for part in [route, source, value] {
        k.push('\u{1e}');
        k.push_str(part);
    }
    k
}

#[derive(Debug, Default)]
struct Inner {
    map: HashMap<String, Binding>,
    clock: u64,
}

/// In-memory bindings.
#[derive(Debug, Default)]
pub struct AffinityStore {
    inner: Mutex<Inner>,
}

impl AffinityStore {
    /// The live binding of a key; an expired one is dropped.
    pub fn get(&self, key: &str, now: UtcMicros) -> Option<Binding> {
        let mut inner = self.inner.lock();
        let live = inner.map.get(key).is_some_and(|b| b.expires_at > now);
        if !live {
            inner.map.remove(key);
            return None;
        }
        inner.map.get(key).cloned()
    }

    /// Writes or refreshes a binding to `position` (route, route path, deployment); the least
    /// recently written entries go beyond `max_entries`.
    pub fn put(
        &self,
        key: String,
        position: (&str, &str, &str),
        ttl: Duration,
        now: UtcMicros,
        max_entries: usize,
    ) {
        let (route, route_path, deployment) = position;
        let mut inner = self.inner.lock();
        inner.clock += 1;
        let touched = inner.clock;
        inner.map.insert(
            key,
            Binding {
                route: route.to_owned(),
                route_path: route_path.to_owned(),
                deployment: deployment.to_owned(),
                expires_at: now.saturating_add(ttl),
                touched,
            },
        );
        let max = max_entries.max(1);
        if inner.map.len() > max {
            inner.map.retain(|_, b| b.expires_at > now);
        }
        while inner.map.len() > max {
            let Some(oldest) = inner
                .map
                .iter()
                .min_by_key(|(_, b)| b.touched)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            inner.map.remove(&oldest);
        }
    }

    /// Drops the binding of `key`.
    pub fn forget(&self, key: &str) {
        self.inner.lock().map.remove(key);
    }

    /// Breaks every binding to a deployment.
    pub fn forget_deployment(&self, deployment: &str) {
        self.inner.lock().map.retain(|_, b| b.deployment != deployment);
    }

    /// Drops every binding of a route; returns how many were live.
    pub fn forget_route(&self, route: &str, now: UtcMicros) -> usize {
        let mut inner = self.inner.lock();
        let mut live = 0;
        inner.map.retain(|_, b| {
            let keep = b.route != route;
            if !keep && b.expires_at > now {
                live += 1;
            }
            keep
        });
        live
    }

    /// Live bindings of a route.
    pub fn count(&self, route: &str, now: UtcMicros) -> usize {
        self.inner
            .lock()
            .map
            .values()
            .filter(|b| b.route == route && b.expires_at > now)
            .count()
    }

    pub fn clear(&self) {
        self.inner.lock().map.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttl_slides_and_entries_are_bounded() {
        let s = AffinityStore::default();
        let t = |secs: i64| UtcMicros(secs * 1_000_000);
        let k = binding_key(&["o", "p"], "r", "header", "s1");
        s.put(k.clone(), ("r", "r/0/a", "a"), Duration::from_secs(2), t(0), 10);
        assert!(s.get(&k, t(1)).is_some());
        s.put(k.clone(), ("r", "r/0/a", "a"), Duration::from_secs(2), t(1), 10);
        assert!(s.get(&k, t(2)).is_some());
        assert!(s.get(&k, t(4)).is_none());
        for i in 0..5 {
            s.put(
                format!("k{i}"),
                ("r", "r/0/a", "a"),
                Duration::from_secs(60),
                t(0),
                3,
            );
        }
        assert_eq!(s.count("r", t(0)), 3);
        s.forget_deployment("a");
        assert_eq!(s.count("r", t(0)), 0);
    }

    #[test]
    fn forgetting_a_route_keeps_the_others() {
        let s = AffinityStore::default();
        let t = |secs: i64| UtcMicros(secs * 1_000_000);
        let ttl = Duration::from_secs(10);
        s.put("k1".into(), ("r", "r/0/a", "a"), ttl, t(0), 10);
        s.put("k2".into(), ("r", "r/0/b", "b"), ttl, t(0), 10);
        s.put("k3".into(), ("q", "q/0/a", "a"), ttl, t(0), 10);
        s.put("k4".into(), ("r", "r/0/a", "a"), Duration::from_secs(1), t(0), 10);
        assert_eq!(s.forget_route("r", t(5)), 2);
        assert_eq!(s.count("r", t(5)), 0);
        assert_eq!(s.count("q", t(5)), 1);
    }
}
