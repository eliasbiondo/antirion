//! The cluster-wide probe leases of the registry's half-open deployments.

use std::sync::Arc;
use std::time::Duration;

use super::{HealthRegistry, Ticket};
use crate::lease::{LeaseAnswer, ProbeLeases};
use crate::settings::HealthSettings;

impl HealthRegistry {
    /// Shares half-open probe slots through `leases` (cluster mode); set once.
    pub fn set_leases(&self, leases: Arc<dyn ProbeLeases>) {
        let _ = self.leases.set(leases);
    }

    /// Takes the cluster-wide lease a probe needs when the deployment's probes are counted across
    /// the cluster; false when every lease is held elsewhere, and the candidate is skipped. A
    /// store that cannot answer leaves the node's own probe slot to decide.
    pub async fn lease(&self, t: &mut Ticket, s: &HealthSettings, ttl: Duration) -> bool {
        if !t.probe || !s.breaker.cluster_probes {
            return true;
        }
        let Some(leases) = self.leases.get() else {
            return true;
        };
        match leases.take(&t.deployment, s.breaker.half_open_probes, ttl).await {
            LeaseAnswer::Held(id) => {
                t.lease = Some(id);
                true
            }
            LeaseAnswer::Refused => false,
            LeaseAnswer::Unavailable => true,
        }
    }

    /// Gives a ticket's lease back, with its verdict or when it is abandoned.
    pub(super) fn give_back(&self, t: &Ticket) {
        if let (Some(id), Some(leases)) = (t.lease, self.leases.get()) {
            leases.give_back(&t.deployment, id);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;

    use antirion_core::UtcMicros;
    use parking_lot::Mutex;

    use super::*;
    use crate::class::Class;
    use crate::health::Verdict;
    use crate::settings::HealthConfig;

    #[derive(Debug, Default)]
    struct OneLease {
        held: Mutex<Vec<u64>>,
        next: Mutex<u64>,
    }

    impl ProbeLeases for OneLease {
        fn take<'a>(
            &'a self,
            _d: &'a str,
            limit: u32,
            _ttl: Duration,
        ) -> Pin<Box<dyn Future<Output = LeaseAnswer> + Send + 'a>> {
            Box::pin(async move {
                let mut held = self.held.lock();
                if held.len() >= limit as usize {
                    return LeaseAnswer::Refused;
                }
                let mut next = self.next.lock();
                *next += 1;
                held.push(*next);
                LeaseAnswer::Held(*next)
            })
        }

        fn give_back(&self, _d: &str, lease: u64) {
            self.held.lock().retain(|l| *l != lease);
        }
    }

    #[tokio::test]
    async fn probes_take_and_give_back_cluster_leases() {
        let cfg = HealthConfig::from_doc(&serde_json::json!({"resilience": {"health": {"breaker": {
            "consecutive_failures": 1, "open_duration": "1ms", "half_open_probes": 1, "probe_scope": "cluster"}}}}));
        let s = cfg.for_deployment("a");
        assert!(s.breaker.cluster_probes);
        let leases = Arc::new(OneLease::default());
        let h = HealthRegistry::default();
        h.set_leases(Arc::clone(&leases) as Arc<dyn ProbeLeases>);
        let now = UtcMicros(1_000_000);
        let fail = Verdict {
            class: Class::ServerError,
            status: Some(500),
            hint: None,
            counts: true,
        };
        let t = h.begin("a", "p", None, s, now).unwrap();
        h.finish(&t, &fail, s, now);
        let later = UtcMicros(now.0 + 10_000);
        let mut probe = h.begin("a", "p", None, s, later).unwrap();
        assert!(h.lease(&mut probe, s, Duration::from_secs(1)).await);
        assert_eq!(leases.held.lock().len(), 1);
        leases.held.lock().push(99);
        h.abandon(&probe);
        assert_eq!(
            *leases.held.lock(),
            vec![99],
            "an abandoned probe gives its lease back"
        );
        let mut other = h.begin("a", "p", None, s, later).unwrap();
        assert!(
            !h.lease(&mut other, s, Duration::from_secs(1)).await,
            "another node holds the lease"
        );
    }
}
