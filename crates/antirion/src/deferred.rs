//! Startup work that needs the store and waits for it when it cannot be reached yet: the MCP
//! session key, the state the store keeps for the node, and the startup document.

use std::sync::Arc;
use std::time::Duration;

use antirion_runtime::Runtime;
use antirion_runtime::atrest::Sealer;
use antirion_store::{Store, StoreError};
use anyhow::anyhow;
use serde_json::{Map, Value};

use crate::cluster::{RETRY_FIRST, backoff};
use crate::startup::{apply_document, random_bytes, sealed_value};

/// Reads (or creates) the MCP session key; false when the store cannot be read, and MCP
/// endpoints answer 503 until [`retry_mcp_key`] reads it.
pub(crate) async fn load_mcp_key(rt: &Runtime, store: &Store, sealer: &Sealer) -> bool {
    let init = match random_bytes(32) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "the MCP session key could not be generated");
            return false;
        }
    };
    match sealed_value(store, sealer, "mcp_session_key", init).await {
        Ok(key) => match <[u8; 32]>::try_from(key.as_slice()) {
            Ok(k) => rt.state.mcp.set_generated_key(k),
            Err(_) => tracing::warn!("the stored MCP session key is malformed; MCP endpoints answer 503"),
        },
        Err(e) => {
            tracing::warn!(
                error = %e,
                "the MCP session key could not be read yet; MCP endpoints answer 503 until it is"
            );
            return false;
        }
    }
    true
}

/// Sleeps for `d` unless the node stops first; false when it stops.
async fn pause(rt: &Runtime, d: Duration) -> bool {
    tokio::select! {
        () = rt.shutdown.cancelled() => false,
        () = tokio::time::sleep(d) => true,
    }
}

/// Tries the MCP session key again every `every` until it is read.
pub(crate) async fn retry_mcp_key(rt: Arc<Runtime>, store: Arc<Store>, sealer: Sealer, every: Duration) {
    while pause(&rt, every).await {
        if !store.is_down() && load_mcp_key(&rt, &store, &sealer).await {
            return;
        }
    }
}

/// Why the store work of startup did not finish.
#[derive(Debug)]
pub(crate) enum Unfinished {
    /// The store could not be reached or did not answer; the work is tried again.
    Retry(anyhow::Error),
    /// Trying again cannot help: an invalid startup document, a stored record that cannot be read.
    Fatal(anyhow::Error),
}

/// Restores what the store keeps for this node (manual disables; with `read_ledger`, cache
/// epochs and the spend ledger) and applies the startup document.
async fn restore_and_apply(
    rt: &Runtime,
    startup_doc: &Map<String, Value>,
    read_ledger: bool,
) -> Result<(), Unfinished> {
    rt.restore_disables().await;
    if read_ledger {
        rt.restore_cache_epochs().await;
        if let Err(e) = rt.check_ledger().await {
            let outage = e.is_outage();
            let e = anyhow::Error::from(e).context("the spend ledger could not be read");
            return Err(if outage {
                Unfinished::Retry(e)
            } else {
                Unfinished::Fatal(e)
            });
        }
    }
    if rt.store.is_down() {
        return Err(Unfinished::Retry(anyhow!(StoreError::Unavailable)));
    }
    match apply_document(rt, startup_doc, "startup").await {
        Ok(()) => Ok(()),
        Err(e) if e.retryable() => Err(Unfinished::Retry(anyhow!(e))),
        Err(e) => Err(Unfinished::Fatal(anyhow!(e))),
    }
}

/// Probes a cluster store that failed until it is in use again.
async fn store_answers(store: &Store, every: Duration) {
    let Some(pg) = store.postgres() else { return };
    while pg.is_down() && !pg.probe().await {
        tokio::time::sleep(every).await;
    }
}

/// The store work of a cluster node that did not start from its snapshot, done before it is
/// ready: restoring its state from the store and applying the startup document. While the store
/// cannot be reached the node stays not ready and tries again, waiting 1 s doubling to 30 s.
pub(crate) async fn first_contact(
    rt: &Runtime,
    startup_doc: &Map<String, Value>,
    probe_every: Duration,
) -> anyhow::Result<()> {
    let mut wait = RETRY_FIRST;
    loop {
        store_answers(&rt.store, probe_every).await;
        match restore_and_apply(rt, startup_doc, true).await {
            Ok(()) => {
                rt.confirm_from_store().await;
                return Ok(());
            }
            Err(Unfinished::Fatal(e)) => return Err(e),
            Err(Unfinished::Retry(e)) => tracing::warn!(
                error = %e,
                retry_in_ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
                "the startup document cannot be applied until the store answers; not ready"
            ),
        }
        tokio::time::sleep(wait).await;
        wait = backoff(wait);
    }
}

/// A node that started from its local snapshot applies the startup document, and restores what
/// the store keeps for it, once the store answers; it tries again while the store fails.
pub(crate) async fn apply_when_store_returns(
    rt: Arc<Runtime>,
    startup_doc: Map<String, Value>,
    every: Duration,
) {
    let mut wait = RETRY_FIRST;
    loop {
        while rt.store.is_down() {
            if !pause(&rt, every).await {
                return;
            }
        }
        match restore_and_apply(&rt, &startup_doc, false).await {
            Ok(()) => return,
            Err(Unfinished::Fatal(e)) => {
                tracing::error!("the startup document could not be applied: {e:#}");
                return;
            }
            Err(Unfinished::Retry(e)) => tracing::warn!(
                error = %e,
                retry_in_ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
                "the startup document cannot be applied until the store answers"
            ),
        }
        if !pause(&rt, wait).await {
            return;
        }
        wait = backoff(wait);
    }
}
