//! Delivery of request-log records and spans off the request path: one bounded queue and one tracked
//! worker per HTTP sink, one for the span exporter, and the stdout writer. A full queue drops and
//! counts; a slow or failing destination never reaches a request.

pub mod alerts;
pub(crate) mod deliver;
mod exporter;
mod http_sink;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use antirion_egress::{EgressClient, EgressPolicy, TlsProfile};
use antirion_obs::settings::{HttpSinkSettings, TraceSettings};
use antirion_obs::sink::LogRecord;
use antirion_obs::trace::SpanData;
use antirion_obs::{Fam, Metrics};
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::snapshot::Snapshot;

/// The health of one sink, read when a response head is built.
#[derive(Debug, Default)]
pub(crate) struct SinkStatus {
    /// A batch failed delivery and has not been delivered since.
    pub(crate) failing: AtomicBool,
    /// The last record offered found the queue full.
    pub(crate) dropping: AtomicBool,
    pub(crate) alarm: DropAlarm,
    /// A whole batch is queued. The worker filling a batch waits for this or its flush time rather
    /// than for each record, so queuing a record does not wake it.
    pub(crate) batch_ready: tokio::sync::Notify,
}

/// Records a full queue dropped, reported as a `log_sink.dropping` event at most once a minute.
#[derive(Debug, Default)]
pub(crate) struct DropAlarm {
    dropped: std::sync::atomic::AtomicU64,
    last: Mutex<Option<std::time::Instant>>,
}

impl DropAlarm {
    const EVERY: std::time::Duration = std::time::Duration::from_secs(60);

    /// Counts one dropped record; the records dropped since the last report when one is due.
    fn dropped(&self, now: std::time::Instant) -> Option<u64> {
        self.dropped.fetch_add(1, Ordering::Relaxed);
        let mut last = self.last.lock();
        if last.is_some_and(|l| now.saturating_duration_since(l) < Self::EVERY) {
            return None;
        }
        *last = Some(now);
        Some(self.dropped.swap(0, Ordering::Relaxed))
    }
}

#[derive(Debug)]
struct SinkHandle {
    settings: HttpSinkSettings,
    tx: mpsc::Sender<Arc<dyn LogRecord>>,
    status: Arc<SinkStatus>,
    stop: CancellationToken,
}

#[derive(Debug)]
struct ExporterHandle {
    settings: TraceSettings,
    tx: mpsc::Sender<SpanData>,
    stop: CancellationToken,
}

/// What every worker needs from the runtime.
#[derive(Clone, Debug)]
pub(crate) struct Env {
    pub(crate) client: EgressClient,
    pub(crate) policy: Arc<EgressPolicy>,
    pub(crate) tls: Option<TlsProfile>,
    pub(crate) metrics: Metrics,
    pub(crate) version: String,
    pub(crate) clock: Arc<dyn antirion_core::Clock>,
}

/// The node's sinks and span exporter, reconciled with each generation's settings.
#[derive(Debug, Default)]
pub struct Telemetry {
    sinks: Mutex<Vec<SinkHandle>>,
    exporter: Mutex<Option<ExporterHandle>>,
    stdout: Mutex<Option<(mpsc::Sender<Vec<u8>>, CancellationToken)>>,
    stdout_alarm: DropAlarm,
}

impl Telemetry {
    /// Starts, keeps or stops workers so they match `snap`'s settings.
    pub(crate) fn reconcile(
        &self,
        snap: &Snapshot,
        env: &Env,
        tasks: &TaskTracker,
        shutdown: &CancellationToken,
    ) {
        let logs = &snap.obs.request_logs;
        let wanted: &[HttpSinkSettings] = if logs.enabled { &logs.http } else { &[] };
        let flush = logs.shutdown_flush_timeout;
        {
            let mut sinks = self.sinks.lock();
            sinks.retain(|h| {
                let keep = wanted.contains(&h.settings);
                if !keep {
                    h.stop.cancel();
                }
                keep
            });
            for s in wanted {
                if sinks.iter().any(|h| h.settings == *s) {
                    continue;
                }
                let (tx, rx) = mpsc::channel(s.queue_capacity);
                let status = Arc::new(SinkStatus::default());
                let stop = CancellationToken::new();
                tasks.spawn(http_sink::run(
                    s.clone(),
                    rx,
                    Arc::clone(&status),
                    env.clone(),
                    stop.clone(),
                    shutdown.clone(),
                    flush,
                ));
                sinks.push(SinkHandle {
                    settings: s.clone(),
                    tx,
                    status,
                    stop,
                });
            }
        }
        let traces = &snap.obs.traces;
        let mut exporter = self.exporter.lock();
        let keep = traces.enabled
            && traces.endpoint.is_some()
            && exporter.as_ref().is_some_and(|h| h.settings == *traces);
        if !keep {
            if let Some(h) = exporter.take() {
                h.stop.cancel();
            }
            if traces.enabled && traces.endpoint.is_some() {
                let (tx, rx) = mpsc::channel(traces.max_queue_size);
                let stop = CancellationToken::new();
                tasks.spawn(exporter::run(
                    traces.clone(),
                    rx,
                    env.clone(),
                    stop.clone(),
                    shutdown.clone(),
                    flush,
                ));
                *exporter = Some(ExporterHandle {
                    settings: traces.clone(),
                    tx,
                    stop,
                });
            }
        }
        let mut stdout = self.stdout.lock();
        match (logs.enabled && logs.stdout.is_some(), stdout.is_some()) {
            (true, false) => {
                let (tx, rx) = mpsc::channel(10_000);
                let stop = CancellationToken::new();
                tasks.spawn(stdout_writer(rx, env.metrics.clone(), stop.clone()));
                *stdout = Some((tx, stop));
            }
            (false, true) => {
                if let Some((_, stop)) = stdout.take() {
                    stop.cancel();
                }
            }
            _ => {}
        }
    }

    /// Hands a finished record to every HTTP sink and the stdout sink of `snap`. Returns the sinks
    /// due a `log_sink.dropping` report, with the records each dropped since its last one.
    pub fn publish(
        &self,
        snap: &Snapshot,
        record: &Arc<dyn LogRecord>,
        metrics: &Metrics,
    ) -> Vec<(String, u64)> {
        let now = std::time::Instant::now();
        let mut due = Vec::new();
        for h in self.sinks.lock().iter() {
            let batch = h.settings.batch_max_records;
            if !offer(
                &h.settings.name,
                &h.tx,
                Arc::clone(record),
                &h.status,
                metrics,
                batch,
            ) && let Some(n) = h.status.alarm.dropped(now)
            {
                due.push((h.settings.name.clone(), n));
            }
        }
        if let (Some(include), Some((tx, _))) = (snap.obs.request_logs.stdout, self.stdout.lock().as_ref()) {
            let mut line = Vec::new();
            record.write_json(include, &mut line);
            line.push(b'\n');
            if tx.try_send(line).is_err() {
                metrics.add(Fam::LogRecords, &["stdout", "dropped_queue_full"], 1.0);
                if let Some(n) = self.stdout_alarm.dropped(now) {
                    due.push(("stdout".to_owned(), n));
                }
            }
        }
        due
    }

    /// The cause to report under the `observability` degraded component, when a sink is dropping
    /// records or failing delivery.
    #[must_use]
    pub fn degraded(&self) -> Option<&'static str> {
        let sinks = self.sinks.lock();
        if sinks.iter().any(|h| h.status.dropping.load(Ordering::Relaxed)) {
            return Some("queue_full");
        }
        sinks
            .iter()
            .any(|h| h.status.failing.load(Ordering::Relaxed))
            .then_some("delivery_failing")
    }

    /// Queues finished spans for export; spans that find the queue full are dropped and counted.
    pub fn export(&self, spans: Vec<SpanData>, metrics: &Metrics) {
        let exporter = self.exporter.lock();
        let Some(h) = exporter.as_ref() else {
            return;
        };
        for s in spans {
            if h.tx.try_send(s).is_err() {
                metrics.add(Fam::SpansDropped, &["queue_full"], 1.0);
            }
        }
    }
}

/// Queues a record for one sink; `false` when the queue was full and the record dropped.
fn offer(
    name: &str,
    tx: &mpsc::Sender<Arc<dyn LogRecord>>,
    record: Arc<dyn LogRecord>,
    status: &SinkStatus,
    metrics: &Metrics,
    batch: usize,
) -> bool {
    let queued = match tx.try_send(record) {
        Ok(()) => {
            status.dropping.store(false, Ordering::Relaxed);
            true
        }
        Err(TrySendError::Full(_) | TrySendError::Closed(_)) => {
            status.dropping.store(true, Ordering::Relaxed);
            metrics.add(Fam::LogRecords, &[name, "dropped_queue_full"], 1.0);
            false
        }
    };
    let depth = tx.max_capacity().saturating_sub(tx.capacity());
    metrics.set(Fam::LogQueueDepth, &[name], depth as f64);
    if depth >= batch {
        status.batch_ready.notify_one();
    }
    queued
}

async fn stdout_writer(mut rx: mpsc::Receiver<Vec<u8>>, metrics: Metrics, stop: CancellationToken) {
    use tokio::io::AsyncWriteExt;
    let mut out = tokio::io::stdout();
    loop {
        let line = tokio::select! {
            () = stop.cancelled() => return,
            l = rx.recv() => match l {
                Some(l) => l,
                None => return,
            },
        };
        let result = if out.write_all(&line).await.is_ok() && out.flush().await.is_ok() {
            "exported"
        } else {
            "failed"
        };
        metrics.add(Fam::LogRecords, &["stdout", result], 1.0);
    }
}

#[cfg(test)]
mod tests {
    use antirion_obs::BuildInfo;
    use serde_json::json;

    use super::*;

    #[test]
    fn drop_reports_come_at_most_once_a_minute_with_the_count() {
        let alarm = DropAlarm::default();
        let t0 = std::time::Instant::now();
        assert_eq!(alarm.dropped(t0), Some(1));
        assert_eq!(alarm.dropped(t0 + std::time::Duration::from_secs(10)), None);
        assert_eq!(alarm.dropped(t0 + std::time::Duration::from_secs(30)), None);
        assert_eq!(alarm.dropped(t0 + std::time::Duration::from_secs(61)), Some(3));
    }

    fn metrics() -> Metrics {
        Metrics::new(&BuildInfo {
            version: "0".into(),
            commit: "c".into(),
            rustc: "r".into(),
            semconv: "s".into(),
            test_mode: true,
        })
    }

    #[test]
    fn a_full_queue_drops_counts_and_marks_the_sink() {
        let m = metrics();
        let (tx, _rx) = mpsc::channel(1);
        let status = SinkStatus::default();
        let record: Arc<dyn LogRecord> = Arc::new(json!({"id": "a"}));
        assert!(offer("w", &tx, Arc::clone(&record), &status, &m, 10));
        assert!(!status.dropping.load(Ordering::Relaxed));
        assert!(!offer("w", &tx, record, &status, &m, 10));
        assert!(status.dropping.load(Ordering::Relaxed));
        assert_eq!(
            m.sum(
                Fam::LogRecords,
                &[("sink", "w"), ("result", "dropped_queue_full")]
            ),
            1.0
        );
        assert_eq!(m.sum(Fam::LogQueueDepth, &[("sink", "w")]), 1.0);
    }
}
