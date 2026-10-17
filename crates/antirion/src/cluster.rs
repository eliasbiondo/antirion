//! Starting a node in cluster mode: the shared PostgreSQL store, the encryption keys every node
//! is configured with, the Valkey counter store and the `cluster.*` timings.

use std::path::Path;
use std::time::Duration;

use antirion_config::scalar::parse_duration;
use antirion_config::views::StaticView;
use antirion_limits::store::valkey::ValkeyCounters;
use antirion_runtime::atrest::Sealer;
use antirion_runtime::cluster::{ClusterSettings, Loaded, SnapshotFile};
use antirion_runtime::state::ConfigState;
use antirion_store::pg::{Migrate, PgSettings};
use antirion_store::{Store, StoreError};
use anyhow::{Context, anyhow, bail};
use serde_json::Value;

use crate::startup::{
    VERSION, configured_keys, persisted_state, random_bytes, resolved_string, sealed_value,
};

/// The exit status of a node whose binary is older than the store's schema allows.
pub(crate) const EXIT_SCHEMA_TOO_NEW: u8 = 78;

fn duration(v: Option<&String>, default: Duration) -> Duration {
    v.and_then(|s| parse_duration(s)).unwrap_or(default)
}

/// The shortest bootstrap token a cluster accepts; every well-known weak value is shorter.
const MIN_BOOTSTRAP_TOKEN: usize = 32;

/// The startup document's cluster settings must name the shared stores, and a strong bootstrap
/// token: a cluster has no data directory to generate one into.
pub(crate) fn check(doc: &Value, v: &StaticView) -> anyhow::Result<()> {
    let token = resolved_string(doc, "/auth/admin/bootstrap_token")?
        .ok_or_else(|| anyhow!("cluster mode needs auth.admin.bootstrap_token"))?;
    if token.chars().count() < MIN_BOOTSTRAP_TOKEN {
        bail!("auth.admin.bootstrap_token must be at least {MIN_BOOTSTRAP_TOKEN} characters in cluster mode");
    }
    if v.store.backend.as_deref() != Some("postgres") {
        bail!("cluster mode needs store.backend: postgres");
    }
    if v.cluster.counters.backend.as_deref() != Some("redis") {
        bail!("cluster mode needs cluster.counters.backend: redis");
    }
    for r in &v.server.probes.readiness_requires {
        if r != "store" && r != "counters" {
            bail!("server.probes.readiness_requires: {r} is not a dependency of this mode");
        }
    }
    Ok(())
}

pub(crate) fn settings(v: &StaticView) -> ClusterSettings {
    let d = ClusterSettings::default();
    let c = &v.cluster;
    ClusterSettings {
        heartbeat_interval: duration(c.heartbeat_interval.as_ref(), d.heartbeat_interval),
        node_ttl: duration(c.node_ttl.as_ref(), d.node_ttl),
        probe_interval: duration(c.probe_interval.as_ref(), d.probe_interval),
        poll_interval: duration(c.config_sync.poll_interval.as_ref(), d.poll_interval),
        snapshot: None,
    }
}

fn pg_settings(doc: &Value, v: &StaticView, node_id: &str) -> anyhow::Result<PgSettings> {
    let p = &v.store.postgres;
    let url = resolved_string(doc, "/store/postgres/url")?
        .ok_or_else(|| anyhow!("cluster mode needs store.postgres.url"))?;
    Ok(PgSettings {
        url,
        pool_size: p.pool_size.unwrap_or(16),
        connect_timeout: duration(p.connect_timeout.as_ref(), Duration::from_secs(5)),
        statement_timeout: duration(p.statement_timeout.as_ref(), Duration::from_secs(5)),
        migrate: if p.migrate.as_deref() == Some("manual") {
            Migrate::Manual
        } else {
            Migrate::Auto
        },
        migrate_timeout: duration(p.migrate_timeout.as_ref(), Duration::from_secs(300)),
        binary_version: VERSION.to_owned(),
        node_id: node_id.to_owned(),
    })
}

/// The first wait between attempts to reach the store at startup, and the longest.
pub(crate) const RETRY_FIRST: Duration = Duration::from_secs(1);
pub(crate) const RETRY_MAX: Duration = Duration::from_secs(30);

/// The next wait of the startup backoff: doubled, up to [`RETRY_MAX`].
pub(crate) fn backoff(wait: Duration) -> Duration {
    (wait * 2).min(RETRY_MAX)
}

/// Whether a startup failure comes from a store that cannot be reached (or did not answer in
/// time), rather than from what it holds.
pub(crate) fn is_outage(e: &anyhow::Error) -> bool {
    e.chain()
        .any(|c| c.downcast_ref::<StoreError>().is_some_and(StoreError::is_outage))
}

/// What a cluster node starts from.
pub(crate) enum Opened {
    /// The store answered: the stored configuration and, when the startup file configures
    /// none, the generated key pepper.
    Store {
        store: Store,
        state: ConfigState,
        pepper: Option<Vec<u8>>,
    },
    /// The store could not be reached: the snapshot this node persisted, with the store down.
    Snapshot { store: Store, local: Loaded },
}

/// Opens the shared store, migrating it as `store.postgres.migrate` says, and reads what the
/// first generation is built from. When the store cannot be reached or does not answer, the node
/// starts from the snapshot it persisted in `data_dir`; without a usable one it keeps trying,
/// waiting 1 s doubling to 30 s.
pub(crate) async fn open(
    doc: &Value,
    v: &StaticView,
    node_id: &str,
    data_dir: &Path,
    sealer: &Sealer,
    read_pepper: bool,
) -> anyhow::Result<Opened> {
    let settings = pg_settings(doc, v, node_id)?;
    let mut wait = RETRY_FIRST;
    loop {
        let failure = match Store::open_postgres(&settings).await {
            Ok(store) => match first_reads(&store, sealer, read_pepper).await {
                Ok((state, pepper)) => return Ok(Opened::Store { store, state, pepper }),
                Err(e) if is_outage(&e) => {
                    store.close();
                    e
                }
                Err(e) => return Err(e),
            },
            Err(e) if e.is_outage() => anyhow::Error::from(e),
            Err(e) => return Err(anyhow::Error::from(e).context("cannot open the cluster store")),
        };
        let local = SnapshotFile::read(data_dir, sealer).context("cannot read the local snapshot")?;
        if let Some(local) = local.filter(|l| !read_pepper || l.pepper.is_some()) {
            tracing::warn!(
                error = %failure,
                generation = local.state.generation,
                persisted_at = %local.persisted_at.to_rfc3339(),
                "the cluster store is unreachable; serving the local snapshot until it returns"
            );
            return Ok(Opened::Snapshot {
                store: Store::postgres_offline(&settings)?,
                local,
            });
        }
        tracing::warn!(
            error = %failure,
            retry_in_ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
            "the cluster store is unreachable and there is no local snapshot; not ready"
        );
        tokio::time::sleep(wait).await;
        wait = backoff(wait);
    }
}

async fn first_reads(
    store: &Store,
    sealer: &Sealer,
    read_pepper: bool,
) -> anyhow::Result<(ConfigState, Option<Vec<u8>>)> {
    let pepper = if read_pepper {
        Some(sealed_value(store, sealer, "key_pepper", random_bytes(32)?).await?)
    } else {
        None
    };
    Ok((persisted_state(store, sealer).await?, pepper))
}

/// The keys that encrypt stored secrets: `security.secrets.encryption_keys` of the startup file.
/// Every node must hold them, so the shared store never does: an empty list is refused.
pub(crate) fn sealer(doc: &Value) -> anyhow::Result<Sealer> {
    let keys = configured_keys(doc)?;
    if keys.is_empty() {
        bail!(
            "cluster mode needs security.secrets.encryption_keys: every node seals and opens secrets with them"
        );
    }
    let configured = keys.len();
    Ok(Sealer::new(&keys)?.with_configured(configured))
}

/// The counter store and its command timeout.
pub(crate) async fn counters(doc: &Value, v: &StaticView) -> anyhow::Result<(ValkeyCounters, Duration)> {
    let r = &v.cluster.counters.redis;
    let url = resolved_string(doc, "/cluster/counters/redis/url")?
        .ok_or_else(|| anyhow!("cluster mode needs cluster.counters.redis.url"))?;
    let prefix = r.key_prefix.clone().unwrap_or_else(|| "antirion:".to_owned());
    let keep_us = u64::try_from(antirion_limits::spend::LEASE_TTL.as_micros()).unwrap_or(u64::MAX);
    let topology = antirion_redis::Topology::parse(r.topology.as_deref().unwrap_or("standalone"));
    let counters = ValkeyCounters::new(&url, &prefix, keep_us, topology)
        .map_err(|_| anyhow!("cluster.counters.redis.url is not a usable Redis URL"))?;
    match tokio::time::timeout(Duration::from_secs(2), counters.load_library()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, "the counter store's functions could not be loaded yet"),
        Err(_) => tracing::warn!("the counter store did not answer at startup"),
    }
    let timeout = duration(r.command_timeout.as_ref(), Duration::from_millis(50));
    Ok((counters, timeout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_startup_backoff_doubles_from_one_second_to_thirty() {
        let mut wait = RETRY_FIRST;
        let mut seen = Vec::new();
        for _ in 0..7 {
            seen.push(wait.as_secs());
            wait = backoff(wait);
        }
        assert_eq!(seen, [1, 2, 4, 8, 16, 30, 30]);
    }

    #[test]
    fn only_an_unreachable_store_is_an_outage() {
        let timeout = anyhow::Error::from(StoreError::Timeout).context("cannot read the pepper");
        assert!(is_outage(&timeout));
        assert!(is_outage(&anyhow::Error::from(StoreError::Unavailable)));
        let corrupt = serde_json::from_str::<Value>("{").unwrap_err();
        assert!(!is_outage(&anyhow::Error::from(StoreError::Corrupt(corrupt))));
        assert!(!is_outage(&anyhow!("the stored configuration is corrupt")));
    }
}
