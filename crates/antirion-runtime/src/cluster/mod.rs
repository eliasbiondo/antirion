//! Cluster mode: every node serves every request from its own snapshot of the shared store.
//!
//! Background tasks keep a node in step with the others: the configuration follower (`sync`), the
//! heartbeat with membership and drain commands (`members`), dependency probes and recovery
//! (`probe`), background jobs under store leases (`jobs`), the exchange of request-log records,
//! ledger lines and cache epochs with the other nodes (`replicate`), health state shared through
//! the counter store (`health`), and the local snapshot a node starts from when the store is
//! unreachable (`local`).

mod affinity;
mod health;
mod inflight;
mod jobs;
mod local;
mod members;
mod probe;
mod probe_lease;
mod replicate;
mod revocations;
mod sync;

use std::sync::Arc;
use std::time::Duration;

use antirion_core::UtcMicros;

use antirion_store::pg::{JobLease, NodeRow};
use parking_lot::Mutex;
use tokio::sync::Notify;

use crate::Runtime;

pub use affinity::Lookup;
pub use jobs::JOBS;
pub use local::{Loaded, LocalError, SnapshotFile};
pub use sync::SyncError;

/// `cluster.*` timings.
#[derive(Clone, Debug)]
pub struct ClusterSettings {
    pub heartbeat_interval: Duration,
    pub node_ttl: Duration,
    pub probe_interval: Duration,
    pub poll_interval: Duration,
    /// Where the node persists its local snapshot.
    pub snapshot: Option<SnapshotFile>,
}

impl Default for ClusterSettings {
    fn default() -> ClusterSettings {
        ClusterSettings {
            heartbeat_interval: Duration::from_secs(2),
            node_ttl: Duration::from_secs(10),
            probe_interval: Duration::from_secs(1),
            poll_interval: Duration::from_secs(1),
            snapshot: None,
        }
    }
}

/// What this node last learned about the cluster.
#[derive(Clone, Debug, Default)]
pub struct View {
    /// Live nodes, from the store.
    pub nodes: Vec<NodeRow>,
    pub leases: Vec<JobLease>,
    /// This node's clock minus the store's, when the last heartbeat could measure it.
    pub clock_skew_ms: Option<i64>,
}

/// A cluster node's shared-state machinery.
#[derive(Debug)]
pub struct Cluster {
    pub settings: ClusterSettings,
    view: Mutex<View>,
    /// Wakes the heartbeat early, so a newly applied generation is reported at once.
    beat: Notify,
    /// When the serving generation was last confirmed to be the store's newest.
    confirmed: Mutex<Option<UtcMicros>>,
    /// When the last on-demand check of the store's generation began.
    last_check: Mutex<Option<std::time::Instant>>,
    /// The local snapshot's last write; held while writing.
    written: tokio::sync::Mutex<local::Written>,
}

impl Cluster {
    #[must_use]
    pub fn new(settings: ClusterSettings) -> Cluster {
        Cluster {
            settings,
            view: Mutex::new(View::default()),
            beat: Notify::new(),
            confirmed: Mutex::new(None),
            last_check: Mutex::new(None),
            written: tokio::sync::Mutex::new(local::Written::default()),
        }
    }

    #[must_use]
    pub fn view(&self) -> View {
        self.view.lock().clone()
    }

    pub(crate) fn confirm(&self, at: UtcMicros) {
        let mut c = self.confirmed.lock();
        *c = Some(c.map_or(at, |prev| prev.max(at)));
    }

    #[must_use]
    pub fn confirmed(&self) -> Option<UtcMicros> {
        *self.confirmed.lock()
    }

    pub(crate) fn report_now(&self) {
        self.beat.notify_one();
    }
}

impl Runtime {
    /// Serves the local snapshot persisted at `persisted_at` until the store returns: its keys count
    /// as verified then. Budget rebuilds read the store's ledger once it answers.
    pub fn started_from_snapshot(&self, persisted_at: UtcMicros) {
        self.state.verified.assume_all(persisted_at);
    }
}

/// Starts the cluster tasks of `rt` on its task tracker; a shared counter store is probed every
/// `probe_interval` in either mode.
pub fn spawn(rt: &Arc<Runtime>, probe_interval: Duration) {
    if rt.accounts.engine().store().valkey().is_some() {
        rt.tasks.spawn(probe::counters(Arc::clone(rt), probe_interval));
        rt.tasks.spawn(inflight::run(Arc::clone(rt)));
    }
    if rt.cluster.is_none() {
        return;
    }
    rt.tasks.spawn(sync::run(Arc::clone(rt)));
    rt.tasks.spawn(members::run(Arc::clone(rt)));
    rt.tasks.spawn(probe::run(Arc::clone(rt)));
    rt.tasks.spawn(jobs::run(Arc::clone(rt)));
    rt.tasks.spawn(replicate::run(Arc::clone(rt)));
    rt.tasks.spawn(revocations::run(Arc::clone(rt)));
    rt.tasks.spawn(health::run(Arc::clone(rt)));
    rt.tasks.spawn(local::run(Arc::clone(rt)));
    if let Some(valkey) = rt.accounts.engine().store().valkey() {
        rt.state.health.set_leases(Arc::new(probe_lease::ValkeyLeases {
            valkey: valkey.clone(),
            tasks: rt.tasks.clone(),
        }));
    }
}

/// Sleeps for `d` unless the node stops first; false when it stops.
pub(crate) async fn pause(rt: &Runtime, d: Duration) -> bool {
    tokio::select! {
        () = rt.shutdown.cancelled() => false,
        () = tokio::time::sleep(d) => true,
    }
}
