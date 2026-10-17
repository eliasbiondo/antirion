//! A fixed set of PostgreSQL connections. Each slot holds one connection, made on first use and
//! again after it closes; an operation holds its slot exclusively, so transactions are possible.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::{Mutex, MutexGuard};
use tokio::task::{AbortHandle, JoinSet};
use tokio_postgres::{Client, Config};

use super::tls::PgTls;

use crate::StoreError;

/// One open connection and the task that drives its socket.
#[derive(Debug)]
pub(crate) struct Conn {
    pub(crate) client: Client,
    task: AbortHandle,
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A connection held for one operation.
#[derive(Debug)]
pub(crate) struct Lease<'a> {
    guard: MutexGuard<'a, Option<Conn>>,
}

impl Lease<'_> {
    pub(crate) fn client(&mut self) -> Result<&mut Client, StoreError> {
        self.guard
            .as_mut()
            .map(|c| &mut c.client)
            .ok_or(StoreError::Closed)
    }

    /// Drops the connection, so the slot reconnects on its next use.
    pub(crate) fn discard(&mut self) {
        self.guard.take();
    }
}

#[derive(Debug)]
pub(crate) struct Pool {
    config: Config,
    tls: PgTls,
    slots: Vec<Mutex<Option<Conn>>>,
    next: AtomicUsize,
    connect_timeout: Duration,
    /// The tasks driving each connection's socket.
    drivers: parking_lot::Mutex<JoinSet<()>>,
}

impl Pool {
    pub(crate) fn new(config: Config, tls: PgTls, size: usize, connect_timeout: Duration) -> Pool {
        Pool {
            config,
            tls,
            slots: (0..size.max(1)).map(|_| Mutex::new(None)).collect(),
            next: AtomicUsize::new(0),
            connect_timeout,
            drivers: parking_lot::Mutex::new(JoinSet::new()),
        }
    }

    /// A new connection outside the pool (migrations and notifications hold their own).
    pub(crate) async fn connect(&self) -> Result<Conn, StoreError> {
        let (client, connection) =
            tokio::time::timeout(self.connect_timeout, self.config.connect(self.tls.clone()))
                .await
                .map_err(|_| StoreError::Timeout)??;
        let mut drivers = self.drivers.lock();
        while drivers.try_join_next().is_some() {}
        let task = drivers.spawn(async move {
            if let Err(e) = connection.await {
                tracing::debug!(error = %e, "a store connection ended");
            }
        });
        Ok(Conn { client, task })
    }

    /// A free slot when there is one, else the next slot in turn once it frees; connected.
    pub(crate) async fn get(&self) -> Result<Lease<'_>, StoreError> {
        let n = self.slots.len();
        let start = self.next.fetch_add(1, Ordering::Relaxed) % n;
        let mut free = None;
        for i in 0..n {
            if let Some(slot) = self.slots.get((start + i) % n)
                && let Ok(g) = slot.try_lock()
            {
                free = Some(g);
                break;
            }
        }
        let mut guard = match free {
            Some(g) => g,
            None => self.slots.get(start).ok_or(StoreError::Closed)?.lock().await,
        };
        if guard.as_ref().is_none_or(|c| c.client.is_closed()) {
            *guard = None;
            *guard = Some(self.connect().await?);
        }
        Ok(Lease { guard })
    }

    /// Closes every idle connection.
    pub(crate) fn close(&self) {
        for slot in &self.slots {
            if let Ok(mut g) = slot.try_lock() {
                g.take();
            }
        }
        self.drivers.lock().abort_all();
    }

    pub(crate) fn config(&self) -> &Config {
        &self.config
    }

    pub(crate) fn tls(&self) -> PgTls {
        self.tls.clone()
    }
}
