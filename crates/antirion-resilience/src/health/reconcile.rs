//! Health state following a configuration change: what did not change keeps its state, a key with a
//! new secret or reference is alive again, a redefined deployment starts closed, and what left the
//! configuration is forgotten. Every node applies its own configuration, so nothing is shared;
//! the parts it resets take the change's time, so an older record of another node is not adopted
//! over them.

use antirion_core::UtcMicros;

use super::{HealthRegistry, Inner, reset_deployment};

/// A configuration change, as the health state sees it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigChange {
    /// Deployments that left the configuration.
    pub removed: Vec<String>,
    /// Deployments whose provider, base URL or upstream model changed, with their provider.
    pub redefined: Vec<(String, String)>,
    /// Keys whose secret or reference changed, or that left their provider: (provider, key, the
    /// provider's deployments).
    pub rekeyed: Vec<(String, String, Vec<String>)>,
}

impl ConfigChange {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.removed.is_empty() && self.redefined.is_empty() && self.rekeyed.is_empty()
    }
}

impl Inner {
    fn stamp_all(&mut self, d: &str, now: UtcMicros) {
        let dep = self.deployments.entry(d.to_owned()).or_default();
        dep.stamps.breaker = now.0;
        dep.stamps.cooldown = now.0;
        for p in dep.pairs.values_mut() {
            p.stamp = now.0;
        }
    }
}

impl HealthRegistry {
    /// Applies a configuration change to the health state.
    pub fn follow(&self, change: &ConfigChange, now: UtcMicros) {
        if change.is_empty() {
            return;
        }
        let mut inner = self.inner.lock();
        for d in &change.removed {
            inner.deployments.remove(d);
        }
        for (d, provider) in &change.redefined {
            reset_deployment(&mut inner, d, provider, now);
            inner.stamp_all(d, now);
        }
        for (provider, key, deployments) in &change.rekeyed {
            let id = (provider.clone(), key.clone());
            inner.keys.remove(&id);
            inner.key_stamps.insert(id, now.0);
            for d in deployments {
                if let Some(dep) = inner.deployments.get_mut(d)
                    && let Some(p) = dep.pairs.get_mut(key)
                {
                    *p = super::Pair {
                        stamp: now.0,
                        ..super::Pair::default()
                    };
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::class::Class;
    use crate::health::{KeyInfo, Verdict};
    use crate::settings::HealthConfig;

    #[test]
    fn redefined_deployments_close_and_new_secrets_revive() {
        let s = HealthConfig::from_doc(
            &json!({"resilience": {"health": {"breaker": {"consecutive_failures": 1}}}}),
        )
        .for_deployment("d")
        .clone();
        let now = UtcMicros(1_000_000_000_000);
        let r = HealthRegistry::default();
        let keys = [KeyInfo {
            name: "k".into(),
            weight: 1,
        }];
        let verdict = |class, status| Verdict {
            class,
            status: Some(status),
            hint: None,
            counts: true,
        };
        let t = r.begin("d", "p", Some("k"), &s, now).unwrap();
        r.finish(&t, &verdict(Class::ServerError, 500), &s, now);
        let t = r.begin("e", "p", Some("k"), &s, now).unwrap();
        r.finish(&t, &verdict(Class::CredentialRejected, 401), &s, now);
        r.follow(&ConfigChange::default(), now);
        assert_eq!(r.view("d", "p", &keys, &s, now).state, "open");
        assert!(r.view("e", "p", &keys, &s, now).keys[0].scope_dead);

        r.follow(
            &ConfigChange {
                redefined: vec![("d".into(), "p".into())],
                rekeyed: vec![("p".into(), "k".into(), vec!["d".into(), "e".into()])],
                ..ConfigChange::default()
            },
            now,
        );
        assert_eq!(r.view("d", "p", &keys, &s, now).state, "closed");
        assert!(r.status("e", "p", &keys, &s, now).is_ok());
    }
}
