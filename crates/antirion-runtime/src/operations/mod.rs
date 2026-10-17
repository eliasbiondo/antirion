//! Operations of the danger zone that run off the request path: ingress pauses, maintenance
//! windows, scheduled organization deletions and the expiry of export files.

pub mod deletion;
pub mod ingress;
pub mod maintenance;

use std::sync::Arc;
use std::time::Duration;

use crate::Runtime;

/// The store kind of an export job; its id is the job id.
pub const JOB: &str = "job";
/// The store kind of a finished job's file; its `created_us` is when the file expires.
pub const JOB_FILE: &str = "job_file";

/// What a node holds of the operations.
#[derive(Debug, Default)]
pub struct Operations {
    pub pauses: ingress::Pauses,
    pub erased: deletion::Erased,
}

impl Operations {
    /// Forgets the pauses and erasures this node knows of (test mode).
    pub fn clear(&self) {
        self.pauses.clear();
        self.erased.clear();
    }
}

/// Every how many refreshes the deletions and erasures are looked at.
const DELETIONS_EVERY: u32 = 2;
/// Every how many refreshes expired export files are removed.
const FILES_EVERY: u32 = 120;

/// Starts the node's operations task.
pub fn spawn(rt: &Arc<Runtime>) {
    rt.tasks.spawn(watch(Arc::clone(rt)));
}

async fn watch(rt: Arc<Runtime>) {
    rt.refresh_pauses().await;
    let mut tick: u32 = 0;
    loop {
        tokio::select! {
            () = rt.shutdown.cancelled() => return,
            () = tokio::time::sleep(ingress::REFRESH) => {}
        }
        tick = tick.wrapping_add(1);
        rt.refresh_pauses().await;
        if tick.is_multiple_of(DELETIONS_EVERY) {
            rt.delete_due_organizations().await;
            rt.learn_erasures().await;
        }
        if tick.is_multiple_of(FILES_EVERY) {
            let now = rt.now().0;
            if let Err(e) = rt.store.delete_requests_before(JOB_FILE, now).await {
                tracing::debug!(error = %e, "expired export files could not be removed");
            }
        }
    }
}

/// The longest grace of a scheduled deletion.
pub const MAX_GRACE: Duration = Duration::from_secs(90 * 24 * 3600);
/// The grace of a scheduled deletion that names none.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(30 * 24 * 3600);
/// How long an export's file can be downloaded.
pub const FILE_LIFETIME: Duration = Duration::from_secs(24 * 3600);
