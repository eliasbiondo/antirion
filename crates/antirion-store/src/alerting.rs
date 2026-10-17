//! Alerting state that is not configuration: alert instances with their timelines, and the events
//! sent to webhooks and channels, kept for redelivery. An instance is one JSON body with the columns
//! its lists and updates need; at most one instance per fingerprint is open (not resolved), which
//! the store enforces, so two nodes raising the same fingerprint at once end with one instance.

use rusqlite::{OptionalExtension, params};

use crate::{Store, StoreError};

/// One alert instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstanceRow {
    pub id: String,
    pub rule: String,
    pub fingerprint: String,
    /// `firing`, `acknowledged` or `resolved`.
    pub status: String,
    pub fired_us: i64,
    pub updated_us: i64,
    /// Raised on every update; an update names the version it read.
    pub version: i64,
    pub body: String,
}

/// One event as sent to one target, kept for redelivery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventRow {
    pub event_id: String,
    /// `webhook` or `channel`.
    pub target_kind: String,
    pub target: String,
    pub event_type: String,
    pub body: String,
    pub created_us: i64,
}

pub(crate) const MIGRATIONS: &[&str] = &[
    "CREATE INDEX IF NOT EXISTS audit_kind ON audit (kind)",
    "CREATE TABLE IF NOT EXISTS alert_instances (
        id TEXT PRIMARY KEY,
        rule TEXT NOT NULL,
        fingerprint TEXT NOT NULL,
        status TEXT NOT NULL,
        fired_us INTEGER NOT NULL,
        updated_us INTEGER NOT NULL,
        version INTEGER NOT NULL,
        body TEXT NOT NULL)",
    "CREATE UNIQUE INDEX IF NOT EXISTS alert_instances_open ON alert_instances (fingerprint)
        WHERE status <> 'resolved'",
    "CREATE INDEX IF NOT EXISTS alert_instances_fired ON alert_instances (fired_us)",
    "CREATE TABLE IF NOT EXISTS alert_events (
        event_id TEXT NOT NULL,
        target_kind TEXT NOT NULL,
        target TEXT NOT NULL,
        event_type TEXT NOT NULL,
        body TEXT NOT NULL,
        created_us INTEGER NOT NULL,
        PRIMARY KEY (event_id, target_kind, target))",
    "CREATE INDEX IF NOT EXISTS alert_events_created ON alert_events (created_us)",
    "CREATE TABLE IF NOT EXISTS deliveries (
        seq INTEGER PRIMARY KEY AUTOINCREMENT,
        event_id TEXT NOT NULL,
        target_kind TEXT NOT NULL,
        target TEXT NOT NULL,
        event_type TEXT NOT NULL,
        attempt INTEGER NOT NULL,
        time_us INTEGER NOT NULL,
        status_code INTEGER,
        result TEXT NOT NULL,
        items INTEGER NOT NULL)",
    "CREATE INDEX IF NOT EXISTS deliveries_time ON deliveries (time_us)",
    "CREATE TABLE IF NOT EXISTS delivery_queues (
        node_id TEXT NOT NULL,
        target_kind TEXT NOT NULL,
        target TEXT NOT NULL,
        depth INTEGER NOT NULL,
        oldest_us INTEGER,
        updated_us INTEGER NOT NULL,
        PRIMARY KEY (node_id, target_kind, target))",
    "CREATE TABLE IF NOT EXISTS audit_sink_cursors (
        sink TEXT PRIMARY KEY,
        last_seq INTEGER NOT NULL)",
];

/// Columns added to tables of earlier releases: `(table, column, declaration)`.
pub(crate) const ADDED_COLUMNS: &[(&str, &str, &str)] = &[("audit", "kind", "TEXT NOT NULL DEFAULT 'write'")];

/// The same schema on PostgreSQL, as one migration.
pub(crate) const PG_MIGRATION: &str =
    "ALTER TABLE audit ADD COLUMN IF NOT EXISTS kind text NOT NULL DEFAULT 'write';
     CREATE INDEX IF NOT EXISTS audit_kind ON audit (kind);
     CREATE TABLE alert_instances (
        id text PRIMARY KEY,
        rule text NOT NULL,
        fingerprint text NOT NULL,
        status text NOT NULL,
        fired_us bigint NOT NULL,
        updated_us bigint NOT NULL,
        version bigint NOT NULL,
        body text NOT NULL);
     CREATE UNIQUE INDEX alert_instances_open ON alert_instances (fingerprint) WHERE status <> 'resolved';
     CREATE INDEX alert_instances_fired ON alert_instances (fired_us);
     CREATE TABLE alert_events (
        event_id text NOT NULL,
        target_kind text NOT NULL,
        target text NOT NULL,
        event_type text NOT NULL,
        body text NOT NULL,
        created_us bigint NOT NULL,
        PRIMARY KEY (event_id, target_kind, target));
     CREATE INDEX alert_events_created ON alert_events (created_us);
     CREATE TABLE deliveries (
        seq bigserial PRIMARY KEY,
        event_id text NOT NULL,
        target_kind text NOT NULL,
        target text NOT NULL,
        event_type text NOT NULL,
        attempt integer NOT NULL,
        time_us bigint NOT NULL,
        status_code integer,
        result text NOT NULL,
        items bigint NOT NULL);
     CREATE INDEX deliveries_time ON deliveries (time_us);
     CREATE TABLE delivery_queues (
        node_id text NOT NULL,
        target_kind text NOT NULL,
        target text NOT NULL,
        depth bigint NOT NULL,
        oldest_us bigint,
        updated_us bigint NOT NULL,
        PRIMARY KEY (node_id, target_kind, target));
     CREATE TABLE audit_sink_cursors (
        sink text PRIMARY KEY,
        last_seq bigint NOT NULL);";

const INSTANCE: &str = "id, rule, fingerprint, status, fired_us, updated_us, version, body";

fn instance(r: &rusqlite::Row<'_>) -> rusqlite::Result<InstanceRow> {
    Ok(InstanceRow {
        id: r.get(0)?,
        rule: r.get(1)?,
        fingerprint: r.get(2)?,
        status: r.get(3)?,
        fired_us: r.get(4)?,
        updated_us: r.get(5)?,
        version: r.get(6)?,
        body: r.get(7)?,
    })
}

fn pg_instance(r: &tokio_postgres::Row) -> Result<InstanceRow, StoreError> {
    Ok(InstanceRow {
        id: r.try_get(0)?,
        rule: r.try_get(1)?,
        fingerprint: r.try_get(2)?,
        status: r.try_get(3)?,
        fired_us: r.try_get(4)?,
        updated_us: r.try_get(5)?,
        version: r.try_get(6)?,
        body: r.try_get(7)?,
    })
}

fn is_unique_violation(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

impl Store {
    /// Stores a new instance; `false` when an open instance already has its fingerprint.
    pub async fn insert_instance(&self, row: InstanceRow) -> Result<bool, StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    match c.execute(
                        &format!(
                            "INSERT INTO alert_instances ({INSTANCE}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
                        ),
                        params![
                            row.id,
                            row.rule,
                            row.fingerprint,
                            row.status,
                            row.fired_us,
                            row.updated_us,
                            row.version,
                            row.body
                        ],
                    ) {
                        Ok(_) => Ok(true),
                        Err(e) if is_unique_violation(&e) => Ok(false),
                        Err(e) => Err(e.into()),
                    }
                })
                .await
            }
            Store::Postgres(p) => p
                .execute(
                    &format!(
                        "INSERT INTO alert_instances ({INSTANCE}) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                         ON CONFLICT DO NOTHING"
                    ),
                    &[
                        &row.id,
                        &row.rule,
                        &row.fingerprint,
                        &row.status,
                        &row.fired_us,
                        &row.updated_us,
                        &row.version,
                        &row.body,
                    ],
                )
                .await
                .map(|n| n == 1),
        }
    }

    /// Replaces an instance while its version is still `row.version - 1`; `false` when another
    /// writer changed it first.
    pub async fn update_instance(&self, row: InstanceRow) -> Result<bool, StoreError> {
        let read = row.version - 1;
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    Ok(c.execute(
                        "UPDATE alert_instances SET status = ?2, updated_us = ?3, version = ?4, body = ?5
                         WHERE id = ?1 AND version = ?6",
                        params![row.id, row.status, row.updated_us, row.version, row.body, read],
                    )? == 1)
                })
                .await
            }
            Store::Postgres(p) => p
                .execute(
                    "UPDATE alert_instances SET status = $2, updated_us = $3, version = $4, body = $5
                     WHERE id = $1 AND version = $6",
                    &[
                        &row.id,
                        &row.status,
                        &row.updated_us,
                        &row.version,
                        &row.body,
                        &read,
                    ],
                )
                .await
                .map(|n| n == 1),
        }
    }

    pub async fn instance(&self, id: &str) -> Result<Option<InstanceRow>, StoreError> {
        let id = id.to_owned();
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    Ok(c.query_row(
                        &format!("SELECT {INSTANCE} FROM alert_instances WHERE id = ?1"),
                        [id],
                        instance,
                    )
                    .optional()?)
                })
                .await
            }
            Store::Postgres(p) => p
                .rows(
                    &format!("SELECT {INSTANCE} FROM alert_instances WHERE id = $1"),
                    &[&id],
                )
                .await?
                .first()
                .map(pg_instance)
                .transpose(),
        }
    }

    /// The instances fired at or after `since_us` (every one when `None`), newest first; only those
    /// of `fingerprint` when it is set.
    pub async fn instances(
        &self,
        since_us: Option<i64>,
        fingerprint: Option<&str>,
    ) -> Result<Vec<InstanceRow>, StoreError> {
        let since = since_us.unwrap_or(i64::MIN);
        let fp = fingerprint.map(str::to_owned);
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    let mut stmt = c.prepare(&format!(
                        "SELECT {INSTANCE} FROM alert_instances
                         WHERE fired_us >= ?1 AND (?2 IS NULL OR fingerprint = ?2)
                         ORDER BY fired_us DESC, id DESC"
                    ))?;
                    let rows = stmt.query_map(params![since, fp], instance)?;
                    Ok(rows.collect::<Result<Vec<_>, _>>()?)
                })
                .await
            }
            Store::Postgres(p) => p
                .rows(
                    &format!(
                        "SELECT {INSTANCE} FROM alert_instances
                         WHERE fired_us >= $1 AND ($2::text IS NULL OR fingerprint = $2)
                         ORDER BY fired_us DESC, id DESC"
                    ),
                    &[&since, &fp],
                )
                .await?
                .iter()
                .map(pg_instance)
                .collect(),
        }
    }

    /// The open (not resolved) instances.
    pub async fn open_instances(&self) -> Result<Vec<InstanceRow>, StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    let mut stmt = c.prepare(&format!(
                        "SELECT {INSTANCE} FROM alert_instances WHERE status <> 'resolved' ORDER BY fired_us"
                    ))?;
                    let rows = stmt.query_map([], instance)?;
                    Ok(rows.collect::<Result<Vec<_>, _>>()?)
                })
                .await
            }
            Store::Postgres(p) => p
                .rows(
                    &format!(
                        "SELECT {INSTANCE} FROM alert_instances WHERE status <> 'resolved' ORDER BY fired_us"
                    ),
                    &[],
                )
                .await?
                .iter()
                .map(pg_instance)
                .collect(),
        }
    }

    /// Removes resolved instances last updated before `before_us`, and events created before
    /// `events_before_us`.
    pub async fn purge_alerting(&self, before_us: i64, events_before_us: i64) -> Result<(), StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    c.execute(
                        "DELETE FROM alert_instances WHERE status = 'resolved' AND updated_us < ?1",
                        [before_us],
                    )?;
                    c.execute(
                        "DELETE FROM alert_events WHERE created_us < ?1",
                        [events_before_us],
                    )?;
                    c.execute("DELETE FROM deliveries WHERE time_us < ?1", [events_before_us])?;
                    Ok(())
                })
                .await
            }
            Store::Postgres(p) => {
                p.execute(
                    "DELETE FROM alert_instances WHERE status = 'resolved' AND updated_us < $1",
                    &[&before_us],
                )
                .await?;
                p.execute(
                    "DELETE FROM alert_events WHERE created_us < $1",
                    &[&events_before_us],
                )
                .await?;
                p.execute("DELETE FROM deliveries WHERE time_us < $1", &[&events_before_us])
                    .await
                    .map(|_| ())
            }
        }
    }

    /// Forgets every instance, event, delivery and queue report (test-mode reset only).
    pub async fn clear_alerting(&self) -> Result<(), StoreError> {
        const TABLES: [&str; 4] = ["alert_instances", "alert_events", "deliveries", "delivery_queues"];
        match self {
            Store::Sqlite(s) => {
                s.call(|c| {
                    for t in TABLES {
                        c.execute(&format!("DELETE FROM {t}"), [])?;
                    }
                    Ok(())
                })
                .await
            }
            Store::Postgres(p) => {
                for t in TABLES {
                    p.execute(&format!("DELETE FROM {t}"), &[]).await?;
                }
                Ok(())
            }
        }
    }

    /// Keeps an event sent to one target.
    pub async fn put_event(&self, row: EventRow) -> Result<(), StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    c.execute(
                        "INSERT OR IGNORE INTO alert_events
                            (event_id, target_kind, target, event_type, body, created_us)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![
                            row.event_id,
                            row.target_kind,
                            row.target,
                            row.event_type,
                            row.body,
                            row.created_us
                        ],
                    )?;
                    Ok(())
                })
                .await
            }
            Store::Postgres(p) => p
                .execute(
                    "INSERT INTO alert_events (event_id, target_kind, target, event_type, body, created_us)
                     VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
                    &[
                        &row.event_id,
                        &row.target_kind,
                        &row.target,
                        &row.event_type,
                        &row.body,
                        &row.created_us,
                    ],
                )
                .await
                .map(|_| ()),
        }
    }

    /// Every target an event was sent to.
    pub async fn event(&self, event_id: &str) -> Result<Vec<EventRow>, StoreError> {
        const SQL: &str =
            "SELECT event_id, target_kind, target, event_type, body, created_us FROM alert_events";
        let id = event_id.to_owned();
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    let mut stmt = c.prepare(&format!("{SQL} WHERE event_id = ?1 ORDER BY target"))?;
                    let rows = stmt.query_map([id], |r| {
                        Ok(EventRow {
                            event_id: r.get(0)?,
                            target_kind: r.get(1)?,
                            target: r.get(2)?,
                            event_type: r.get(3)?,
                            body: r.get(4)?,
                            created_us: r.get(5)?,
                        })
                    })?;
                    Ok(rows.collect::<Result<Vec<_>, _>>()?)
                })
                .await
            }
            Store::Postgres(p) => p
                .rows(&format!("{SQL} WHERE event_id = $1 ORDER BY target"), &[&id])
                .await?
                .iter()
                .map(|r| {
                    Ok(EventRow {
                        event_id: r.try_get(0)?,
                        target_kind: r.try_get(1)?,
                        target: r.try_get(2)?,
                        event_type: r.try_get(3)?,
                        body: r.try_get(4)?,
                        created_us: r.try_get(5)?,
                    })
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use antirion_core::audit::{Actor, ActorKind, AuditEntry, AuditKind, Outcome};

    use super::*;
    use crate::{AuditQuery, DeliveryQuery, DeliveryRow, QueueRow};

    fn row(id: &str, fingerprint: &str, fired_us: i64) -> InstanceRow {
        InstanceRow {
            id: id.into(),
            rule: "r".into(),
            fingerprint: fingerprint.into(),
            status: "firing".into(),
            fired_us,
            updated_us: fired_us,
            version: 1,
            body: format!("{{\"id\":\"{id}\"}}"),
        }
    }

    fn audit(n: i64, kind: AuditKind) -> AuditEntry {
        AuditEntry {
            id: format!("a{n}"),
            time: antirion_core::UtcMicros(1_700_000_000_000_000 + n).to_rfc3339(),
            actor: Actor {
                kind: ActorKind::Token,
                id: Some("t".into()),
            },
            action: "keys.create".into(),
            resource: format!("keys/k{n}"),
            outcome: Outcome::Success,
            status_code: 201,
            generation_before: None,
            generation_after: None,
            request_id: "r".into(),
            source_ip: None,
            changes: serde_json::Value::Null,
            kind,
            reason: Some("why".into()),
        }
    }

    async fn exercise(store: &Store) {
        assert!(store.insert_instance(row("i1", "fp", 10)).await.unwrap());
        assert!(
            !store.insert_instance(row("i2", "fp", 11)).await.unwrap(),
            "one open instance per fingerprint"
        );
        let mut resolved = row("i1", "fp", 10);
        resolved.status = "resolved".into();
        resolved.version = 2;
        resolved.updated_us = 20;
        assert!(store.update_instance(resolved.clone()).await.unwrap());
        assert!(
            !store.update_instance(resolved).await.unwrap(),
            "an update of a version already replaced is refused"
        );
        assert!(store.insert_instance(row("i2", "fp", 30)).await.unwrap());
        let all = store.instances(None, Some("fp")).await.unwrap();
        assert_eq!(
            all.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["i2", "i1"]
        );
        assert_eq!(store.open_instances().await.unwrap().len(), 1);
        assert_eq!(store.instance("i1").await.unwrap().unwrap().status, "resolved");
        assert!(store.instances(Some(25), None).await.unwrap().len() == 1);

        for (n, result) in [(1, "failed"), (2, "delivered")] {
            store
                .record_delivery(DeliveryRow {
                    event_id: "e1".into(),
                    target_kind: "webhook".into(),
                    target: "ops".into(),
                    event_type: "test.ping".into(),
                    attempt: n,
                    time_us: 100 + i64::from(n),
                    status_code: Some(if n == 1 { 500 } else { 200 }),
                    result: result.into(),
                    items: 1,
                })
                .await
                .unwrap();
        }
        let (rows, next) = store
            .deliveries(DeliveryQuery {
                target: Some("ops".into()),
                limit: 1,
                ..DeliveryQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(rows.first().map(|r| r.1.attempt), Some(2));
        let (rows, _) = store
            .deliveries(DeliveryQuery {
                before: next,
                limit: 10,
                ..DeliveryQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(rows.iter().map(|r| r.1.attempt).collect::<Vec<_>>(), [1]);
        let (rows, _) = store
            .deliveries(DeliveryQuery {
                result: Some("delivered".into()),
                limit: 10,
                ..DeliveryQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);

        store
            .put_event(EventRow {
                event_id: "e1".into(),
                target_kind: "webhook".into(),
                target: "ops".into(),
                event_type: "test.ping".into(),
                body: "{}".into(),
                created_us: 100,
            })
            .await
            .unwrap();
        assert_eq!(store.event("e1").await.unwrap().len(), 1);

        let q = QueueRow {
            node_id: "n".into(),
            target_kind: "webhook".into(),
            target: "ops".into(),
            depth: 3,
            oldest_us: Some(5),
            updated_us: 50,
        };
        store.report_queue(q.clone()).await.unwrap();
        store.report_queue(QueueRow { depth: 1, ..q }).await.unwrap();
        let queues = store.queues(40).await.unwrap();
        assert_eq!(queues.iter().map(|q| q.depth).collect::<Vec<_>>(), [1]);
        assert!(store.queues(60).await.unwrap().is_empty());

        store.append_audit(audit(1, AuditKind::Write)).await.unwrap();
        store.append_audit(audit(2, AuditKind::Read)).await.unwrap();
        let reads = store
            .audit(AuditQuery {
                kind: Some("read".into()),
                limit: 10,
                ..AuditQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(reads.items.len(), 1);
        assert_eq!(reads.items[0].reason.as_deref(), Some("why"));
        let head = store.audit_head().await.unwrap();
        let after = store.audit_after(0, 10).await.unwrap();
        assert_eq!(after.len(), 2);
        assert_eq!(after.last().map(|e| e.0), Some(head));
        assert_eq!(store.audit_backlog(after[0].0).await.unwrap().0, 1);
        assert_eq!(store.sink_cursor("s").await.unwrap(), None);
        store.set_sink_cursor("s", head).await.unwrap();
        assert_eq!(store.sink_cursor("s").await.unwrap(), Some(head));

        store.purge_alerting(25, 0).await.unwrap();
        assert!(store.instance("i1").await.unwrap().is_none());
        store.clear_alerting().await.unwrap();
        assert!(store.instances(None, None).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_embedded_store_keeps_alerting_state() {
        let dir = std::env::temp_dir().join(format!(
            "antirion-store-alerting-{:x}",
            antirion_core::ResourceUid::random().0
        ));
        let store = Store::open_embedded(&dir).unwrap();
        exercise(&store).await;
        store.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn postgres_keeps_alerting_state() {
        let Ok(base) = std::env::var("ANTIRION_TEST_POSTGRES_URL") else {
            return;
        };
        let name = format!("t_{:x}", antirion_core::ResourceUid::random().0);
        let (client, conn) = tokio_postgres::connect(&base, tokio_postgres::NoTls)
            .await
            .unwrap();
        let mut set = tokio::task::JoinSet::new();
        set.spawn(async move {
            let _ = conn.await;
        });
        client
            .batch_execute(&format!("CREATE DATABASE {name}"))
            .await
            .unwrap();
        let (prefix, _) = base.rsplit_once('/').unwrap();
        let settings = crate::pg::PgSettings {
            url: format!("{prefix}/{name}"),
            pool_size: 2,
            connect_timeout: std::time::Duration::from_secs(5),
            statement_timeout: std::time::Duration::from_secs(5),
            migrate: crate::pg::Migrate::Auto,
            migrate_timeout: std::time::Duration::from_secs(30),
            binary_version: "0.1.0".into(),
            node_id: "n".into(),
        };
        let store = Store::open_postgres(&settings).await.unwrap();
        exercise(&store).await;
        store.close();
    }
}
