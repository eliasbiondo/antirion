//! The cache console off the request path: invalidation rules fired by configuration changes, by
//! their schedule, by a request's metadata (`antirion-pipeline` calls [`Runtime::fire_cache_rule`])
//! or by hand, each firing recorded and audited; and the semantic layer's statistics, tallied per
//! node in memory and written to the store four times a second.

mod rules;
mod stats;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use crate::Runtime;

pub use stats::SemanticStats;

const TICK: Duration = Duration::from_millis(250);
const EVERY_SECOND: u32 = 4;
/// How long firings and tallies are kept.
const KEEP_DAYS: i64 = 31;
const DAY_MS: i64 = 86_400_000;

/// Counts of one semantic group on one day that the store does not have yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Tally {
    hits: i64,
    similarity_micros: i64,
    reports: i64,
}

/// What this node counted and when it last looked at the schedules.
#[derive(Debug, Default)]
pub struct CacheConsoleState {
    tallies: Mutex<HashMap<(String, i64), Tally>>,
    /// The last minute whose schedules were evaluated.
    schedule_minute: Mutex<Option<i64>>,
}

impl CacheConsoleState {
    pub(crate) fn clear(&self) {
        self.tallies.lock().clear();
        *self.schedule_minute.lock() = None;
    }
}

/// Starts the scheduled work of this module.
pub fn spawn(rt: &Arc<Runtime>) {
    rt.tasks.spawn(run(Arc::clone(rt)));
}

async fn run(rt: Arc<Runtime>) {
    let mut ticks: u32 = 0;
    loop {
        tokio::select! {
            () = rt.shutdown.cancelled() => {
                rt.flush_semantic_tallies().await;
                return;
            }
            () = tokio::time::sleep(TICK) => {}
        }
        rt.flush_semantic_tallies().await;
        ticks = ticks.wrapping_add(1);
        if !ticks.is_multiple_of(EVERY_SECOND) {
            continue;
        }
        rt.run_schedules().await;
        if ticks.is_multiple_of(EVERY_SECOND * 3600) {
            let now_ms = rt.now().0 / 1000;
            let day = now_ms.div_euclid(DAY_MS);
            if let Err(e) = rt
                .store
                .prune_cache_console((now_ms - KEEP_DAYS * DAY_MS) * 1000, day - KEEP_DAYS)
                .await
            {
                tracing::debug!(error = %e, "old cache firings and tallies could not be removed");
            }
        }
    }
}
