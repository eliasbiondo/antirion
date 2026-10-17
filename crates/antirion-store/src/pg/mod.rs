//! The cluster store on PostgreSQL. Every node of a cluster reads and writes the same database;
//! configuration changes are announced with `NOTIFY` on [`CONFIG_CHANNEL`].
//!
//! The store tracks its own health: a failure that is not an answer from the server (a refused or
//! dropped connection, a timeout) marks it down, after which calls fail at once until three probes
//! in a row succeed.

mod cluster;
mod erasure;
mod listen;
mod logs;
pub(crate) mod ops;
mod pool;
mod safety;
mod schema;
mod tls;

use std::future::Future;
use std::time::Duration;

use parking_lot::Mutex;

pub use cluster::{JobLease, NodeLoad, NodeRow, SharedResponse};
pub use listen::Listener;

use crate::StoreError;
use pool::Pool;

/// The channel configuration commits are announced on; the payload is the new generation.
pub const CONFIG_CHANNEL: &str = "antirion_config";

/// Consecutive good probes before a store that failed is used again.
pub const PROBES_TO_RECOVER: u8 = 3;

/// Whether migrations run at startup.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Migrate {
    Auto,
    Manual,
}

/// `store.postgres` and what the node needs to open it.
#[derive(Clone)]
pub struct PgSettings {
    /// The connection URL (it may carry a password: never logged).
    pub url: String,
    pub pool_size: usize,
    pub connect_timeout: Duration,
    pub statement_timeout: Duration,
    pub migrate: Migrate,
    pub migrate_timeout: Duration,
    /// This binary's version, checked against the schema's oldest compatible binary.
    pub binary_version: String,
    /// This node, recorded with the rows it writes.
    pub node_id: String,
}

impl std::fmt::Debug for PgSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgSettings")
            .field("pool_size", &self.pool_size)
            .field("migrate", &self.migrate)
            .finish_non_exhaustive()
    }
}

/// The store's health as the node last saw it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Health {
    pub down: bool,
    /// Consecutive good probes while down.
    pub good_probes: u8,
    /// The last failure, without connection details.
    pub last_error: Option<String>,
}

/// The schema check a node makes before it first uses the store.
#[derive(Clone, Debug)]
struct SchemaCheck {
    migrate: Migrate,
    binary_version: String,
    migrate_timeout: Duration,
}

impl SchemaCheck {
    async fn run(&self, pool: &Pool) -> Result<(), StoreError> {
        let mut conn = pool.connect().await?;
        match self.migrate {
            Migrate::Auto => {
                schema::migrate(&mut conn.client, &self.binary_version, self.migrate_timeout).await?;
            }
            Migrate::Manual => schema::verify(&conn.client, &self.binary_version).await?,
        }
        Ok(())
    }
}

/// The PostgreSQL store of a cluster node.
#[derive(Debug)]
pub struct PgStore {
    pool: Pool,
    node: String,
    op_timeout: Duration,
    health: Mutex<Health>,
    /// Set while a node that started without reaching the store has not checked its schema yet.
    unchecked: Mutex<Option<SchemaCheck>>,
}

impl PgStore {
    fn build(settings: &PgSettings) -> Result<(PgStore, SchemaCheck), StoreError> {
        let (mut config, tls) = tls::parse(&settings.url)?;
        let statement_ms = settings.statement_timeout.as_millis().max(1);
        config.options(format!("-c statement_timeout={statement_ms}"));
        config.connect_timeout(settings.connect_timeout);
        config.application_name("antirion");
        let store = PgStore {
            pool: Pool::new(config, tls, settings.pool_size, settings.connect_timeout),
            node: settings.node_id.clone(),
            op_timeout: settings.statement_timeout + settings.connect_timeout,
            health: Mutex::new(Health::default()),
            unchecked: Mutex::new(None),
        };
        let check = SchemaCheck {
            migrate: settings.migrate,
            binary_version: settings.binary_version.clone(),
            migrate_timeout: settings.migrate_timeout,
        };
        Ok((store, check))
    }

    /// Connects, then migrates (or, with `migrate: manual`, verifies) the schema.
    pub async fn open(settings: &PgSettings) -> Result<PgStore, StoreError> {
        let (store, check) = Self::build(settings)?;
        check.run(&store.pool).await?;
        Ok(store)
    }

    /// A store that could not be reached at startup: it starts down, and checks the schema before
    /// the probes that bring it back.
    pub fn offline(settings: &PgSettings) -> Result<PgStore, StoreError> {
        let (store, check) = Self::build(settings)?;
        *store.health.lock() = Health {
            down: true,
            good_probes: 0,
            last_error: Some("not reached since the node started".to_owned()),
        };
        *store.unchecked.lock() = Some(check);
        Ok(store)
    }

    #[must_use]
    pub fn health(&self) -> Health {
        self.health.lock().clone()
    }

    #[must_use]
    pub fn is_down(&self) -> bool {
        self.health.lock().down
    }

    fn mark_down(&self, e: &StoreError) {
        let mut h = self.health.lock();
        if !h.down {
            tracing::warn!(error = %e, "the store is unavailable");
            h.down = true;
        }
        h.good_probes = 0;
        h.last_error = Some(e.to_string());
    }

    /// Runs one operation with the store's deadline, unless the store is down.
    pub(crate) async fn run<T>(
        &self,
        op: impl Future<Output = Result<T, StoreError>>,
    ) -> Result<T, StoreError> {
        if self.is_down() {
            return Err(StoreError::Unavailable);
        }
        self.attempt(op).await
    }

    async fn attempt<T>(&self, op: impl Future<Output = Result<T, StoreError>>) -> Result<T, StoreError> {
        let result = tokio::time::timeout(self.op_timeout, op)
            .await
            .unwrap_or(Err(StoreError::Timeout));
        if let Err(e) = &result
            && e.is_outage()
        {
            self.mark_down(e);
        }
        result
    }

    /// One health probe, also while down; true when the store is in use afterwards.
    pub async fn probe(&self) -> bool {
        let ok = self.attempt(self.ping_inner()).await.is_ok() && self.check_schema().await;
        let mut h = self.health.lock();
        if !h.down {
            return true;
        }
        if !ok {
            h.good_probes = 0;
            return false;
        }
        h.good_probes = h.good_probes.saturating_add(1);
        if h.good_probes >= PROBES_TO_RECOVER {
            h.down = false;
            h.good_probes = 0;
            tracing::info!("the store is available again");
            return true;
        }
        false
    }

    /// Runs the schema check a node owes the store, if any; false while it cannot pass.
    async fn check_schema(&self) -> bool {
        let Some(check) = self.unchecked.lock().clone() else {
            return true;
        };
        match tokio::time::timeout(check.migrate_timeout + self.op_timeout, check.run(&self.pool)).await {
            Ok(Ok(())) => {
                *self.unchecked.lock() = None;
                true
            }
            Ok(Err(e)) => {
                if !e.is_outage() {
                    tracing::error!(error = %e, "the store's schema cannot be used by this node");
                }
                false
            }
            Err(_) => false,
        }
    }

    async fn ping_inner(&self) -> Result<(), StoreError> {
        let mut lease = self.pool.get().await?;
        let r = lease.client()?.simple_query("SELECT 1").await;
        if r.is_err() {
            lease.discard();
        }
        r.map(|_| ()).map_err(StoreError::from)
    }

    pub async fn ping(&self) -> Result<(), StoreError> {
        self.run(self.ping_inner()).await
    }

    /// A connection that receives configuration announcements.
    pub async fn listen(&self) -> Result<Listener, StoreError> {
        self.run(listen::open(&self.pool, CONFIG_CHANNEL)).await
    }

    pub fn close(&self) {
        self.pool.close();
    }
}

#[cfg(test)]
mod tests;
