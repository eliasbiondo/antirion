//! Health state shared by the nodes of a cluster. Each change a node makes to a breaker (a trip, a
//! close), a deployment cooldown, a manual disable, a pair or a key becomes a record that carries
//! the change's time; a node adopts another node's record when it is newer than what it knows of
//! that part. Failure counts, windows and probe slots stay with each node.

use std::collections::BTreeMap;

use antirion_core::UtcMicros;

use super::breaker::BreakerState;
use super::{Dead, DeadReason, HealthRegistry, Inner};

/// Separates the names in a record's field; names never contain it.
const SEP: char = '\u{1f}';

/// One record of the shared health state: the part it describes (`field`), and its value, which
/// starts with `stamp`, the time of the change in microseconds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedRecord {
    pub field: String,
    pub stamp: i64,
    pub value: String,
}

fn opt(at: Option<UtcMicros>) -> i64 {
    at.map_or(0, |u| u.0)
}

fn at(n: i64) -> Option<UtcMicros> {
    (n > 0).then_some(UtcMicros(n))
}

fn dead_text(d: Option<&Dead>) -> String {
    d.map_or_else(
        || "-".to_owned(),
        |d| format!("{}:{}:{}", d.reason.as_str(), d.recheck_at.0, d.k),
    )
}

fn parse_dead(s: &str) -> Option<Option<Dead>> {
    if s == "-" {
        return Some(None);
    }
    let mut parts = s.split(':');
    let reason = match parts.next()? {
        "credential" => DeadReason::Credential,
        "quota" => DeadReason::Quota,
        "permission" => DeadReason::Permission,
        _ => return None,
    };
    Some(Some(Dead {
        reason,
        recheck_at: UtcMicros(parts.next()?.parse().ok()?),
        k: parts.next()?.parse().ok()?,
        probing: false,
    }))
}

impl Inner {
    fn post(&mut self, field: String, stamp: i64, rest: &str) {
        if let Some(out) = self.outbox.as_mut() {
            let value = format!("{stamp}|{rest}");
            out.insert(field.clone(), SharedRecord { field, stamp, value });
        }
    }

    pub(super) fn share_breaker(&mut self, d: &str, now: UtcMicros) {
        let dep = self.deployments.entry(d.to_owned()).or_default();
        dep.stamps.breaker = now.0;
        let b = &dep.breaker;
        let rest = match b.state {
            BreakerState::Open => format!("open|{}|{}", opt(b.open_until), b.trips),
            _ => format!("closed|{}", b.trips),
        };
        self.post(format!("b{SEP}{d}"), now.0, &rest);
    }

    pub(super) fn share_cooldown(&mut self, d: &str, now: UtcMicros) {
        let dep = self.deployments.entry(d.to_owned()).or_default();
        dep.stamps.cooldown = now.0;
        let rest = opt(dep.cooldown_until).to_string();
        self.post(format!("c{SEP}{d}"), now.0, &rest);
    }

    pub(super) fn share_manual(&mut self, d: &str, now: UtcMicros) {
        let dep = self.deployments.entry(d.to_owned()).or_default();
        dep.stamps.manual = now.0;
        let rest = match dep.disabled {
            Some(until) => format!("on|{}", opt(until)),
            None => "off".to_owned(),
        };
        self.post(format!("m{SEP}{d}"), now.0, &rest);
    }

    pub(super) fn share_pair(&mut self, d: &str, key: &str, now: UtcMicros) {
        let pair = self
            .deployments
            .entry(d.to_owned())
            .or_default()
            .pairs
            .entry(key.to_owned())
            .or_default();
        pair.stamp = now.0;
        let rest = format!("{}|{}", opt(pair.cooldown_until), dead_text(pair.dead.as_ref()));
        self.post(format!("p{SEP}{d}{SEP}{key}"), now.0, &rest);
    }

    pub(super) fn share_key(&mut self, provider: &str, key: &str, now: UtcMicros) {
        let id = (provider.to_owned(), key.to_owned());
        self.key_stamps.insert(id.clone(), now.0);
        let rest = dead_text(self.keys.get(&id));
        self.post(format!("k{SEP}{provider}{SEP}{key}"), now.0, &rest);
    }

    /// Adopts one record of another node when it is newer than what this node knows.
    fn adopt(&mut self, field: &str, value: &str, now: UtcMicros) -> Option<()> {
        let mut names = field.split(SEP);
        let kind = names.next()?;
        let first = names.next()?;
        let second = names.next();
        let mut parts = value.split('|');
        let stamp: i64 = parts.next()?.parse().ok()?;
        let rest: Vec<&str> = parts.collect();
        match (kind, second, rest.as_slice()) {
            ("b", None, [state, more @ ..]) => {
                let dep = self.deployments.entry(first.to_owned()).or_default();
                if stamp <= dep.stamps.breaker {
                    return Some(());
                }
                let (open, trips) = match (*state, more) {
                    ("open", [until, trips]) => (Some(UtcMicros(until.parse().ok()?)), trips.parse().ok()?),
                    ("closed", [trips]) => (None, trips.parse().ok()?),
                    _ => return None,
                };
                dep.breaker.adopt(open, trips, now);
                dep.stamps.breaker = stamp;
            }
            ("c", None, [until]) => {
                let dep = self.deployments.entry(first.to_owned()).or_default();
                if stamp > dep.stamps.cooldown {
                    dep.cooldown_until = at(until.parse().ok()?);
                    dep.stamps.cooldown = stamp;
                }
            }
            ("m", None, [state, more @ ..]) => {
                let dep = self.deployments.entry(first.to_owned()).or_default();
                if stamp <= dep.stamps.manual {
                    return Some(());
                }
                dep.disabled = match (*state, more) {
                    ("on", [until]) => Some(at(until.parse().ok()?)),
                    ("off", []) => None,
                    _ => return None,
                };
                dep.stamps.manual = stamp;
            }
            ("p", Some(key), [cooldown, dead]) => {
                let pair = self
                    .deployments
                    .entry(first.to_owned())
                    .or_default()
                    .pairs
                    .entry(key.to_owned())
                    .or_default();
                if stamp > pair.stamp {
                    pair.cooldown_until = at(cooldown.parse().ok()?);
                    pair.dead = parse_dead(dead)?;
                    pair.stamp = stamp;
                }
            }
            ("k", Some(key), [dead]) => {
                let id = (first.to_owned(), key.to_owned());
                if stamp > self.key_stamps.get(&id).copied().unwrap_or(0) {
                    match parse_dead(dead)? {
                        Some(d) => self.keys.insert(id.clone(), d),
                        None => self.keys.remove(&id),
                    };
                    self.key_stamps.insert(id, stamp);
                }
            }
            _ => return None,
        }
        Some(())
    }
}

impl HealthRegistry {
    /// Starts keeping this node's changes for the other nodes of its cluster.
    pub fn share(&self) {
        let mut inner = self.inner.lock();
        if inner.outbox.is_none() {
            inner.outbox = Some(BTreeMap::new());
        }
    }

    /// The changes made since the last call, for the shared store.
    pub fn take_shared(&self) -> Vec<SharedRecord> {
        let mut inner = self.inner.lock();
        inner
            .outbox
            .as_mut()
            .map(|o| std::mem::take(o).into_values().collect())
            .unwrap_or_default()
    }

    /// Puts back changes the shared store did not take, unless a newer change replaced them.
    pub fn requeue_shared(&self, records: Vec<SharedRecord>) {
        let mut inner = self.inner.lock();
        let Some(out) = inner.outbox.as_mut() else {
            return;
        };
        for r in records {
            if out.get(&r.field).is_none_or(|e| e.stamp < r.stamp) {
                out.insert(r.field.clone(), r);
            }
        }
    }

    /// Adopts the shared store's records (field, value) that are newer than this node's state.
    /// Returns how many records could not be read.
    pub fn merge_shared(&self, records: &[(String, String)], now: UtcMicros) -> usize {
        let mut inner = self.inner.lock();
        records
            .iter()
            .filter(|(f, v)| inner.adopt(f, v, now).is_none())
            .count()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::class::Class;
    use crate::health::{KeyInfo, Verdict};
    use crate::settings::{HealthConfig, HealthSettings};

    fn settings() -> HealthSettings {
        HealthConfig::from_doc(&json!({"resilience": {"health": {"breaker": {"consecutive_failures": 1}}}}))
            .for_deployment("d")
            .clone()
    }

    fn at(secs: f64) -> UtcMicros {
        UtcMicros((secs * 1e6) as i64 + 1_000_000_000_000)
    }

    fn fail(
        r: &HealthRegistry,
        s: &HealthSettings,
        key: Option<&str>,
        class: Class,
        status: u16,
        now: UtcMicros,
    ) {
        let t = r.begin("d", "p", key, s, now).unwrap();
        r.finish(
            &t,
            &Verdict {
                class,
                status: Some(status),
                hint: None,
                counts: true,
            },
            s,
            now,
        );
    }

    fn sync(from: &HealthRegistry, to: &HealthRegistry, now: UtcMicros) -> Vec<SharedRecord> {
        let records = from.take_shared();
        let pairs: Vec<(String, String)> = records
            .iter()
            .map(|r| (r.field.clone(), r.value.clone()))
            .collect();
        assert_eq!(to.merge_shared(&pairs, now), 0);
        records
    }

    #[test]
    fn trips_closes_and_dead_keys_reach_another_node() {
        let s = settings();
        let (a, b) = (HealthRegistry::default(), HealthRegistry::default());
        a.share();
        b.share();
        let keys = [KeyInfo {
            name: "k".into(),
            weight: 1,
        }];
        fail(&a, &s, Some("k"), Class::ServerError, 500, at(0.0));
        let sent = sync(&a, &b, at(0.1));
        assert!(sent.iter().any(|r| r.field.starts_with('b')));
        assert_eq!(b.view("d", "p", &keys, &s, at(0.2)).state, "open");
        assert!(b.status("d", "p", &keys, &s, at(0.2)).unwrap_err().breaker_only);
        assert!(b.take_shared().is_empty(), "adopted records are not sent again");

        a.reset("d", "p", at(1.0));
        sync(&a, &b, at(1.1));
        assert_eq!(b.view("d", "p", &keys, &s, at(1.2)).state, "closed");

        fail(&a, &s, Some("k"), Class::CredentialRejected, 401, at(2.0));
        sync(&a, &b, at(2.1));
        let v = b.view("d", "p", &keys, &s, at(2.2));
        assert!(v.keys[0].scope_dead);
        assert_eq!(v.state, "closed", "a credential failure never trips the breaker");
    }

    #[test]
    fn older_records_never_replace_newer_state() {
        let s = settings();
        let (a, b) = (HealthRegistry::default(), HealthRegistry::default());
        a.share();
        b.share();
        a.disable("d", None, at(5.0));
        let stale = a.take_shared();
        b.enable("d", at(6.0));
        let pairs: Vec<(String, String)> = stale.iter().map(|r| (r.field.clone(), r.value.clone())).collect();
        b.merge_shared(&pairs, at(6.1));
        assert!(b.status("d", "p", &[], &s, at(6.2)).is_ok());
        assert_eq!(
            b.merge_shared(&[("b\u{1f}d".into(), "x|open".into())], at(7.0)),
            1
        );
    }

    #[test]
    fn unsent_records_are_requeued_unless_replaced() {
        let r = HealthRegistry::default();
        r.share();
        r.disable("d", None, at(1.0));
        let first = r.take_shared();
        r.enable("d", at(2.0));
        r.requeue_shared(first);
        let out = r.take_shared();
        assert_eq!(out.len(), 1);
        assert!(out[0].value.ends_with("|off"));
    }
}
