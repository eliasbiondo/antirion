//! The cluster's half-open probe leases on the shared counter store.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use antirion_limits::store::valkey::ValkeyCounters;
use antirion_resilience::{LeaseAnswer, ProbeLeases};
use tokio_util::task::TaskTracker;

/// How long taking a lease may wait for the store before the node probes on its own slots.
const TAKE_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Debug)]
pub(crate) struct ValkeyLeases {
    pub(crate) valkey: ValkeyCounters,
    pub(crate) tasks: TaskTracker,
}

impl ProbeLeases for ValkeyLeases {
    fn take<'a>(
        &'a self,
        deployment: &'a str,
        limit: u32,
        ttl: Duration,
    ) -> Pin<Box<dyn Future<Output = LeaseAnswer> + Send + 'a>> {
        Box::pin(async move {
            let id =
                u64::try_from(antirion_core::ResourceUid::random().0 & u128::from(u64::MAX)).unwrap_or(0);
            let ttl_ms = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX);
            match tokio::time::timeout(
                TAKE_TIMEOUT,
                self.valkey.probe_lease(deployment, limit, ttl_ms, id),
            )
            .await
            {
                Ok(Ok(true)) => LeaseAnswer::Held(id),
                Ok(Ok(false)) => LeaseAnswer::Refused,
                _ => LeaseAnswer::Unavailable,
            }
        })
    }

    fn give_back(&self, deployment: &str, lease: u64) {
        let valkey = self.valkey.clone();
        let deployment = deployment.to_owned();
        self.tasks.spawn(async move {
            if let Err(e) = valkey.probe_release(&deployment, lease).await {
                tracing::debug!(error = %e, "a probe lease expires on its own");
            }
        });
    }
}
