//! The delivery status of webhooks and channels, cluster-wide: counts from the shared delivery log
//! and queue depths every node reports. Also the previous signing secrets a rotation's overlap still
//! accepts, which every node reads back from the store.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use antirion_store::{DeliveryQuery, QueueRow, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Runtime;

const REPORT_EVERY: Duration = Duration::from_secs(2);
/// Queue reports older than this are from nodes that stopped.
const FRESH_US: i64 = 15_000_000;
const DAY_US: i64 = 86_400_000_000;

/// The store key of a webhook's previous signing secret.
pub(crate) fn previous_key(webhook: &str) -> String {
    format!("alert_webhook_previous_secret/{webhook}")
}

/// A previous signing secret as kept in the store: sealed, with the end of its overlap.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Previous {
    pub(crate) sealed: String,
    pub(crate) until_us: i64,
}

/// Reports this node's queues and reads back the previous signing secrets, until the node stops.
pub(crate) async fn report(rt: Arc<Runtime>) {
    loop {
        tokio::select! {
            () = rt.shutdown.cancelled() => return,
            () = tokio::time::sleep(REPORT_EVERY) => {}
        }
        let now = rt.now().0;
        for (kind, target, depth, oldest) in rt.state.alerting.queue_depths() {
            let row = QueueRow {
                node_id: rt.node.id.clone(),
                target_kind: kind,
                target,
                depth: i64::try_from(depth).unwrap_or(i64::MAX),
                oldest_us: oldest,
                updated_us: now,
            };
            if let Err(e) = rt.store.report_queue(row).await {
                tracing::debug!(error = %e, "a delivery queue could not be reported");
                break;
            }
        }
        let hooks: Vec<String> = rt.gens.load().alerts.iter().map(|h| h.name.clone()).collect();
        for name in hooks {
            let Ok(stored) = rt.store.value(&previous_key(&name)).await else {
                break;
            };
            let previous = stored
                .and_then(|v| serde_json::from_slice::<Previous>(&v).ok())
                .filter(|p| p.until_us > now)
                .and_then(|p| {
                    let plain = rt.sealer.open(&previous_key(&name), &p.sealed).ok()?;
                    Some((String::from_utf8(plain).ok()?, p.until_us))
                });
            rt.state.alerting.set_previous_secret(&name, previous);
        }
    }
}

#[derive(Default)]
struct Counts {
    delivered: u64,
    failed: u64,
    last_time: Option<i64>,
    last_status: Option<i32>,
}

/// `{name, enabled, last_delivery_at, last_status, deliveries_24h, failures_24h, queued}` for every
/// configured webhook (`kind` `webhook`) or channel (`channel`), by name.
pub async fn targets(rt: &Runtime, kind: &str) -> Result<Vec<Value>, StoreError> {
    let now = rt.now().0;
    let names: Vec<(String, bool)> = if kind == "webhook" {
        rt.gens
            .load()
            .alerts
            .iter()
            .map(|h| (h.name.clone(), h.enabled))
            .collect()
    } else {
        rt.alerting()
            .channels
            .values()
            .map(|c| (c.name.clone(), c.enabled))
            .collect()
    };
    let (rows, _) = rt
        .store
        .deliveries(DeliveryQuery {
            target_kind: Some(kind.to_owned()),
            since_us: now - DAY_US,
            limit: 100_000,
            ..DeliveryQuery::default()
        })
        .await?;
    let mut counts: BTreeMap<String, Counts> = BTreeMap::new();
    for (_, d) in rows {
        let c = counts.entry(d.target.clone()).or_default();
        if d.result == "delivered" {
            c.delivered += 1;
        } else {
            c.failed += 1;
        }
        if c.last_time.is_none_or(|t| d.time_us > t) {
            c.last_time = Some(d.time_us);
            c.last_status = d.status_code;
        }
    }
    let queues = rt.store.queues(now - FRESH_US).await?;
    let mut out = Vec::new();
    for (name, enabled) in names {
        let c = counts.remove(&name).unwrap_or_default();
        let queued: i64 = queues
            .iter()
            .filter(|q| q.target_kind == kind && q.target == name)
            .map(|q| q.depth)
            .sum();
        out.push(json!({
            "name": name,
            "enabled": enabled,
            "last_delivery_at": c.last_time.map(|t| antirion_core::UtcMicros(t).to_rfc3339()),
            "last_status": c.last_status,
            "deliveries_24h": c.delivered,
            "failures_24h": c.failed,
            "queued": queued.max(0),
        }));
    }
    out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Ok(out)
}
