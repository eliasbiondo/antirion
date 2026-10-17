//! The configuration follower: a node applies every generation another node commits, woken by the
//! store's notification or, when one is lost, by the next poll. Generations only move forward.

use std::collections::HashSet;
use std::sync::Arc;

use antirion_core::ResourceUid;
use antirion_store::StoreError;
use antirion_store::pg::Listener;

use crate::Runtime;
use crate::snapshot;
use crate::state::{ConfigState, Metadata};

/// Why a stored generation could not be applied on this node.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("the stored generation cannot be read: {0}")]
    Unreadable(String),
    #[error("the stored generation does not build on this node: {0}")]
    Invalid(String),
}

impl Runtime {
    /// Applies the store's generation when it is newer than the one served. The caller holds the
    /// write lock, so no local commit interleaves. True when a generation was applied.
    pub(crate) async fn catch_up_locked(&self) -> Result<bool, SyncError> {
        let checked_at = self.now();
        let applied = self.apply_stored().await?;
        if let Some(c) = &self.cluster {
            c.confirm(checked_at);
            if applied {
                self.persist_local(&self.gens.load(), checked_at).await;
            }
        }
        Ok(applied)
    }

    async fn apply_stored(&self) -> Result<bool, SyncError> {
        let Some(pg) = self.store.postgres() else {
            return Ok(false);
        };
        let current = self.gens.load().generation;
        if pg.head().await? <= current {
            return Ok(false);
        }
        let Some(stored) = self.store.load_config().await? else {
            return Ok(false);
        };
        if stored.generation <= current {
            return Ok(false);
        }
        let mut doc: serde_json::Value =
            serde_json::from_str(&stored.document).map_err(|e| SyncError::Unreadable(e.to_string()))?;
        self.sealer
            .open_document(&mut doc)
            .map_err(|e| SyncError::Unreadable(e.to_string()))?;
        let meta: Metadata =
            serde_json::from_str(&stored.metadata).map_err(|e| SyncError::Unreadable(e.to_string()))?;
        let state = ConfigState {
            generation: stored.generation,
            doc,
            meta,
        };
        Box::pin(self.prefetch_secrets(&state.doc)).await;
        let secret_paths = antirion_config::schema::check_document(&state.doc).secret_paths;
        let secrets = Arc::clone(&self.secrets);
        let pepper = self.pepper.clone();
        let built = tokio::task::spawn_blocking(move || {
            snapshot::build(state, secret_paths, secrets.as_ref(), &pepper)
        })
        .await
        .map_err(|e| SyncError::Invalid(e.to_string()))?
        .map_err(|errors| {
            SyncError::Invalid(
                errors
                    .iter()
                    .map(|e| format!("{}: {}", e.path, e.code.as_str()))
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        })?;
        let built = Arc::new(built);
        let previous = self.gens.load();
        self.gens.swap(Arc::clone(&built));
        self.health_follows(&previous, &built);
        self.observe_generation(&built);
        let live: HashSet<ResourceUid> = built.config.live_uids().collect();
        self.state.key_usage.retain(&live);
        self.state
            .verified
            .retain(|kid| built.identity.keys_by_kid.contains_key(kid));
        if let Some(c) = &self.cluster {
            c.report_now();
        }
        tracing::debug!(
            generation = built.generation,
            "applied a generation committed elsewhere"
        );
        Ok(true)
    }

    /// Brings this node to the store's generation unless a check began after `asked`: requests
    /// that wait together share one store read.
    pub async fn catch_up_after(&self, asked: std::time::Instant) {
        let Some(cluster) = &self.cluster else { return };
        let _guard = self.write_lock.lock().await;
        {
            let mut last = cluster.last_check.lock();
            if last.is_some_and(|t| t >= asked) {
                return;
            }
            *last = Some(std::time::Instant::now());
        }
        if let Err(e) = self.catch_up_locked().await {
            tracing::debug!(error = %e, "the stored generation could not be read");
        }
    }

    /// Brings this node to the store's generation (before a local commit, and from the follower).
    pub(crate) async fn catch_up(&self) -> Result<bool, SyncError> {
        let _guard = self.write_lock.lock().await;
        self.catch_up_locked().await
    }
}

/// Follows the store's generations until the node stops.
pub(crate) async fn run(rt: Arc<Runtime>) {
    let Some(cluster) = &rt.cluster else { return };
    let poll = cluster.settings.poll_interval;
    let mut listener: Option<Listener> = None;
    let mut failing = false;
    loop {
        if listener.is_none()
            && let Some(pg) = rt.store.postgres()
            && !pg.is_down()
        {
            listener = pg.listen().await.ok();
        }
        let woken = async {
            match listener.as_mut() {
                Some(l) => l.generations.changed().await.is_ok(),
                None => std::future::pending().await,
            }
        };
        let lost = tokio::select! {
            () = rt.shutdown.cancelled() => return,
            alive = woken => !alive,
            () = tokio::time::sleep(poll) => false,
        };
        if lost || rt.store.is_down() {
            listener = None;
            continue;
        }
        match rt.catch_up().await {
            Ok(_) => failing = false,
            Err(e) => {
                if !failing {
                    tracing::warn!(error = %e, "config_apply_failed: the stored generation could not be applied");
                }
                failing = true;
            }
        }
    }
}
