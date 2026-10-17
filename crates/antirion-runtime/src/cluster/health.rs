//! Health state shared through the counter store: every `EVERY`, the node writes the changes it
//! made to breakers, cooldowns, keys and manual disables, then, when the shared version moved,
//! reads every record and adopts those newer than its own state. A request never waits on the
//! store. While the store is unreachable the node keeps enforcing its own state and writes its
//! changes when the store returns.

use std::sync::Arc;
use std::time::Duration;

use antirion_resilience::SharedRecord;

use super::pause;
use crate::Runtime;

/// How often a node exchanges health state: the bound on how stale another node's change is.
const EVERY: Duration = Duration::from_millis(250);

pub(crate) async fn run(rt: Arc<Runtime>) {
    rt.state.health.share();
    let mut seen: Option<i64> = None;
    while pause(&rt, EVERY).await {
        let engine = rt.accounts.engine();
        let Some(valkey) = engine.store().valkey() else {
            return;
        };
        if engine.is_store_down() {
            continue;
        }
        let out: Vec<SharedRecord> = rt.state.health.take_shared();
        if !out.is_empty() {
            let records: Vec<(String, i64, String)> = out
                .iter()
                .map(|r| (r.field.clone(), r.stamp, r.value.clone()))
                .collect();
            if let Err(e) = valkey.health_put(&records).await {
                tracing::debug!(error = %e, "health changes are shared with the next round");
                rt.state.health.requeue_shared(out);
                continue;
            }
        }
        let Ok(version) = valkey.health_version().await else {
            continue;
        };
        if seen == Some(version) {
            continue;
        }
        match valkey.health_records().await {
            Ok(all) => {
                let unread = rt.state.health.merge_shared(&all, rt.now());
                if unread > 0 {
                    tracing::debug!(unread, "shared health records this node cannot read");
                }
                seen = Some(version);
            }
            Err(e) => tracing::debug!(error = %e, "shared health state could not be read"),
        }
    }
}
