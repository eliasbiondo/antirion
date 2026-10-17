//! The heartbeat: every `heartbeat_interval` (and at once after a generation is applied) a node
//! records its state in the store and its membership in the counter store, reads the live nodes,
//! picks up a drain another node asked for, and measures its clock against the store's.

use std::sync::Arc;
use std::time::Duration;

use antirion_store::pg::NodeRow;

use super::Cluster;
use crate::Runtime;
use crate::drain::Origin;

/// How long a membership call to the counter store may take before the store's count is used.
const ANNOUNCE_TIMEOUT: Duration = Duration::from_millis(250);

/// Clock skew above this is reported as a warning.
const SKEW_WARNING_MS: i64 = 1000;

pub(crate) async fn run(rt: Arc<Runtime>) {
    let Some(cluster) = &rt.cluster else { return };
    if let Some(pg) = rt.store.postgres()
        && let Err(e) = pg.clear_drain(&rt.node.id).await
    {
        tracing::debug!(error = %e, "an earlier drain command could not be cleared");
    }
    loop {
        beat(&rt, cluster, true).await;
        tokio::select! {
            () = rt.shutdown.cancelled() => {
                beat(&rt, cluster, false).await;
                return;
            }
            () = tokio::time::sleep(cluster.settings.heartbeat_interval) => {}
            () = cluster.beat.notified() => {}
        }
    }
}

async fn beat(rt: &Runtime, cluster: &Cluster, running: bool) {
    let draining = rt.drain.is_draining();
    let row = NodeRow {
        node_id: rt.node.id.clone(),
        version: rt.node.version.clone(),
        config_generation: rt.gens.load().generation,
        ready: running && crate::probes::ready(rt),
        draining,
        degraded: rt.degraded().into_iter().map(str::to_owned).collect(),
        last_seen_us: 0,
        alive: true,
        load: crate::probes::load(rt),
    };
    let ttl = cluster.settings.node_ttl;
    let Some(pg) = rt.store.postgres() else { return };
    match pg.heartbeat(&row).await {
        Ok(true) if running && !draining => {
            tracing::info!("another node asked this node to drain");
            rt.drain.start(Origin::Api);
        }
        Ok(_) => {}
        Err(e) => tracing::debug!(error = %e, "the heartbeat could not be stored"),
    }
    let engine = rt.accounts.engine();
    let mut known = None;
    if let Some(v) = engine.store().valkey()
        && !engine.is_store_down()
    {
        let ttl_ms = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX);
        if let Ok(Ok(n)) = tokio::time::timeout(ANNOUNCE_TIMEOUT, v.announce(&rt.node.id, ttl_ms)).await {
            known = Some(n);
        }
    }
    if let Ok(nodes) = pg.nodes(ttl).await {
        let live: Vec<NodeRow> = nodes.into_iter().filter(|n| n.alive).collect();
        if known.is_none() {
            known = Some(u32::try_from(live.len()).unwrap_or(u32::MAX).max(1));
        }
        cluster.view.lock().nodes = live;
    }
    if let Some(n) = known {
        engine.set_known_nodes(n);
    }
    let before = rt.now();
    if let Ok(store_now) = pg.now_us().await {
        let after = rt.now();
        let local = before.0 + (after.0 - before.0) / 2;
        let skew_ms = (local - store_now) / 1000;
        if skew_ms.abs() > SKEW_WARNING_MS {
            tracing::warn!(skew_ms, "this node's clock differs from the store's");
        }
        cluster.view.lock().clock_skew_ms = Some(skew_ms);
    }
}
