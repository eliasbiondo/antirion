//! Session-affinity bindings shared through the counter store: a binding this node writes is also
//! written there, and every lookup reads it from there (another node may have moved it), so every
//! node routes a session alike. While the counter store cannot be read, sessions are routed
//! without affinity.

use std::time::Duration;

use antirion_core::UtcMicros;
use antirion_router::affinity::Binding;
use sha2::{Digest, Sha256};

use crate::Runtime;

/// How long routing waits for the shared binding before it routes without it.
const READ_TIMEOUT: Duration = Duration::from_millis(100);

const SEP: char = '\u{1f}';

/// What affinity routing may use for one request.
#[derive(Debug)]
pub enum Lookup {
    Bound(Binding),
    Unbound,
    /// The shared bindings cannot be read: route without affinity, and say so.
    Unavailable,
}

fn digest(key: &str) -> String {
    Sha256::digest(key.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl Runtime {
    fn shared_bindings(&self) -> Option<&antirion_limits::store::valkey::ValkeyCounters> {
        self.cluster.as_ref()?;
        self.accounts.engine().store().valkey()
    }

    /// The binding of `key`: the shared one in cluster mode, else this node's.
    pub async fn affinity_lookup(&self, key: &str, now: UtcMicros, max_entries: usize) -> Lookup {
        let Some(valkey) = self.shared_bindings() else {
            return self
                .state
                .affinity
                .get(key, now)
                .map_or(Lookup::Unbound, Lookup::Bound);
        };
        if self.accounts.engine().is_store_down() {
            return Lookup::Unavailable;
        }
        let read = tokio::time::timeout(READ_TIMEOUT, valkey.binding(&digest(key))).await;
        let Ok(Ok(found)) = read else {
            return Lookup::Unavailable;
        };
        let parsed = found.as_deref().and_then(|v| {
            let mut parts = v.split(SEP);
            let route = parts.next()?;
            let path = parts.next()?;
            let deployment = parts.next()?;
            let expires = UtcMicros(parts.next()?.parse().ok()?);
            Some((route.to_owned(), path.to_owned(), deployment.to_owned(), expires))
        });
        match parsed {
            Some((route, path, deployment, expires)) if expires > now => {
                let ttl = Duration::from_micros(u64::try_from(expires.0 - now.0).unwrap_or(0));
                self.state.affinity.put(
                    key.to_owned(),
                    (&route, &path, &deployment),
                    ttl,
                    now,
                    max_entries,
                );
                self.state
                    .affinity
                    .get(key, now)
                    .map_or(Lookup::Unbound, Lookup::Bound)
            }
            _ => {
                self.state.affinity.forget(key);
                Lookup::Unbound
            }
        }
    }

    /// Deletes every session-affinity binding of `route`: this node's and, in cluster mode, the
    /// shared ones. Returns how many live bindings went: the shared count in cluster mode (other
    /// nodes keep no binding of their own that a lookup would use), else this node's.
    pub async fn affinity_unbind_route(
        &self,
        route: &str,
    ) -> Result<usize, antirion_limits::store::StoreError> {
        let local = self.state.affinity.forget_route(route, self.now());
        let Some(valkey) = self.shared_bindings() else {
            return Ok(local);
        };
        valkey.unbind_where(|v| v.split(SEP).next() == Some(route)).await
    }

    /// Binds `key` to `position` (route, route path, deployment) on this node and, in cluster
    /// mode, for the others.
    pub fn affinity_bind(
        &self,
        key: &str,
        position: (&str, &str, &str),
        ttl: Duration,
        now: UtcMicros,
        max_entries: usize,
    ) {
        self.state
            .affinity
            .put(key.to_owned(), position, ttl, now, max_entries);
        let Some(valkey) = self.shared_bindings().cloned() else {
            return;
        };
        if self.accounts.engine().is_store_down() {
            return;
        }
        let (route, path, deployment) = position;
        let expires = now.saturating_add(ttl);
        let value = format!("{route}{SEP}{path}{SEP}{deployment}{SEP}{}", expires.0);
        let ttl_ms = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX);
        let digest = digest(key);
        self.tasks.spawn(async move {
            if let Err(e) = valkey.bind(&digest, &value, ttl_ms).await {
                tracing::debug!(error = %e, "an affinity binding could not be shared");
            }
        });
    }
}
