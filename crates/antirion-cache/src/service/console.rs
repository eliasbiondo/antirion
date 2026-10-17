//! What the console reads and removes: hits counted per entry, listings and single entries,
//! dimension counts, and the removal of the entries an invalidation selects.

use std::sync::Arc;
use std::time::Duration;

use super::{Backend, Cache};
use crate::CacheError;
use crate::entry::Entry;
use crate::invalidate::Target;
use crate::listing::{ListQuery, Listed, Listing};
use crate::meta::dim;

/// The least a console operation on a shared backend may take.
const CONSOLE_TIMEOUT: Duration = Duration::from_secs(5);

impl Cache {
    fn console_bound(&self) -> Duration {
        self.settings.lock().write_timeout.max(CONSOLE_TIMEOUT)
    }

    async fn bounded<T>(&self, f: impl Future<Output = Result<T, CacheError>>) -> Result<T, CacheError> {
        tokio::time::timeout(self.console_bound(), f)
            .await
            .map_err(|_| CacheError::Timeout)?
    }

    /// Counts a hit served from the entry under `key`.
    pub async fn hit(&self, key: &str, now_ms: i64) -> Result<(), CacheError> {
        let (backend, settings) = self.current();
        match &*backend {
            Backend::Memory(m) => {
                m.hit(key, now_ms);
                Ok(())
            }
            Backend::Valkey(v) => tokio::time::timeout(settings.write_timeout, v.touch(key, now_ms))
                .await
                .map_err(|_| CacheError::Timeout)?,
            Backend::Broken => Err(CacheError::Unavailable),
        }
    }

    /// A page of live entries; entries a finer invalidation made dead are left out.
    pub async fn list(&self, q: &ListQuery) -> Result<Listing, CacheError> {
        let (backend, _) = self.current();
        let mut listing = match &*backend {
            Backend::Memory(m) => m.list(q),
            Backend::Valkey(v) => self.bounded(v.list(q)).await?,
            Backend::Broken => return Err(CacheError::Unavailable),
        };
        listing.items.retain(|i| !self.dead_item(i));
        Ok(listing)
    }

    fn dead_item(&self, i: &Listed) -> bool {
        let probe = Entry {
            written_at_ms: i.created_ms,
            ttl_ms: 0,
            content_type: String::new(),
            body: None,
            events: None,
            provider: String::new(),
            deployment: i.deployment.clone(),
            model: String::new(),
            upstream_status: 0,
            upstream_dialect: None,
            usage: None,
            meta: Some(i.meta.clone()),
        };
        self.epochs.dead(&probe)
    }

    /// The live entry under `key` as a listing reports it, with the stored entry when the backend
    /// still has it.
    pub async fn read(
        &self,
        key: &str,
        now_ms: i64,
    ) -> Result<Option<(Listed, Option<Arc<Entry>>)>, CacheError> {
        let (backend, _) = self.current();
        let found = match &*backend {
            Backend::Memory(m) => m.read(key, now_ms).map(|(l, e)| (l, Some(e))),
            Backend::Valkey(v) => self
                .bounded(v.read(key, now_ms))
                .await?
                .map(|(l, e)| (l, e.map(Arc::new))),
            Backend::Broken => return Err(CacheError::Unavailable),
        };
        Ok(found.filter(|(l, _)| !self.dead_item(l)))
    }

    /// How many live entries carry `dim`.
    pub async fn count_dim(&self, dim: &str, now_ms: i64) -> Result<u64, CacheError> {
        let (backend, _) = self.current();
        match &*backend {
            Backend::Memory(m) => Ok(m.count(dim, now_ms)),
            Backend::Valkey(v) => self.bounded(v.count(dim, now_ms)).await,
            Backend::Broken => Err(CacheError::Unavailable),
        }
    }

    /// Removes from the backend the entries `target` selects that were written at or before
    /// `written_le`; how many.
    pub async fn remove(&self, target: &Target, written_le: i64) -> Result<u64, CacheError> {
        let (backend, _) = self.current();
        let n = match (&*backend, target) {
            (Backend::Memory(m), Target::All | Target::OlderThan) => m.drop_older(written_le),
            (Backend::Memory(m), Target::Dim { dim, digest }) => {
                m.drop_dim(dim, digest.as_deref(), written_le)
            }
            (Backend::Valkey(v), Target::All | Target::OlderThan) => {
                self.bounded(v.drop_older(written_le)).await?
            }
            (Backend::Valkey(v), Target::Dim { dim, digest }) => {
                self.bounded(v.drop_dim(dim, digest.as_deref(), written_le))
                    .await?
            }
            (Backend::Broken, _) => return Err(CacheError::Unavailable),
        };
        Ok(n)
    }

    /// Makes the entries `target` selects dead as of `cutoff_ms` on every node (the cutoff is
    /// kept with the epochs) and removes them from this node's backend; how many it removed.
    pub async fn invalidate(&self, target: &Target, cutoff_ms: i64) -> Result<u64, CacheError> {
        if let Some(key) = target.cutoff_key() {
            self.epochs.raise(&key, u64::try_from(cutoff_ms).unwrap_or(0));
        }
        self.remove(target, cutoff_ms).await
    }

    /// The selection an epoch level stands for.
    #[must_use]
    pub fn level_target(level: &str, name: &str) -> Target {
        match level {
            "gateway" => Target::All,
            _ => Target::Dim {
                dim: dim(level, name),
                digest: None,
            },
        }
    }

    /// Raises an epoch or cutoff to what the store holds; when it grew, a node keeping entries in
    /// memory removes the ones it made unreachable.
    pub async fn raise_from_store(&self, key: &str, value: u64, now_ms: i64) {
        if !self.epochs.raise(key, value) || !self.local() {
            return;
        }
        let target = match Target::of_cutoff_key(key) {
            Some(t) => Some((t, i64::try_from(value).unwrap_or(i64::MAX))),
            None => key
                .split_once('/')
                .map(|(level, name)| (Cache::level_target(level, name), now_ms)),
        };
        if let Some((t, le)) = target
            && let Err(e) = self.remove(&t, le).await
        {
            tracing::debug!(error = %e, "invalidated cache entries could not be removed");
        }
    }
}
