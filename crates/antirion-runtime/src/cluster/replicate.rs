//! What nodes read from each other through the store besides configuration and the request log
//! (which every node queries in the store): the ledger lines stored since this node started (into
//! its window) and the cache epochs. Reads overlap the last few seconds, because rows commit in a
//! different order than their store timestamps; what is read twice is recognised by its id.

use std::sync::Arc;
use std::time::Duration;

use antirion_limits::ledger::LedgerLine;
use serde_json::Value;

use super::pause;
use crate::Runtime;

const EVERY: Duration = Duration::from_millis(250);
const OVERLAP_US: i64 = 5_000_000;
const READ_LIMIT: i64 = 5000;

/// A position in a store table, by store time.
struct Cursor {
    at: i64,
    /// The last read was cut by the limit: continue exactly where it stopped.
    cut: bool,
}

impl Cursor {
    fn after(&self) -> i64 {
        if self.cut { self.at } else { self.at - OVERLAP_US }
    }

    fn advance(&mut self, newest: i64, read: usize) {
        self.at = self.at.max(newest);
        self.cut = i64::try_from(read).unwrap_or(i64::MAX) >= READ_LIMIT;
    }
}

pub(crate) async fn run(rt: Arc<Runtime>) {
    if rt.cluster.is_none() {
        return;
    }
    let Some(pg) = rt.store.postgres() else { return };
    let start = loop {
        match pg.now_us().await {
            Ok(t) => break t,
            Err(_) if pause(&rt, EVERY).await => {}
            Err(_) => return,
        }
    };
    let mut ledger = Cursor {
        at: start,
        cut: false,
    };
    let mut tick = 0u64;
    while pause(&rt, EVERY).await {
        tick += 1;
        if pg.is_down() {
            continue;
        }
        if let Ok((rows, newest)) = pg.ledger_since(ledger.after(), READ_LIMIT).await {
            ledger.advance(newest, rows.len());
            let lines: Vec<LedgerLine> = rows
                .iter()
                .filter_map(|r| serde_json::from_str::<Value>(&r.body).ok())
                .filter_map(|v| LedgerLine::from_json(&v))
                .collect();
            rt.record_stored(lines);
        }
        if tick.is_multiple_of(4)
            && let Ok(epochs) = pg.epochs().await
        {
            let now_ms = rt.now().0 / 1000;
            for (k, e) in epochs {
                rt.state
                    .cache
                    .raise_from_store(&k, u64::try_from(e).unwrap_or(0), now_ms)
                    .await;
            }
        }
    }
}
