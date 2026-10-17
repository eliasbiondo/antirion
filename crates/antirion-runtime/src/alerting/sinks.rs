//! Audit sinks: every audit entry is delivered to each enabled `governance.audit_sinks` entry in
//! the log's order, at least once. The audit log in the store is the spool: each sink keeps a
//! position in it (also in the store), moved only after a batch was accepted, so entries survive a
//! restart and a failing sink loses nothing until `max_retry_duration`, after which the entries
//! older than that are skipped, counted and reported. One node delivers (the `audit_sinks` lease).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use antirion_core::audit::AuditEntry;
use antirion_core::{SplitMix64, UtcMicros};
use antirion_obs::sink::backoff;
use antirion_store::{DeliveryQuery, DeliveryRow, StoreError};
use serde_json::{Value, json};
use tokio::time::Instant;

use super::config::AuditSink;
use super::deliver::{TestOutcome, env};
use crate::Runtime;
use crate::telemetry::deliver::{Sent, send};

const TICK: Duration = Duration::from_millis(100);
const TIMEOUT: Duration = Duration::from_secs(10);
const LEASE: &str = "audit_sinks";
const LEASE_TTL: Duration = Duration::from_secs(15);
const RENEW_EVERY: Duration = Duration::from_secs(5);
const DAY_US: i64 = 86_400_000_000;

#[derive(Default)]
struct SinkState {
    last_flush: Option<Instant>,
    retry_at: Option<Instant>,
    tries: u32,
}

fn encode(sink: &AuditSink, entries: &[AuditEntry]) -> (Vec<u8>, &'static str) {
    if sink.json_array {
        (
            serde_json::to_vec(entries).unwrap_or_default(),
            "application/json",
        )
    } else {
        let mut out = Vec::new();
        for e in entries {
            if serde_json::to_writer(&mut out, e).is_ok() {
                out.push(b'\n');
            }
        }
        (out, "application/x-ndjson")
    }
}

async fn post(rt: &Runtime, sink: &AuditSink, body: Vec<u8>, content_type: &'static str) -> Sent {
    send(
        &env(rt),
        &sink.url,
        &sink.headers,
        content_type,
        body,
        false,
        TIMEOUT,
    )
    .await
}

async fn log(rt: &Runtime, sink: &str, items: usize, sent: Option<Sent>, result: &str) {
    let row = DeliveryRow {
        event_id: super::uuid(),
        target_kind: "audit_sink".into(),
        target: sink.to_owned(),
        event_type: "audit".into(),
        attempt: 1,
        time_us: rt.now().0,
        status_code: sent.and_then(|s| s.status).map(i32::from),
        result: result.to_owned(),
        items: i64::try_from(items).unwrap_or(i64::MAX),
    };
    if let Err(e) = rt.store.record_delivery(row).await {
        tracing::debug!(error = %e, "an audit sink delivery could not be recorded");
    }
}

/// The position a sink starts from: the last entry written before the governance settings that
/// configured it were committed.
async fn start(rt: &Runtime, name: &str) -> Result<i64, StoreError> {
    if let Some(c) = rt.store.sink_cursor(name).await? {
        return Ok(c);
    }
    let since = rt
        .gens
        .load()
        .config
        .meta
        .settings
        .get("governance")
        .and_then(|m| UtcMicros::parse_rfc3339(&m.updated_at).ok());
    let head = rt.store.audit_head().await?;
    let mut cursor = head;
    if let Some(since) = since {
        let mut after = 0;
        loop {
            let page = rt.store.audit_after(after, 500).await?;
            let Some(last) = page.last().map(|e| e.0) else {
                break;
            };
            if let Some(first_new) = page.iter().find(|e| e.1 >= since.0) {
                cursor = first_new.0 - 1;
                break;
            }
            after = last;
        }
    }
    rt.store.set_sink_cursor(name, cursor).await?;
    Ok(cursor)
}

/// One step of one sink: skips entries past their retry limit, then sends the next batch when one is
/// due.
async fn step(rt: &Runtime, sink: &AuditSink, st: &mut SinkState) -> Result<(), StoreError> {
    if st.retry_at.is_some_and(|t| Instant::now() < t) {
        return Ok(());
    }
    let mut cursor = start(rt, &sink.name).await?;
    let now = rt.now().0;
    let batch = rt.store.audit_after(cursor, sink.batch_max_records).await?;
    let expired: Vec<&(i64, i64, AuditEntry)> = batch
        .iter()
        .take_while(|e| now - e.1 > sink.max_retry_us)
        .collect();
    if let Some(last) = expired.last() {
        cursor = last.0;
        rt.store.set_sink_cursor(&sink.name, cursor).await?;
        log(rt, &sink.name, expired.len(), None, "dropped").await;
        tracing::warn!(sink = %sink.name, dropped = expired.len(), "audit entries were dropped after max_retry_duration");
        return Ok(());
    }
    if batch.is_empty() {
        return Ok(());
    }
    let full = batch.len() >= sink.batch_max_records;
    let due = st.last_flush.is_none_or(|t| t.elapsed() >= sink.flush_interval);
    if !full && !due {
        return Ok(());
    }
    st.last_flush = Some(Instant::now());
    let entries: Vec<AuditEntry> = batch.iter().map(|e| e.2.clone()).collect();
    let (body, content_type) = encode(sink, &entries);
    let sent = post(rt, sink, body, content_type).await;
    log(rt, &sink.name, entries.len(), Some(sent), sent.result()).await;
    if sent.ok() {
        if let Some(last) = batch.last() {
            rt.store.set_sink_cursor(&sink.name, last.0).await?;
        }
        st.tries = 0;
        st.retry_at = None;
    } else {
        st.tries = st.tries.saturating_add(1);
        #[allow(clippy::cast_precision_loss)]
        let u = (SplitMix64::from_entropy().next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        st.retry_at = Some(Instant::now() + backoff(st.tries, u));
    }
    Ok(())
}

pub(crate) async fn run(rt: Arc<Runtime>) {
    let mut states: HashMap<String, SinkState> = HashMap::new();
    let mut renewed: Option<Instant> = None;
    loop {
        tokio::select! {
            () = rt.shutdown.cancelled() => return,
            () = tokio::time::sleep(TICK) => {}
        }
        let cfg = rt.alerting();
        if cfg.audit_sinks.is_empty() {
            continue;
        }
        if rt.cluster.is_some()
            && let Some(pg) = rt.store.postgres()
            && !renewed.is_some_and(|t| t.elapsed() < RENEW_EVERY)
        {
            if matches!(pg.lease(LEASE, &rt.node.id, LEASE_TTL).await, Ok(true)) {
                renewed = Some(Instant::now());
            } else {
                renewed = None;
                continue;
            }
        }
        states.retain(|n, _| cfg.audit_sinks.iter().any(|s| &s.name == n));
        for sink in cfg.audit_sinks.iter().filter(|s| s.enabled) {
            let st = states.entry(sink.name.clone()).or_default();
            if let Err(e) = step(&rt, sink, st).await {
                tracing::debug!(error = %e, sink = %sink.name, "an audit sink could not be served");
            }
        }
    }
}

/// `GET /runtime/audit/sinks`: each configured sink's delivery status.
pub async fn status(rt: &Runtime) -> Result<Vec<Value>, StoreError> {
    let now = rt.now().0;
    let (rows, _) = rt
        .store
        .deliveries(DeliveryQuery {
            target_kind: Some("audit_sink".into()),
            since_us: now - DAY_US,
            limit: 100_000,
            ..DeliveryQuery::default()
        })
        .await?;
    let mut out = Vec::new();
    for sink in &rt.alerting().audit_sinks {
        let mine: Vec<&DeliveryRow> = rows
            .iter()
            .map(|r| &r.1)
            .filter(|r| r.target == sink.name)
            .collect();
        let entries: i64 = mine
            .iter()
            .filter(|r| r.result == "delivered")
            .map(|r| r.items)
            .sum();
        let failures = mine
            .iter()
            .filter(|r| r.result == "failed" || r.result == "timeout")
            .count();
        let last_write = mine
            .iter()
            .filter(|r| r.result == "delivered")
            .map(|r| r.time_us)
            .max();
        let state = match mine.first().map(|r| r.result.as_str()) {
            Some("failed" | "timeout") => "retrying",
            Some("dropped") => "dropping",
            _ => "ok",
        };
        let (queued, oldest) = match rt.store.sink_cursor(&sink.name).await? {
            Some(c) => rt.store.audit_backlog(c).await?,
            None => (0, None),
        };
        out.push(json!({
            "name": sink.name,
            "enabled": sink.enabled,
            "last_write_at": last_write.map(|t| UtcMicros(t).to_rfc3339()),
            "entries_24h": entries,
            "failures_24h": failures,
            "queued": queued,
            "oldest_queued_at": oldest.map(|t| UtcMicros(t).to_rfc3339()),
            "state": state,
        }));
    }
    Ok(out)
}

/// `POST /runtime/audit/sinks/{name}:test`: one entry marked as a test; `None` for an unknown sink.
pub async fn test(rt: &Runtime, name: &str) -> Option<TestOutcome> {
    let sink = rt
        .alerting()
        .audit_sinks
        .iter()
        .find(|s| s.name == name)
        .cloned()?;
    let entry = json!({
        "id": super::uuid(),
        "time": rt.now().to_rfc3339(),
        "test": true,
        "action": "audit_sink.test",
        "resource": format!("governance/audit_sinks/{name}"),
        "outcome": "success",
    });
    let (body, content_type) = if sink.json_array {
        (
            serde_json::to_vec(&[&entry]).unwrap_or_default(),
            "application/json",
        )
    } else {
        let mut b = serde_json::to_vec(&entry).unwrap_or_default();
        b.push(b'\n');
        (b, "application/x-ndjson")
    };
    let started = Instant::now();
    let sent = post(rt, &sink, body, content_type).await;
    let latency = started.elapsed();
    Some(TestOutcome {
        ok: sent.ok(),
        status_code: sent.status,
        latency_ms: Some((latency.as_secs_f64() * 1e6).round() / 1e3),
        error: (!sent.ok()).then(|| match sent.status {
            Some(s) => format!("the endpoint answered {s}"),
            None => "the endpoint could not be reached".to_owned(),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sink(json_array: bool) -> AuditSink {
        AuditSink {
            name: "s".into(),
            enabled: true,
            url: "https://s".into(),
            headers: Vec::new(),
            json_array,
            batch_max_records: 2,
            flush_interval: Duration::from_secs(1),
            max_retry_us: 1,
        }
    }

    #[test]
    fn batches_are_ndjson_or_an_array() {
        let e: AuditEntry = serde_json::from_value(json!({
            "id": "a", "time": "t", "actor": {"type": "token", "id": "x"}, "action": "a", "resource": "r",
            "outcome": "success", "status_code": 200, "generation_before": null, "generation_after": null,
            "request_id": "q", "source_ip": null, "changes": null}))
        .unwrap();
        let (nd, ct) = encode(&sink(false), &[e.clone(), e.clone()]);
        assert_eq!(ct, "application/x-ndjson");
        assert_eq!(String::from_utf8(nd).unwrap().lines().count(), 2);
        let (arr, ct) = encode(&sink(true), &[e]);
        assert_eq!(ct, "application/json");
        assert!(serde_json::from_slice::<Value>(&arr).unwrap().is_array());
    }
}
