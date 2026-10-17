//! Settled ledger lines on their way to the store. They are batched every
//! `store.ledger.flush_interval`; while the store cannot take them they go to an on-disk spool
//! (one length-prefixed, CRC-checked record per line, synced per batch) bounded by
//! `store.spool.max_size`, which is replayed oldest first once the store answers again. Replays are
//! harmless: the store keeps the first copy of each `(request id, line)`. Lines carry the ledger
//! window's sequence numbers, so the window learns which of its lines the store holds.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use antirion_store::{LedgerRow, Store, StoreError};
use parking_lot::Mutex;
use serde_json::{Value, json};

const FILE: &str = "ledger.spool";
const REPLAY_CHUNK: usize = 1000;

/// `store.ledger` and `store.spool`.
#[derive(Clone, Debug)]
pub struct SpoolSettings {
    pub dir: PathBuf,
    pub max_bytes: u64,
    /// `on_full: reject_budgeted`: budgeted requests are refused while lines cannot be recorded.
    pub reject_budgeted: bool,
    pub flush_interval: Duration,
}

/// What the detailed health reports about the spool.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct SpoolState {
    pub bytes: u64,
    pub max_bytes: u64,
    pub records: u64,
    pub full: bool,
    pub dropped: u64,
}

/// The ledger writer of a node.
#[derive(Debug)]
pub struct LedgerWriter {
    settings: SpoolSettings,
    /// The lines to write, and the window sequence number below which every line was pushed.
    pending: Mutex<(Vec<LedgerRow>, u64)>,
    /// The sequence number below which every line written to the spool was pushed.
    spooled_below: AtomicU64,
    bytes: AtomicU64,
    records: AtomicU64,
    full: AtomicBool,
    dropped: AtomicU64,
    flushing: tokio::sync::Mutex<()>,
}

fn encode(row: &LedgerRow) -> Vec<u8> {
    let body = json!({
        "request_id": row.request_id,
        "line": row.line,
        "admitted_us": row.admitted_us,
        "body": row.body,
    })
    .to_string()
    .into_bytes();
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(&u32::try_from(body.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(&crc32fast::hash(&body).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// The records of a spool file, up to the first torn or corrupt one.
fn decode(bytes: &[u8]) -> Vec<LedgerRow> {
    let mut out = Vec::new();
    let mut rest = bytes;
    while let (Some(len), Some(crc)) = (rest.get(..4), rest.get(4..8)) {
        let len = u32::from_le_bytes(len.try_into().unwrap_or_default()) as usize;
        let crc = u32::from_le_bytes(crc.try_into().unwrap_or_default());
        let Some(body) = rest.get(8..8 + len) else { break };
        if crc32fast::hash(body) != crc {
            break;
        }
        let Ok(v) = serde_json::from_slice::<Value>(body) else {
            break;
        };
        let row = LedgerRow {
            request_id: v
                .get("request_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            line: v
                .get("line")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(0),
            admitted_us: v.get("admitted_us").and_then(Value::as_i64).unwrap_or(0),
            body: v
                .get("body")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            charges: None,
        };
        out.push(row);
        rest = rest.get(8 + len..).unwrap_or_default();
    }
    out
}

#[cfg(unix)]
fn open_append(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_append(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new().append(true).create(true).open(path)
}

fn append_blocking(dir: &Path, chunks: &[Vec<u8>]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut f = open_append(&dir.join(FILE))?;
    for c in chunks {
        f.write_all(c)?;
    }
    f.sync_data()
}

fn read_blocking(dir: &Path) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    match std::fs::File::open(dir.join(FILE)) {
        Ok(mut f) => {
            f.read_to_end(&mut buf)?;
            Ok(buf)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(buf),
        Err(e) => Err(e),
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> std::io::Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|e| Err(std::io::Error::other(e.to_string())))
}

impl LedgerWriter {
    /// A writer whose spool may already hold lines from an earlier run.
    #[must_use]
    pub fn new(settings: SpoolSettings) -> LedgerWriter {
        let (bytes, records) = std::fs::read(settings.dir.join(FILE))
            .map(|b| (b.len() as u64, decode(&b).len() as u64))
            .unwrap_or((0, 0));
        LedgerWriter {
            settings,
            pending: Mutex::new((Vec::new(), 0)),
            spooled_below: AtomicU64::new(0),
            bytes: AtomicU64::new(bytes),
            records: AtomicU64::new(records),
            full: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
            flushing: tokio::sync::Mutex::new(()),
        }
    }

    #[must_use]
    pub fn flush_interval(&self) -> Duration {
        self.settings.flush_interval
    }

    /// Queues lines for the next flush; every line with a window sequence number below `below`
    /// has been pushed.
    pub fn push(&self, rows: Vec<LedgerRow>, below: u64) {
        let mut p = self.pending.lock();
        p.0.extend(rows);
        p.1 = p.1.max(below);
    }

    /// Holds off flushes while the caller reads the store and the window's unwritten lines as one.
    pub async fn hold(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.flushing.lock().await
    }

    /// Lines are in the spool: the store is degraded until they are replayed.
    #[must_use]
    pub fn spooling(&self) -> bool {
        self.records.load(Ordering::Relaxed) > 0
    }

    /// Budgeted requests are refused: their lines could be recorded neither in the store nor in
    /// the spool.
    #[must_use]
    pub fn rejects_budgeted(&self) -> bool {
        self.settings.reject_budgeted && self.full.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn state(&self) -> SpoolState {
        SpoolState {
            bytes: self.bytes.load(Ordering::Relaxed),
            max_bytes: self.settings.max_bytes,
            records: self.records.load(Ordering::Relaxed),
            full: self.full.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
        }
    }

    /// Forgets pending lines and the spool (test-mode reset).
    pub async fn clear(&self) {
        let _flushing = self.flushing.lock().await;
        self.pending.lock().0.clear();
        let dir = self.settings.dir.clone();
        let _ = blocking(move || match std::fs::remove_file(dir.join(FILE)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        })
        .await;
        self.reset_counts();
    }

    fn reset_counts(&self) {
        self.bytes.store(0, Ordering::Relaxed);
        self.records.store(0, Ordering::Relaxed);
        self.full.store(false, Ordering::Relaxed);
    }

    /// Writes the pending lines: to the store when it answers (after replaying the spool), to the
    /// spool otherwise. Once the store holds every line pushed so far, `durable` gets the sequence
    /// number below which they all are.
    pub async fn flush(&self, store: &Store, durable: impl FnOnce(u64)) {
        let _flushing = self.flushing.lock().await;
        let (rows, below) = {
            let mut p = self.pending.lock();
            (std::mem::take(&mut p.0), p.1)
        };
        if store.is_down() {
            self.spool(rows, below).await;
            return;
        }
        if self.spooling()
            && let Err(e) = self.replay(store).await
        {
            tracing::warn!(error = %e, "the ledger spool could not be replayed yet");
            self.spool(rows, below).await;
            return;
        }
        if !rows.is_empty()
            && let Err(e) = store.append_ledger(rows.clone()).await
        {
            tracing::warn!(error = %e, "ledger lines go to the spool");
            self.spool(rows, below).await;
            return;
        }
        durable(below.max(self.spooled_below.load(Ordering::Relaxed)));
    }

    async fn replay(&self, store: &Store) -> Result<(), StoreError> {
        let dir = self.settings.dir.clone();
        let bytes = blocking(move || read_blocking(&dir)).await?;
        let rows = decode(&bytes);
        for chunk in rows.chunks(REPLAY_CHUNK) {
            store.append_ledger(chunk.to_vec()).await?;
        }
        let dir = self.settings.dir.clone();
        blocking(move || std::fs::remove_file(dir.join(FILE))).await?;
        self.reset_counts();
        tracing::info!(lines = rows.len(), "the ledger spool was replayed");
        Ok(())
    }

    async fn spool(&self, rows: Vec<LedgerRow>, below: u64) {
        if rows.is_empty() {
            return;
        }
        self.spooled_below.fetch_max(below, Ordering::Relaxed);
        let mut chunks = Vec::with_capacity(rows.len());
        let mut size = 0u64;
        let used = self.bytes.load(Ordering::Relaxed);
        for row in &rows {
            let c = encode(row);
            if used + size + c.len() as u64 > self.settings.max_bytes {
                break;
            }
            size += c.len() as u64;
            chunks.push(c);
        }
        let kept = chunks.len();
        if kept < rows.len() {
            self.full.store(true, Ordering::Relaxed);
            self.dropped
                .fetch_add((rows.len() - kept) as u64, Ordering::Relaxed);
            tracing::error!(
                dropped = rows.len() - kept,
                "the ledger spool is full; lines were dropped"
            );
        }
        if chunks.is_empty() {
            return;
        }
        let dir = self.settings.dir.clone();
        match blocking(move || append_blocking(&dir, &chunks)).await {
            Ok(()) => {
                self.bytes.fetch_add(size, Ordering::Relaxed);
                self.records.fetch_add(kept as u64, Ordering::Relaxed);
            }
            Err(e) => {
                self.full.store(true, Ordering::Relaxed);
                self.dropped.fetch_add(kept as u64, Ordering::Relaxed);
                tracing::error!(error = %e, "the ledger spool cannot be written; lines were dropped");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str) -> LedgerRow {
        LedgerRow {
            request_id: id.into(),
            line: 0,
            admitted_us: 7,
            body: "{\"x\":1}".into(),
            charges: None,
        }
    }

    #[test]
    fn records_round_trip_and_stop_at_a_torn_tail() {
        let mut bytes = encode(&row("a"));
        bytes.extend(encode(&row("b")));
        let whole = decode(&bytes);
        assert_eq!(whole, vec![row("a"), row("b")]);
        bytes.truncate(bytes.len() - 3);
        assert_eq!(decode(&bytes), vec![row("a")]);
        let mut corrupt = encode(&row("c"));
        if let Some(b) = corrupt.last_mut() {
            *b ^= 1;
        }
        assert!(decode(&corrupt).is_empty());
    }

    #[tokio::test]
    async fn a_full_spool_refuses_budgeted_requests_until_it_is_replayed() {
        let dir = std::env::temp_dir().join(format!(
            "antirion-spool-{:x}",
            antirion_core::ResourceUid::random().0
        ));
        let w = LedgerWriter::new(SpoolSettings {
            dir: dir.clone(),
            max_bytes: encode(&row("a")).len() as u64 * 2,
            reject_budgeted: true,
            flush_interval: Duration::from_secs(1),
        });
        w.spool(vec![row("a"), row("b"), row("c")], 3).await;
        assert!(w.spooling() && w.rejects_budgeted());
        assert_eq!(w.state().records, 2);
        assert_eq!(w.state().dropped, 1);
        let reopened = LedgerWriter::new(w.settings.clone());
        assert_eq!(reopened.state().records, 2);
        let store = Store::open_embedded(&dir.join("store")).unwrap();
        w.push(vec![row("d")], 4);
        let mut durable = None;
        w.flush(&store, |b| durable = Some(b)).await;
        assert_eq!(durable, Some(4));
        assert!(!w.spooling() && !w.rejects_budgeted());
        assert_eq!(store.ledger().await.unwrap().len(), 3);
        store.close();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
