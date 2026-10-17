//! The response cache's invalidation epochs, kept in the store so they survive a restart.

use crate::Runtime;

const EPOCHS_KEY: &str = "cache_epochs";

impl Runtime {
    /// Writes the cache epochs to the store; in cluster mode each is raised in the shared table,
    /// which every node reads.
    pub async fn persist_cache_epochs(&self) {
        let exported = self.state.cache.epochs.export();
        if let Some(pg) = self.store.postgres() {
            let epochs: Vec<(String, i64)> = exported
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(k, v)| Some((k.clone(), i64::try_from(v.as_u64()?).ok()?)))
                .collect();
            if let Err(e) = pg.raise_epochs(&epochs).await {
                tracing::warn!(error = %e, "cache epochs could not be stored");
            }
            return;
        }
        let bytes = serde_json::to_vec(&exported).unwrap_or_default();
        if let Err(e) = self.store.put_value(EPOCHS_KEY, bytes).await {
            tracing::warn!(error = %e, "cache epochs could not be stored");
        }
    }

    /// Restores the cache epochs a previous run stored.
    pub async fn restore_cache_epochs(&self) {
        if let Some(pg) = self.store.postgres() {
            for (k, e) in pg.epochs().await.unwrap_or_default() {
                self.state.cache.epochs.raise(&k, u64::try_from(e).unwrap_or(0));
            }
            return;
        }
        let Ok(bytes) = self.store.get_or_init(EPOCHS_KEY, b"{}".to_vec()).await else {
            return;
        };
        if let Ok(v) = serde_json::from_slice(&bytes) {
            self.state.cache.epochs.restore(&v);
        }
    }
}
