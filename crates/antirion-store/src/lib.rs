//! Durable state: the config document and its generations, admin tokens, sessions and token
//! bindings, the audit log, the spend ledger and small node values (the key pepper, the node id).
//! Standalone nodes use the embedded SQLite store; cluster nodes share one PostgreSQL database.

#![forbid(unsafe_code)]

mod access;
mod alerting;
mod anomalies;
mod bandits;
mod budgets;
mod cache_console;
pub mod chlogs;
mod deliveries;
mod erasure;
mod generations;
mod idempotency;
mod identity;
mod ledger;
pub mod logs;
mod marks;
mod objects;
mod once;
pub mod pg;
mod pressure;
mod requests;
mod safety;
mod sqlite;

use antirion_core::UtcMicros;
use antirion_core::audit::AuditEntry;
use rusqlite::{OptionalExtension, params};

pub use access::{SessionRecord, TokenBindingRecord};
pub use alerting::{EventRow, InstanceRow};
pub use anomalies::{AnomalyFilter, AnomalyRow, SeenRow};
pub use bandits::{ArmReward, BanditStop};
pub use budgets::{BudgetEventFilter, BudgetEventRow, GrantRow, OverageRow, TallyRow};
pub use cache_console::{RuleFirings, SemanticTally};
pub use deliveries::{DeliveryQuery, DeliveryRow, QueueRow};
pub use erasure::Tenant;
pub use identity::IdentityRecord;
pub use ledger::LedgerRow;
pub use objects::UpstreamObject;
pub use pg::PgStore;
pub use pressure::PressureRow;
pub use requests::{PendingRequest, RequestFilter};
pub use safety::{GuardrailHour, SafetyColumn, SafetyEventRow, SafetyFilter};
pub use sqlite::SqliteStore;

/// Why a store operation failed. Messages are for operator logs only and never carry connection
/// details.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("postgres: {message}")]
    Postgres { message: String, outage: bool },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("stored record is corrupt: {0}")]
    Corrupt(#[from] serde_json::Error),
    #[error("the store is closed")]
    Closed,
    #[error("the store is unavailable")]
    Unavailable,
    #[error("the store did not answer in time")]
    Timeout,
    #[error("another node committed a newer configuration first")]
    Conflict,
    #[error("schema migration {schema} needs binary {needs} or later; this binary is {binary}")]
    SchemaTooNew {
        schema: i32,
        needs: String,
        binary: String,
    },
    #[error("the store schema is incomplete: {0}")]
    NotMigrated(String),
    #[error("{0}")]
    Config(String),
    #[error("the request-log store: {message}")]
    LogStore { message: String, outage: bool },
}

impl From<tokio_postgres::Error> for StoreError {
    fn from(e: tokio_postgres::Error) -> StoreError {
        let outage = match e.code() {
            None => true,
            Some(code) => matches!(code.code().get(..2), Some("08" | "53" | "57")),
        };
        StoreError::Postgres {
            message: e.to_string(),
            outage,
        }
    }
}

impl StoreError {
    /// Whether the failure says the store is unreachable, rather than that it refused a statement.
    #[must_use]
    pub fn is_outage(&self) -> bool {
        match self {
            StoreError::Postgres { outage, .. } | StoreError::LogStore { outage, .. } => *outage,
            StoreError::Timeout | StoreError::Unavailable | StoreError::Closed | StoreError::Io(_) => true,
            _ => false,
        }
    }
}

/// The current dynamic configuration, serialised by its owner.
#[derive(Clone, Debug)]
pub struct StoredConfig {
    pub generation: u64,
    pub document: String,
    pub metadata: String,
}

/// One row of the generation history.
#[derive(Clone, Debug)]
pub struct GenerationRecord {
    pub generation: u64,
    pub created_at: String,
    pub actor: String,
    pub source: String,
    pub summary: String,
}

/// An admin token: its verifier only, never the plaintext.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdminTokenRecord {
    pub kid: String,
    pub user: String,
    pub verifier: String,
    pub description: Option<String>,
    pub created_at: String,
    pub expires_at: Option<String>,
    /// `read_only` limits the token to the read permissions of its user's bindings; `None` is full.
    pub access: Option<String>,
    pub last_used_us: Option<i64>,
}

/// Filters and position of an audit listing.
#[derive(Clone, Debug, Default)]
pub struct AuditQuery {
    pub actor: Option<String>,
    pub resource: Option<String>,
    pub action: Option<String>,
    pub outcome: Option<String>,
    /// `write` or `read`.
    pub kind: Option<String>,
    pub from: Option<UtcMicros>,
    pub to: Option<UtcMicros>,
    /// Only entries older than this position (the cursor).
    pub before: Option<i64>,
    pub limit: usize,
}

/// A page of audit entries, newest first.
#[derive(Clone, Debug)]
pub struct AuditPage {
    pub items: Vec<AuditEntry>,
    /// The position to continue from, when more entries match.
    pub next: Option<i64>,
}

/// The store of a node: the embedded one in standalone mode, PostgreSQL in cluster mode.
#[derive(Debug)]
pub enum Store {
    Sqlite(SqliteStore),
    Postgres(Box<PgStore>),
}

pub(crate) fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

impl Store {
    /// Opens the embedded store in `dir`.
    pub fn open_embedded(dir: &std::path::Path) -> Result<Store, StoreError> {
        Ok(Store::Sqlite(SqliteStore::open(dir)?))
    }

    /// Opens the cluster store, migrating its schema as `settings` says.
    pub async fn open_postgres(settings: &pg::PgSettings) -> Result<Store, StoreError> {
        Ok(Store::Postgres(Box::new(PgStore::open(settings).await?)))
    }

    /// The cluster store of a node that could not reach it at startup; see [`PgStore::offline`].
    pub fn postgres_offline(settings: &pg::PgSettings) -> Result<Store, StoreError> {
        Ok(Store::Postgres(Box::new(PgStore::offline(settings)?)))
    }

    /// The cluster store, in cluster mode.
    #[must_use]
    pub fn postgres(&self) -> Option<&PgStore> {
        match self {
            Store::Postgres(p) => Some(p.as_ref()),
            Store::Sqlite(_) => None,
        }
    }

    /// Whether the store is known to be unreachable (the embedded store never is).
    #[must_use]
    pub fn is_down(&self) -> bool {
        self.postgres().is_some_and(PgStore::is_down)
    }

    /// Checks that the store answers.
    pub async fn ping(&self) -> Result<(), StoreError> {
        match self {
            Store::Sqlite(s) => s.call(|c| Ok(c.query_row("SELECT 1", [], |_| Ok(()))?)).await,
            Store::Postgres(p) => p.ping().await,
        }
    }

    pub async fn load_config(&self) -> Result<Option<StoredConfig>, StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(|c| {
                    Ok(c.query_row(
                        "SELECT generation, document, metadata FROM config WHERE id = 1",
                        [],
                        |r| {
                            Ok(StoredConfig {
                                generation: u64::try_from(r.get::<_, i64>(0)?).unwrap_or(0),
                                document: r.get(1)?,
                                metadata: r.get(2)?,
                            })
                        },
                    )
                    .optional()?)
                })
                .await
            }
            Store::Postgres(p) => p.load_config().await,
        }
    }

    /// Commits a new generation, its history row and its audit entry in one transaction. On the
    /// cluster store the commit is refused with [`StoreError::Conflict`] unless the stored
    /// generation is the one before it.
    pub async fn commit_config(
        &self,
        config: StoredConfig,
        history: GenerationRecord,
        audit: Option<AuditEntry>,
    ) -> Result<(), StoreError> {
        let s = match self {
            Store::Sqlite(s) => s,
            Store::Postgres(p) => return p.commit_config(config, history, audit).await,
        };
        s.call(move |c| {
            let tx = c.transaction()?;
            tx.execute(
                "INSERT INTO config (id, generation, document, metadata) VALUES (1, ?1, ?2, ?3)
                 ON CONFLICT (id) DO UPDATE SET generation = ?1, document = ?2, metadata = ?3",
                params![to_i64(config.generation), config.document, config.metadata],
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO generations (generation, created_at, actor, source, summary)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    to_i64(history.generation),
                    history.created_at,
                    history.actor,
                    history.source,
                    history.summary
                ],
            )?;
            if let Some(entry) = audit {
                insert_audit(&tx, &entry)?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn append_audit(&self, entry: AuditEntry) -> Result<(), StoreError> {
        match self {
            Store::Sqlite(s) => s.call(move |c| insert_audit(c, &entry)).await,
            Store::Postgres(p) => p.append_audit(entry).await,
        }
    }

    pub async fn audit(&self, q: AuditQuery) -> Result<AuditPage, StoreError> {
        let s = match self {
            Store::Sqlite(s) => s,
            Store::Postgres(p) => return p.audit(q).await,
        };
        s.call(move |c| {
            let mut sql = String::from("SELECT seq, entry FROM audit WHERE 1 = 1");
            let mut args: Vec<rusqlite::types::Value> = Vec::new();
            let mut text_filter = |column: &str, value: &Option<String>, sql: &mut String| {
                if let Some(v) = value {
                    let (clause, value) = text_condition(column, v, "?");
                    sql.push_str(&clause);
                    args.push(rusqlite::types::Value::Text(value));
                }
            };
            text_filter("actor_id", &q.actor, &mut sql);
            text_filter("resource", &q.resource, &mut sql);
            text_filter("action", &q.action, &mut sql);
            text_filter("outcome", &q.outcome, &mut sql);
            text_filter("kind", &q.kind, &mut sql);
            if let Some(from) = q.from {
                sql.push_str(" AND time_us >= ?");
                args.push(rusqlite::types::Value::Integer(from.0));
            }
            if let Some(to) = q.to {
                sql.push_str(" AND time_us <= ?");
                args.push(rusqlite::types::Value::Integer(to.0));
            }
            if let Some(before) = q.before {
                sql.push_str(" AND seq < ?");
                args.push(rusqlite::types::Value::Integer(before));
            }
            sql.push_str(" ORDER BY seq DESC LIMIT ?");
            let limit = i64::try_from(q.limit).unwrap_or(i64::MAX);
            args.push(rusqlite::types::Value::Integer(limit.saturating_add(1)));
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(args), |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?;
            let mut items = Vec::new();
            let mut last_seq = None;
            let mut more = false;
            for row in rows {
                let (seq, entry) = row?;
                if items.len() == q.limit {
                    more = true;
                    break;
                }
                items.push(serde_json::from_str::<AuditEntry>(&entry)?);
                last_seq = Some(seq);
            }
            Ok(AuditPage {
                items,
                next: if more { last_seq } else { None },
            })
        })
        .await
    }

    pub async fn audit_entry(&self, id: String) -> Result<Option<AuditEntry>, StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    let entry: Option<String> = c
                        .query_row("SELECT entry FROM audit WHERE id = ?1", [id], |r| r.get(0))
                        .optional()?;
                    Ok(entry.map(|e| serde_json::from_str(&e)).transpose()?)
                })
                .await
            }
            Store::Postgres(p) => p.audit_entry(id).await,
        }
    }

    /// Removes every audit entry (test-mode reset only).
    pub async fn clear_audit(&self) -> Result<(), StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(|c| Ok(c.execute("DELETE FROM audit", []).map(|_| ())?))
                    .await
            }
            Store::Postgres(p) => p.execute("DELETE FROM audit", &[]).await.map(|_| ()),
        }
    }

    /// The value stored under `key`, storing `init` first when there is none.
    pub async fn get_or_init(&self, key: &str, init: Vec<u8>) -> Result<Vec<u8>, StoreError> {
        let key = key.to_owned();
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    c.execute(
                        "INSERT OR IGNORE INTO kv (key, value) VALUES (?1, ?2)",
                        params![key, init],
                    )?;
                    Ok(c.query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))?)
                })
                .await
            }
            Store::Postgres(p) => p.get_or_init(&key, init).await,
        }
    }

    /// Stores `value` under `key`, replacing what was there.
    pub async fn put_value(&self, key: &str, value: Vec<u8>) -> Result<(), StoreError> {
        let key = key.to_owned();
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    c.execute(
                        "INSERT INTO kv (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                        params![key, value],
                    )?;
                    Ok(())
                })
                .await
            }
            Store::Postgres(p) => p.put_value(&key, value).await,
        }
    }

    pub async fn put_admin_token(&self, rec: AdminTokenRecord) -> Result<(), StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    c.execute(
                        "INSERT INTO admin_tokens (kid, user_name, verifier, description, created_at, expires_at, access)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![
                            rec.kid,
                            rec.user,
                            rec.verifier,
                            rec.description,
                            rec.created_at,
                            rec.expires_at,
                            rec.access
                        ],
                    )?;
                    Ok(())
                })
                .await
            }
            Store::Postgres(p) => p.put_admin_token(rec).await,
        }
    }

    pub async fn admin_token(&self, kid: String) -> Result<Option<AdminTokenRecord>, StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    Ok(c.query_row(
                        "SELECT kid, user_name, verifier, description, created_at, expires_at, access, last_used_us
                         FROM admin_tokens WHERE kid = ?1",
                        [kid],
                        token_row,
                    )
                    .optional()?)
                })
                .await
            }
            Store::Postgres(p) => p.admin_token(kid).await,
        }
    }

    pub async fn admin_tokens_of(&self, user: String) -> Result<Vec<AdminTokenRecord>, StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    let mut stmt = c.prepare(
                        "SELECT kid, user_name, verifier, description, created_at, expires_at, access, last_used_us
                         FROM admin_tokens WHERE user_name = ?1 ORDER BY kid",
                    )?;
                    let rows = stmt.query_map([user], token_row)?;
                    Ok(rows.collect::<Result<Vec<_>, _>>()?)
                })
                .await
            }
            Store::Postgres(p) => p.admin_tokens_of(user).await,
        }
    }

    /// Deletes one token; true when it existed.
    pub async fn delete_admin_token(&self, kid: String) -> Result<bool, StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| Ok(c.execute("DELETE FROM admin_tokens WHERE kid = ?1", [kid])? > 0))
                    .await
            }
            Store::Postgres(p) => p
                .execute("DELETE FROM admin_tokens WHERE kid = $1", &[&kid])
                .await
                .map(|n| n > 0),
        }
    }

    /// Deletes every token of a user (the user was deleted).
    pub async fn delete_admin_tokens_of(&self, user: String) -> Result<(), StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    Ok(c.execute("DELETE FROM admin_tokens WHERE user_name = ?1", [user])
                        .map(|_| ())?)
                })
                .await
            }
            Store::Postgres(p) => p
                .execute("DELETE FROM admin_tokens WHERE user_name = $1", &[&user])
                .await
                .map(|_| ()),
        }
    }

    pub fn close(&self) {
        match self {
            Store::Sqlite(s) => s.close(),
            Store::Postgres(p) => p.close(),
        }
    }
}

/// An audit filter on `column`: equality, or, for a value ending in `*`, the values starting with
/// what precedes it. Returns the clause (with `placeholder` for the value) and the value to bind.
pub(crate) fn text_condition(column: &str, value: &str, placeholder: &str) -> (String, String) {
    match value.strip_suffix('*') {
        Some(prefix) => (
            format!(
                " AND substr({column}, 1, {}) = {placeholder}",
                prefix.chars().count()
            ),
            prefix.to_owned(),
        ),
        None => (format!(" AND {column} = {placeholder}"), value.to_owned()),
    }
}

fn insert_audit(c: &rusqlite::Connection, e: &AuditEntry) -> Result<(), StoreError> {
    let time_us = UtcMicros::parse_rfc3339(&e.time).map(|t| t.0).unwrap_or(0);
    c.execute(
        "INSERT INTO audit (id, time, time_us, actor_id, action, resource, outcome, entry, kind)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            e.id,
            e.time,
            time_us,
            e.actor.id,
            e.action,
            e.resource,
            e.outcome.as_str(),
            serde_json::to_string(e)?,
            e.kind.as_str()
        ],
    )?;
    Ok(())
}

fn token_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<AdminTokenRecord> {
    Ok(AdminTokenRecord {
        kid: r.get(0)?,
        user: r.get(1)?,
        verifier: r.get(2)?,
        description: r.get(3)?,
        created_at: r.get(4)?,
        expires_at: r.get(5)?,
        access: r.get(6)?,
        last_used_us: r.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use antirion_core::audit::{Actor, ActorKind, Outcome};

    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("antirion-store-{name}-{}", uuid_like()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn uuid_like() -> String {
        format!("{:x}", antirion_core::ResourceUid::random().0)
    }

    fn entry(n: usize, outcome: Outcome) -> AuditEntry {
        AuditEntry {
            id: format!("e{n}"),
            time: UtcMicros(1_700_000_000_000_000 + i64::try_from(n).unwrap()).to_rfc3339(),
            actor: Actor {
                kind: ActorKind::Token,
                id: Some("bootstrap".into()),
            },
            action: "models.update".into(),
            resource: format!("models/m{n}"),
            outcome,
            status_code: 200,
            generation_before: Some(1),
            generation_after: Some(2),
            request_id: "r".into(),
            source_ip: None,
            kind: antirion_core::audit::AuditKind::Write,
            reason: None,
            changes: serde_json::Value::Null,
        }
    }

    #[tokio::test]
    async fn config_round_trip_and_kv() {
        let dir = temp_dir("config");
        let store = Store::open_embedded(&dir).unwrap();
        assert!(store.load_config().await.unwrap().is_none());
        let cfg = StoredConfig {
            generation: 3,
            document: "{}".into(),
            metadata: "{}".into(),
        };
        let hist = GenerationRecord {
            generation: 3,
            created_at: "t".into(),
            actor: "a".into(),
            source: "api".into(),
            summary: "".into(),
        };
        store
            .commit_config(cfg, hist, Some(entry(1, Outcome::Success)))
            .await
            .unwrap();
        assert_eq!(store.load_config().await.unwrap().unwrap().generation, 3);
        let first = store.get_or_init("pepper", vec![1, 2]).await.unwrap();
        let again = store.get_or_init("pepper", vec![9]).await.unwrap();
        assert_eq!(first, vec![1, 2]);
        assert_eq!(again, first);
        store.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn audit_pages_newest_first_with_filters() {
        let dir = temp_dir("audit");
        let store = Store::open_embedded(&dir).unwrap();
        for n in 0..5 {
            let outcome = if n % 2 == 0 {
                Outcome::Success
            } else {
                Outcome::Denied
            };
            store.append_audit(entry(n, outcome)).await.unwrap();
        }
        let page = store
            .audit(AuditQuery {
                limit: 2,
                ..AuditQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(
            page.items.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["e4", "e3"]
        );
        let next = store
            .audit(AuditQuery {
                limit: 10,
                before: page.next,
                ..AuditQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(next.items.len(), 3);
        assert!(next.next.is_none());
        let denied = store
            .audit(AuditQuery {
                limit: 10,
                outcome: Some("denied".into()),
                ..AuditQuery::default()
            })
            .await
            .unwrap();
        assert!(denied.items.iter().all(|e| e.outcome == Outcome::Denied));
        assert_eq!(denied.items.len(), 2);
        let under = |resource: &str| AuditQuery {
            limit: 10,
            resource: Some(resource.into()),
            ..AuditQuery::default()
        };
        assert_eq!(store.audit(under("models/*")).await.unwrap().items.len(), 5);
        assert_eq!(store.audit(under("models/m1*")).await.unwrap().items.len(), 1);
        assert_eq!(store.audit(under("models/")).await.unwrap().items.len(), 0);
        store.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn admin_tokens() {
        let dir = temp_dir("tokens");
        let store = Store::open_embedded(&dir).unwrap();
        let rec = AdminTokenRecord {
            kid: "abc".into(),
            user: "svc".into(),
            verifier: "v".into(),
            description: None,
            created_at: "t".into(),
            expires_at: None,
            access: Some("read_only".into()),
            last_used_us: None,
        };
        store.put_admin_token(rec.clone()).await.unwrap();
        assert_eq!(store.admin_token("abc".into()).await.unwrap(), Some(rec));
        store.touch_admin_token("abc".into(), 7).await.unwrap();
        assert_eq!(
            store
                .admin_token("abc".into())
                .await
                .unwrap()
                .unwrap()
                .last_used_us,
            Some(7)
        );
        assert_eq!(store.admin_tokens_of("svc".into()).await.unwrap().len(), 1);
        store.delete_admin_tokens_of("svc".into()).await.unwrap();
        assert!(store.admin_token("abc".into()).await.unwrap().is_none());
        store.close();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
