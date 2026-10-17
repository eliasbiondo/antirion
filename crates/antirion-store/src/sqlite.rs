//! The embedded store: SQLite databases in WAL mode, each owned by a dedicated thread. Async callers
//! send them closures over a channel, so no SQLite call ever runs on a runtime worker. `state.db`
//! holds configuration, credentials, the ledger and the audit log with full synchronisation;
//! `logs.db` holds the request log and its rollups.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, mpsc};
use std::thread::JoinHandle;

use rusqlite::{Connection, OptionalExtension};
use tokio::sync::oneshot;

use crate::StoreError;

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

/// The SQLite store of a standalone node.
#[derive(Debug)]
pub struct SqliteStore {
    tx: Mutex<Option<mpsc::Sender<Job>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    path: PathBuf,
    /// The request-log database, beside the state database.
    logs: Option<Box<SqliteStore>>,
}

const MIGRATIONS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS config (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        generation INTEGER NOT NULL,
        document TEXT NOT NULL,
        metadata TEXT NOT NULL)",
    "CREATE TABLE IF NOT EXISTS generations (
        generation INTEGER PRIMARY KEY,
        created_at TEXT NOT NULL,
        actor TEXT NOT NULL,
        source TEXT NOT NULL,
        summary TEXT NOT NULL)",
    "CREATE TABLE IF NOT EXISTS audit (
        seq INTEGER PRIMARY KEY AUTOINCREMENT,
        id TEXT NOT NULL UNIQUE,
        time TEXT NOT NULL,
        time_us INTEGER NOT NULL,
        actor_id TEXT,
        action TEXT NOT NULL,
        resource TEXT NOT NULL,
        outcome TEXT NOT NULL,
        entry TEXT NOT NULL)",
    "CREATE INDEX IF NOT EXISTS audit_time ON audit (time_us)",
    "CREATE TABLE IF NOT EXISTS admin_tokens (
        kid TEXT PRIMARY KEY,
        user_name TEXT NOT NULL,
        verifier TEXT NOT NULL,
        description TEXT,
        created_at TEXT NOT NULL,
        expires_at TEXT)",
    "CREATE INDEX IF NOT EXISTS admin_tokens_user ON admin_tokens (user_name)",
];

impl SqliteStore {
    /// Opens (creating when needed) `state.db` and `logs.db` under `dir`, which is created with
    /// mode 0700.
    pub fn open(dir: &Path) -> Result<SqliteStore, StoreError> {
        create_private_dir(dir)?;
        let mut state = Self::start(&dir.join("state.db"), "FULL", "antirion-sqlite", migrate_state)?;
        let logs = Self::start(&dir.join("logs.db"), "NORMAL", "antirion-sqlite-logs", |tx| {
            for m in crate::logs::sqlite_schema() {
                tx.execute_batch(&m)?;
            }
            crate::logs::add_rollup_columns(tx)?;
            Ok(())
        })?;
        state.logs = Some(Box::new(logs));
        Ok(state)
    }

    fn start(
        path: &Path,
        synchronous: &str,
        thread_name: &str,
        migrate: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<(), StoreError>,
    ) -> Result<SqliteStore, StoreError> {
        let mut conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", synchronous)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let tx = conn.transaction()?;
        migrate(&tx)?;
        tx.commit()?;
        let (sender, receiver) = mpsc::channel::<Job>();
        let thread = std::thread::Builder::new()
            .name(thread_name.to_owned())
            .spawn(move || {
                while let Ok(job) = receiver.recv() {
                    job(&mut conn);
                }
            })?;
        Ok(SqliteStore {
            tx: Mutex::new(Some(sender)),
            thread: Mutex::new(Some(thread)),
            path: path.to_owned(),
            logs: None,
        })
    }

    /// The store of the request log.
    pub(crate) fn logs(&self) -> &SqliteStore {
        self.logs.as_deref().unwrap_or(self)
    }

    /// The database file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Runs `f` on the store thread and waits for its result without blocking the runtime.
    pub async fn call<T, F>(&self, f: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, StoreError> + Send + 'static,
    {
        let (done, wait) = oneshot::channel();
        let job: Job = Box::new(move |conn| {
            let _ = done.send(f(conn));
        });
        {
            let guard = self.tx.lock().map_err(|_| StoreError::Closed)?;
            guard
                .as_ref()
                .ok_or(StoreError::Closed)?
                .send(job)
                .map_err(|_| StoreError::Closed)?;
        }
        wait.await.map_err(|_| StoreError::Closed)?
    }

    /// Stops the store threads after the jobs already queued.
    pub fn close(&self) {
        if let Some(logs) = &self.logs {
            logs.close();
        }
        if let Ok(mut guard) = self.tx.lock() {
            guard.take();
        }
        if let Ok(mut guard) = self.thread.lock()
            && let Some(handle) = guard.take()
        {
            let _ = handle.join();
        }
    }
}

impl Drop for SqliteStore {
    fn drop(&mut self) {
        self.close();
    }
}

fn migrate_state(tx: &rusqlite::Transaction<'_>) -> Result<(), StoreError> {
    for m in MIGRATIONS
        .iter()
        .chain(crate::access::MIGRATIONS)
        .chain(crate::ledger::MIGRATIONS)
    {
        tx.execute_batch(m)?;
    }
    let charges: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'ledger_charges'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if charges.is_none() {
        tx.execute_batch(crate::ledger::CHARGES)?;
    }
    for (table, column, decl) in crate::access::ADDED_COLUMNS
        .iter()
        .chain(crate::alerting::ADDED_COLUMNS)
    {
        add_column(tx, table, column, decl)?;
    }
    for m in crate::access::LATER_INDEXES
        .iter()
        .chain(crate::identity::MIGRATIONS)
        .chain(crate::alerting::MIGRATIONS)
        .chain(crate::objects::MIGRATIONS)
        .chain(crate::requests::MIGRATIONS)
        .chain(crate::marks::MIGRATIONS)
        .chain(crate::anomalies::MIGRATIONS)
        .chain(crate::budgets::MIGRATIONS)
        .chain(crate::generations::MIGRATIONS)
        .chain(crate::cache_console::MIGRATIONS)
        .chain(crate::bandits::MIGRATIONS)
        .chain(crate::pressure::MIGRATIONS)
    {
        tx.execute_batch(m)?;
    }
    Ok(())
}

/// Adds a column a later release introduced, once: SQLite has no `ADD COLUMN IF NOT EXISTS`.
fn add_column(c: &Connection, table: &str, column: &str, decl: &str) -> Result<(), StoreError> {
    let exists: Option<i64> = c
        .query_row(
            &format!("SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1"),
            [column],
            |r| r.get(0),
        )
        .optional()?;
    if exists.is_none() {
        c.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
    }
    Ok(())
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    if !dir.exists() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    let mut perms = std::fs::metadata(dir)?.permissions();
    if perms.mode() & 0o077 != 0 {
        perms.set_mode(0o700);
        std::fs::set_permissions(dir, perms)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> Result<(), StoreError> {
    Ok(std::fs::create_dir_all(dir)?)
}
