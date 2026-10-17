//! Dependency probes. A dependency that failed is probed every `probe_interval` and used again
//! after three good probes in a row: the store then takes the spooled ledger lines and the node
//! catches up with the configuration; the counter store gets the settlements and the local-share
//! consumption it missed. A counter store in use is pinged as often, so an outage is noticed
//! without traffic.

use std::sync::Arc;
use std::time::Duration;

use super::pause;
use crate::Runtime;

pub(crate) async fn run(rt: Arc<Runtime>) {
    let Some(cluster) = &rt.cluster else { return };
    let every = cluster.settings.probe_interval;
    while pause(&rt, every).await {
        if let Some(pg) = rt.store.postgres()
            && pg.is_down()
            && pg.probe().await
        {
            rt.flush_ledger().await;
            if let Err(e) = rt.catch_up().await {
                tracing::warn!(error = %e, "config_apply_failed: the stored generation could not be applied");
            }
            cluster.report_now();
        }
    }
}

/// Probes a shared counter store (in either mode) every `every` until the node stops.
pub(crate) async fn counters(rt: Arc<Runtime>, every: Duration) {
    while pause(&rt, every).await {
        let engine = rt.accounts.engine();
        let changed = if engine.is_store_down() {
            let up = engine.probe().await;
            if up {
                let replayed = engine.flush().await;
                tracing::info!(replayed, "the counter store is available again");
            }
            up
        } else {
            let down = engine.heartbeat().await.is_err();
            if down {
                tracing::warn!("the counter store is unavailable");
            }
            down
        };
        if changed && let Some(c) = &rt.cluster {
            c.report_now();
        }
    }
}
