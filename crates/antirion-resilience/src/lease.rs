//! Cluster-wide half-open probe leases (`breaker.probe_scope: cluster`): a node sends a probe to a
//! half-open deployment only while it holds one of the deployment's `half_open_probes` leases in the
//! shared store. A lease is given back with the probe's verdict and expires on its own after the
//! attempt's timers; while the store cannot answer, nodes probe on their own slots alone.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// What the shared store answered to a lease request.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LeaseAnswer {
    /// A lease, by its id.
    Held(u64),
    /// Every lease of the deployment is held.
    Refused,
    /// The store could not be asked.
    Unavailable,
}

/// The shared store of probe leases.
pub trait ProbeLeases: Send + Sync + fmt::Debug {
    /// Takes one of `limit` leases on `deployment`'s probes, for at most `ttl`.
    fn take<'a>(
        &'a self,
        deployment: &'a str,
        limit: u32,
        ttl: Duration,
    ) -> Pin<Box<dyn Future<Output = LeaseAnswer> + Send + 'a>>;

    /// Gives a lease back; never waits for the store.
    fn give_back(&self, deployment: &str, lease: u64);
}
