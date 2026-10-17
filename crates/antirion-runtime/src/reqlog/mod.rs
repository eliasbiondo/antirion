//! The request log (the analytics store sink): records are queued at settlement with their rollups,
//! and one task writes both to the store in batches. Every read first writes what is queued, so a
//! node reads its own records at once; other nodes' records are read from the shared store. New
//! records are also offered to the live tails. With `store.logs.backend: clickhouse` the records go
//! to ClickHouse and every record read comes from there; rollups and feedback stay in the primary
//! store. A full queue drops its oldest record and counts it, and a store that fails or drops marks
//! responses `observability` degraded; neither ever holds a request.

pub mod analytics;
pub mod facts;
mod feedback;
pub use antirion_store::logs::FeedbackRow;
pub mod rollup;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use antirion_obs::Fam;
use antirion_store::StoreError;
use antirion_store::chlogs::ClickHouseLogs;
use antirion_store::logs::{ListedLog, LogFilter, LogOrder, LogRow, StoredLog};
use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::broadcast;

use crate::Runtime;
pub use facts::{Extras, Facts, environment_of};

/// Records waiting to be written; past this the oldest are dropped.
const QUEUE_CAPACITY: usize = 50_000;
/// How often queued records are written.
const WRITE_EVERY: Duration = Duration::from_millis(250);
/// Records written per store call.
const BATCH: usize = 1_000;
/// How often (on the gateway's clock) records past their retention are deleted.
const EXPIRE_EVERY_US: i64 = 60_000_000;
/// Live tails a node serves at once.
pub const MAX_TAILS: usize = 10;

/// A record offered to the live tails, with the facts kept beside it.
#[derive(Clone, Debug)]
pub struct Tailed {
    pub record: Arc<Value>,
    pub extras: Extras,
}

#[derive(Debug, Default)]
struct Queue {
    rows: VecDeque<LogRow>,
    rollups: rollup::Pending,
}

/// The node's side of the request log.
#[derive(Debug)]
pub struct RequestLogs {
    queue: Mutex<Queue>,
    /// Held while a batch is written, so a read that flushes waits for a write in progress.
    write: tokio::sync::Mutex<()>,
    tail: broadcast::Sender<Tailed>,
    tails: Arc<AtomicUsize>,
    /// When retention last ran, on the gateway's clock.
    expired_at: Mutex<Option<i64>>,
    /// The records' store when it is not the primary one.
    clickhouse: OnceLock<Arc<ClickHouseLogs>>,
    /// The queue dropped a record since the last write that succeeded.
    dropping: AtomicBool,
    /// The last write failed.
    failing: AtomicBool,
}

impl Default for RequestLogs {
    fn default() -> RequestLogs {
        RequestLogs {
            queue: Mutex::new(Queue::default()),
            write: tokio::sync::Mutex::new(()),
            tail: broadcast::channel(1_024).0,
            tails: Arc::new(AtomicUsize::new(0)),
            expired_at: Mutex::new(None),
            clickhouse: OnceLock::new(),
            dropping: AtomicBool::new(false),
            failing: AtomicBool::new(false),
        }
    }
}

/// A live tail's slot; dropping it frees the slot.
#[derive(Debug)]
pub struct TailSlot {
    tails: Arc<AtomicUsize>,
    pub receiver: broadcast::Receiver<Tailed>,
}

impl Drop for TailSlot {
    fn drop(&mut self) {
        self.tails.fetch_sub(1, Ordering::AcqRel);
    }
}

impl RequestLogs {
    /// Sends the records to ClickHouse instead of the primary store (set once, at startup).
    pub fn use_clickhouse(&self, logs: Arc<ClickHouseLogs>) {
        let _ = self.clickhouse.set(logs);
    }

    fn clickhouse(&self) -> Option<&Arc<ClickHouseLogs>> {
        self.clickhouse.get()
    }

    /// Why responses list `observability` in `x-antirion-degraded` for a ClickHouse log store, if
    /// they do.
    #[must_use]
    pub fn degraded(&self) -> Option<&'static str> {
        self.clickhouse()?;
        if self.dropping.load(Ordering::Relaxed) {
            Some("queue_full")
        } else if self.failing.load(Ordering::Relaxed) {
            Some("delivery_failing")
        } else {
            None
        }
    }

    /// How often queued records are written, and how many per batch.
    fn pace(&self) -> (Duration, usize) {
        match self.clickhouse() {
            Some(c) => (c.settings().flush_interval, c.settings().batch_size.max(1)),
            None => (WRITE_EVERY, BATCH),
        }
    }

    /// A live tail of the records written from now on; `None` when [`MAX_TAILS`] are open.
    pub fn subscribe(&self) -> Option<TailSlot> {
        let taken = self.tails.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < MAX_TAILS).then_some(n + 1)
        });
        taken.ok().map(|_| TailSlot {
            tails: Arc::clone(&self.tails),
            receiver: self.tail.subscribe(),
        })
    }

    fn take(&self, max: usize) -> (Vec<LogRow>, rollup::Pending) {
        let mut q = self.queue.lock();
        let n = q.rows.len().min(max);
        let rows = q.rows.drain(..n).collect();
        (rows, std::mem::take(&mut q.rollups))
    }

    fn put_back(&self, rows: Vec<LogRow>, rollups: Vec<antirion_store::logs::RollupRow>) {
        let mut q = self.queue.lock();
        for r in rows.into_iter().rev() {
            q.rows.push_front(r);
        }
        while q.rows.len() > QUEUE_CAPACITY {
            q.rows.pop_back();
        }
        q.rollups.restore(rollups);
    }

    fn clear_queue(&self) {
        let mut q = self.queue.lock();
        q.rows.clear();
        q.rollups.clear();
    }
}

/// A stored record with its content put back.
#[must_use]
pub fn joined(log: &StoredLog) -> Value {
    let mut v: Value = serde_json::from_str(&log.record).unwrap_or(Value::Null);
    if let Some(o) = v.as_object_mut() {
        let content = log
            .content
            .as_deref()
            .and_then(|c| serde_json::from_str(c).ok())
            .unwrap_or(Value::Null);
        o.insert("content".into(), content);
    }
    v
}

impl Runtime {
    /// Queues a settled record and its rollups, and offers it to the live tails.
    pub fn keep_log(&self, record: Arc<Value>, extras: Extras) {
        let snap = self.gens.load();
        let tags: Vec<String> = record
            .get("tags")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default();
        let environment = environment_of(&tags, &snap.obs.environment_tag);
        let facts = Facts::of(&record, &extras, environment);
        let row = facts.row(&record, &self.node.id);
        let dropped = {
            let mut q = self.state.request_logs.queue.lock();
            let full = q.rows.len() >= QUEUE_CAPACITY;
            if full {
                q.rows.pop_front();
            }
            q.rows.push_back(row);
            q.rollups.add(&facts);
            full
        };
        if dropped {
            self.state.request_logs.dropping.store(true, Ordering::Relaxed);
            self.metrics
                .add(Fam::LogRecords, &["store", "dropped_queue_full"], 1.0);
        }
        let _ = self.state.request_logs.tail.send(Tailed { record, extras });
    }

    /// Writes the queued records and rollups.
    pub async fn flush_logs(&self) -> Result<(), StoreError> {
        let logs = &self.state.request_logs;
        let _writing = logs.write.lock().await;
        let (_, batch) = logs.pace();
        loop {
            let (rows, mut pending) = logs.take(batch);
            if rows.is_empty() && pending.is_empty() {
                return Ok(());
            }
            let rollups = pending.drain(&self.node.id);
            let more = rows.len() == batch;
            let written = match logs.clickhouse() {
                Some(c) => c.insert(rows.clone()).await,
                None => self.store.insert_logs(rows.clone()).await,
            };
            logs.failing.store(written.is_err(), Ordering::Relaxed);
            if let Err(e) = written {
                logs.put_back(rows, rollups);
                return Err(e);
            }
            logs.dropping.store(false, Ordering::Relaxed);
            if let Err(e) = self
                .store
                .merge_rollups(rollups.clone(), rollup::merge_encoded)
                .await
            {
                logs.put_back(Vec::new(), rollups);
                return Err(e);
            }
            if !more {
                return Ok(());
            }
        }
    }

    /// Writes queued records every quarter second (ClickHouse: every `flush_interval`) until the
    /// node stops, then once more.
    pub async fn write_logs(self: Arc<Self>) {
        loop {
            let (every, _) = self.state.request_logs.pace();
            let stop = tokio::select! {
                () = self.shutdown.cancelled() => true,
                () = tokio::time::sleep(every) => false,
            };
            if let Err(e) = self.flush_logs().await
                && !e.is_outage()
            {
                tracing::warn!(error = %e, "request-log records could not be written");
            }
            if let Err(e) = self.flush_safety().await
                && !e.is_outage()
            {
                tracing::warn!(error = %e, "safety events could not be written");
            }
            self.announce_safety().await;
            self.expire_when_due().await;
            if let Err(e) = self.flush_pressure().await
                && !e.is_outage()
            {
                tracing::warn!(error = %e, "provider pressure could not be written");
            }
            if stop {
                return;
            }
        }
    }

    /// One stored record, with its content.
    pub async fn stored_log(&self, id: &str) -> Result<Option<Value>, StoreError> {
        self.read_logs().await;
        Ok(self.stored_record(id).await?.as_ref().map(joined))
    }

    async fn stored_record(&self, id: &str) -> Result<Option<StoredLog>, StoreError> {
        match self.state.request_logs.clickhouse() {
            Some(c) => c.log(id).await,
            None => self.store.log(id).await,
        }
    }

    /// A page of stored records matching `f`, in `order`.
    pub async fn list_logs(&self, f: LogFilter, order: LogOrder) -> Result<Vec<ListedLog>, StoreError> {
        self.read_logs().await;
        self.list_stored(f, order).await
    }

    /// [`Runtime::list_logs`] without writing the queue first.
    pub(crate) async fn list_stored(
        &self,
        f: LogFilter,
        order: LogOrder,
    ) -> Result<Vec<ListedLog>, StoreError> {
        match self.state.request_logs.clickhouse() {
            Some(c) => c.list(&f, &order).await,
            None => self.store.list_logs(f, order).await,
        }
    }

    /// How many stored records match `f`, counting at most `cap + 1`.
    pub async fn count_logs(&self, f: LogFilter, cap: u64) -> Result<u64, StoreError> {
        self.read_logs().await;
        match self.state.request_logs.clickhouse() {
            Some(c) => c.count(&f, cap).await,
            None => self.store.count_logs(f, cap).await,
        }
    }

    pub(crate) async fn read_logs(&self) {
        if let Err(e) = self.flush_logs().await {
            tracing::debug!(error = %e, "queued request-log records could not be written before a read");
        }
        self.expire_when_due().await;
    }

    /// Deletes records past `retention` and the content of records past `content_retention`.
    pub async fn expire_logs(&self) -> Result<u64, StoreError> {
        let now = self.now().0;
        *self.state.request_logs.expired_at.lock() = Some(now);
        let s = &self.gens.load().obs.request_logs;
        let age = |d: Duration| now.saturating_sub(i64::try_from(d.as_micros()).unwrap_or(i64::MAX));
        let (before, content_before) = (age(s.retention), age(s.content_retention.min(s.retention)));
        self.expire_safety(before).await?;
        match self.state.request_logs.clickhouse() {
            Some(c) => {
                let n = c.expire(before, content_before).await?;
                self.store.expire_logs(before, content_before).await?;
                Ok(n)
            }
            None => self.store.expire_logs(before, content_before).await,
        }
    }

    async fn expire_when_due(&self) {
        let now = self.now().0;
        let due = self
            .state
            .request_logs
            .expired_at
            .lock()
            .is_none_or(|at| !(0..EXPIRE_EVERY_US).contains(&now.saturating_sub(at)));
        if due && let Err(e) = self.expire_logs().await {
            tracing::debug!(error = %e, "request-log retention failed");
        }
    }

    /// Changes a record in place, queued or stored; false when no record has that id.
    pub async fn patch_log(&self, id: &str, f: impl FnOnce(&mut Value)) -> bool {
        let f = {
            let mut q = self.state.request_logs.queue.lock();
            match q.rows.iter_mut().find(|r| r.id == id) {
                Some(row) => {
                    if let Ok(mut v) = serde_json::from_str::<Value>(&row.record) {
                        f(&mut v);
                        row.record = v.to_string();
                    }
                    return true;
                }
                None => f,
            }
        };
        let _writing = self.state.request_logs.write.lock().await;
        let Ok(Some(stored)) = self.stored_record(id).await else {
            return false;
        };
        let Ok(mut v) = serde_json::from_str::<Value>(&stored.record) else {
            return false;
        };
        f(&mut v);
        let updated = match self.state.request_logs.clickhouse() {
            Some(c) => c.update(id, v.to_string()).await,
            None => self.store.update_log(id, v.to_string()).await,
        };
        updated.unwrap_or(false)
    }

    /// Forgets every record, queued and stored.
    pub async fn clear_logs(&self) -> Result<(), StoreError> {
        let _writing = self.state.request_logs.write.lock().await;
        self.state.request_logs.clear_queue();
        *self.state.request_logs.expired_at.lock() = None;
        if let Some(c) = self.state.request_logs.clickhouse() {
            c.clear().await?;
        }
        self.clear_safety().await?;
        self.store.clear_logs().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_is_joined_back() {
        let log = StoredLog {
            record: r#"{"id":"a","content":null}"#.into(),
            content: Some(r#"{"request":{"x":1},"response":null}"#.into()),
            ..StoredLog::default()
        };
        assert_eq!(joined(&log)["content"]["request"]["x"], 1);
        let none = StoredLog {
            record: r#"{"id":"a","content":null}"#.into(),
            ..StoredLog::default()
        };
        assert!(joined(&none)["content"].is_null());
    }

    #[test]
    fn tails_are_bounded() {
        let logs = RequestLogs::default();
        let slots: Vec<_> = (0..MAX_TAILS).map(|_| logs.subscribe().unwrap()).collect();
        assert!(logs.subscribe().is_none());
        drop(slots);
        assert!(logs.subscribe().is_some());
    }
}
