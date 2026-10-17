//! Key anomaly flags and the per-key history detection compares against. A flag is open until
//! dismissed; one key has at most one open flag per kind and subject (a network, a user agent
//! family, a country), enforced by a partial unique index, so nodes that notice the same thing
//! at once add to one flag. History is what each key was seen with (`key_seen`) and how many
//! requests each node counted per hour (`key_hours`, one row per node so counts are never added
//! twice).

use std::collections::HashMap;

use rusqlite::{OptionalExtension, params};

use crate::{Store, StoreError};

/// One flag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnomalyRow {
    pub id: String,
    pub key_uid: String,
    pub key_name: String,
    pub owner: Option<String>,
    pub organization: String,
    pub team: String,
    pub project: String,
    /// `spike`, `network`, `user_agent`, `country` or `exposed`.
    pub kind: String,
    /// What the flag is about within its kind: the network, family or country; `hour` for spikes.
    pub subject: String,
    pub severity: String,
    pub first_us: i64,
    pub last_us: i64,
    pub count: i64,
    /// JSON.
    pub detail: String,
    /// `open` or `dismissed`.
    pub state: String,
    pub dismissed_by: Option<String>,
    pub dismissed_us: Option<i64>,
    pub reason: Option<String>,
}

/// What a flag list selects.
#[derive(Clone, Debug, Default)]
pub struct AnomalyFilter {
    pub key: Option<String>,
    pub owner: Option<String>,
    pub state: Option<String>,
    /// Any of these kinds; every kind when empty.
    pub kinds: Vec<String>,
    /// Only ids before this one (ids sort by creation, newest listed first).
    pub before: Option<String>,
    pub limit: u64,
}

/// What one node saw of a key during an interval, written in one batch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SeenRow {
    pub key_uid: String,
    /// `network`, `user_agent`, `country`, or `first` (the key's first use, subject empty).
    pub kind: String,
    pub subject: String,
    pub at_us: i64,
}

pub(crate) const MIGRATIONS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS key_anomalies (
        id TEXT PRIMARY KEY,
        key_uid TEXT NOT NULL,
        key_name TEXT NOT NULL,
        owner TEXT,
        organization TEXT NOT NULL,
        team TEXT NOT NULL,
        project TEXT NOT NULL,
        kind TEXT NOT NULL,
        subject TEXT NOT NULL,
        severity TEXT NOT NULL,
        first_us INTEGER NOT NULL,
        last_us INTEGER NOT NULL,
        count INTEGER NOT NULL,
        detail TEXT NOT NULL,
        state TEXT NOT NULL,
        dismissed_by TEXT,
        dismissed_us INTEGER,
        reason TEXT)",
    "CREATE UNIQUE INDEX IF NOT EXISTS key_anomalies_open ON key_anomalies (key_uid, kind, subject)
        WHERE state = 'open'",
    "CREATE INDEX IF NOT EXISTS key_anomalies_key ON key_anomalies (key_name, id)",
    "CREATE TABLE IF NOT EXISTS key_seen (
        key_uid TEXT NOT NULL,
        kind TEXT NOT NULL,
        subject TEXT NOT NULL,
        first_us INTEGER NOT NULL,
        last_us INTEGER NOT NULL,
        PRIMARY KEY (key_uid, kind, subject))",
    "CREATE TABLE IF NOT EXISTS key_hours (
        key_uid TEXT NOT NULL,
        hour_us INTEGER NOT NULL,
        node TEXT NOT NULL,
        count INTEGER NOT NULL,
        PRIMARY KEY (key_uid, hour_us, node))",
];

pub(crate) const PG_MIGRATION: &str = "CREATE TABLE IF NOT EXISTS key_anomalies (
        id text PRIMARY KEY,
        key_uid text NOT NULL,
        key_name text NOT NULL,
        owner text,
        organization text NOT NULL,
        team text NOT NULL,
        project text NOT NULL,
        kind text NOT NULL,
        subject text NOT NULL,
        severity text NOT NULL,
        first_us bigint NOT NULL,
        last_us bigint NOT NULL,
        count bigint NOT NULL,
        detail text NOT NULL,
        state text NOT NULL,
        dismissed_by text,
        dismissed_us bigint,
        reason text);
     CREATE UNIQUE INDEX IF NOT EXISTS key_anomalies_open ON key_anomalies (key_uid, kind, subject)
        WHERE state = 'open';
     CREATE INDEX IF NOT EXISTS key_anomalies_key ON key_anomalies (key_name, id);
     CREATE TABLE IF NOT EXISTS key_seen (
        key_uid text NOT NULL,
        kind text NOT NULL,
        subject text NOT NULL,
        first_us bigint NOT NULL,
        last_us bigint NOT NULL,
        PRIMARY KEY (key_uid, kind, subject));
     CREATE TABLE IF NOT EXISTS key_hours (
        key_uid text NOT NULL,
        hour_us bigint NOT NULL,
        node text NOT NULL,
        count bigint NOT NULL,
        PRIMARY KEY (key_uid, hour_us, node));";

const COLUMNS: &str = "id, key_uid, key_name, owner, organization, team, project, kind, subject, severity, \
                       first_us, last_us, count, detail, state, dismissed_by, dismissed_us, reason";

const RAISE: &str =
    "INSERT INTO key_anomalies (id, key_uid, key_name, owner, organization, team, project, kind,
        subject, severity, first_us, last_us, count, detail, state)
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, 'open')
     ON CONFLICT (key_uid, kind, subject) WHERE state = 'open' DO UPDATE SET
        count = key_anomalies.count + excluded.count,
        last_us = CASE WHEN excluded.last_us > key_anomalies.last_us THEN excluded.last_us
                       ELSE key_anomalies.last_us END,
        severity = excluded.severity, detail = excluded.detail";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<AnomalyRow> {
    Ok(AnomalyRow {
        id: r.get(0)?,
        key_uid: r.get(1)?,
        key_name: r.get(2)?,
        owner: r.get(3)?,
        organization: r.get(4)?,
        team: r.get(5)?,
        project: r.get(6)?,
        kind: r.get(7)?,
        subject: r.get(8)?,
        severity: r.get(9)?,
        first_us: r.get(10)?,
        last_us: r.get(11)?,
        count: r.get(12)?,
        detail: r.get(13)?,
        state: r.get(14)?,
        dismissed_by: r.get(15)?,
        dismissed_us: r.get(16)?,
        reason: r.get(17)?,
    })
}

fn pg_row(r: &tokio_postgres::Row) -> Result<AnomalyRow, StoreError> {
    Ok(AnomalyRow {
        id: r.try_get(0)?,
        key_uid: r.try_get(1)?,
        key_name: r.try_get(2)?,
        owner: r.try_get(3)?,
        organization: r.try_get(4)?,
        team: r.try_get(5)?,
        project: r.try_get(6)?,
        kind: r.try_get(7)?,
        subject: r.try_get(8)?,
        severity: r.try_get(9)?,
        first_us: r.try_get(10)?,
        last_us: r.try_get(11)?,
        count: r.try_get(12)?,
        detail: r.try_get(13)?,
        state: r.try_get(14)?,
        dismissed_by: r.try_get(15)?,
        dismissed_us: r.try_get(16)?,
        reason: r.try_get(17)?,
    })
}

fn lite(sql: &str) -> String {
    sql.replace('$', "?")
}

/// The list query's `WHERE` clause and text parameters, numbered from `$2` (`$1` is the limit).
fn conditions(f: &AnomalyFilter) -> (String, Vec<String>) {
    let mut sql = String::from("1 = 1");
    let mut args: Vec<String> = Vec::new();
    for (column, value, op) in [
        ("key_name", &f.key, "="),
        ("owner", &f.owner, "="),
        ("state", &f.state, "="),
        ("id", &f.before, "<"),
    ] {
        if let Some(v) = value {
            args.push(v.clone());
            sql.push_str(&format!(" AND {column} {op} ${}", args.len() + 1));
        }
    }
    if !f.kinds.is_empty() {
        let mut parts = Vec::new();
        for k in &f.kinds {
            args.push(k.clone());
            parts.push(format!("${}", args.len() + 1));
        }
        sql.push_str(&format!(" AND kind IN ({})", parts.join(", ")));
    }
    (sql, args)
}

impl Store {
    /// Opens a flag, or adds `count` and the latest time to the open flag of the same key, kind and
    /// subject. Returns the flag and whether this call opened it.
    pub async fn raise_anomaly(&self, a: AnomalyRow) -> Result<(AnomalyRow, bool), StoreError> {
        let proposed = a.id.clone();
        let (uid, kind, subject) = (a.key_uid.clone(), a.kind.clone(), a.subject.clone());
        let select = format!(
            "SELECT {COLUMNS} FROM key_anomalies WHERE key_uid = $1 AND kind = $2 AND subject = $3 AND state = 'open'"
        );
        let stored = match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    let tx = c.transaction()?;
                    tx.execute(
                        &lite(RAISE),
                        params![
                            a.id,
                            a.key_uid,
                            a.key_name,
                            a.owner,
                            a.organization,
                            a.team,
                            a.project,
                            a.kind,
                            a.subject,
                            a.severity,
                            a.first_us,
                            a.last_us,
                            a.count,
                            a.detail
                        ],
                    )?;
                    let got = tx
                        .query_row(&lite(&select), params![uid, kind, subject], row)
                        .optional()?;
                    tx.commit()?;
                    Ok(got)
                })
                .await?
            }
            Store::Postgres(p) => {
                p.execute(
                    RAISE,
                    &[
                        &a.id,
                        &a.key_uid,
                        &a.key_name,
                        &a.owner,
                        &a.organization,
                        &a.team,
                        &a.project,
                        &a.kind,
                        &a.subject,
                        &a.severity,
                        &a.first_us,
                        &a.last_us,
                        &a.count,
                        &a.detail,
                    ],
                )
                .await?;
                p.rows(&select, &[&uid, &kind, &subject])
                    .await?
                    .first()
                    .map(pg_row)
                    .transpose()?
            }
        };
        let stored = stored.ok_or(StoreError::Conflict)?;
        let opened = stored.id == proposed;
        Ok((stored, opened))
    }

    /// Flags matching `f`, newest first.
    pub async fn anomalies(&self, f: &AnomalyFilter) -> Result<Vec<AnomalyRow>, StoreError> {
        let (cond, args) = conditions(f);
        let sql = format!("SELECT {COLUMNS} FROM key_anomalies WHERE {cond} ORDER BY id DESC LIMIT $1");
        let limit = i64::try_from(f.limit).unwrap_or(i64::MAX);
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    let mut values: Vec<rusqlite::types::Value> = vec![limit.into()];
                    values.extend(args.into_iter().map(rusqlite::types::Value::Text));
                    let mut stmt = c.prepare(&lite(&sql))?;
                    let rows = stmt.query_map(rusqlite::params_from_iter(values), row)?;
                    Ok(rows.collect::<Result<Vec<_>, _>>()?)
                })
                .await
            }
            Store::Postgres(p) => {
                let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = vec![&limit];
                params.extend(
                    args.iter()
                        .map(|a| a as &(dyn tokio_postgres::types::ToSql + Sync)),
                );
                p.rows(&sql, &params).await?.iter().map(pg_row).collect()
            }
        }
    }

    /// One flag.
    pub async fn anomaly(&self, id: &str) -> Result<Option<AnomalyRow>, StoreError> {
        let id = id.to_owned();
        let sql = format!("SELECT {COLUMNS} FROM key_anomalies WHERE id = $1");
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| Ok(c.query_row(&lite(&sql), params![id], row).optional()?))
                    .await
            }
            Store::Postgres(p) => p.rows(&sql, &[&id]).await?.first().map(pg_row).transpose(),
        }
    }

    /// Dismisses an open flag; `false` when it is not open.
    pub async fn dismiss_anomaly(
        &self,
        id: &str,
        by: &str,
        reason: Option<&str>,
        at_us: i64,
    ) -> Result<bool, StoreError> {
        let (id, by, reason) = (id.to_owned(), by.to_owned(), reason.map(str::to_owned));
        let sql = "UPDATE key_anomalies SET state = 'dismissed', dismissed_by = $2, dismissed_us = $3,
                   reason = $4 WHERE id = $1 AND state = 'open'";
        let changed = match self {
            Store::Sqlite(s) => {
                s.call(move |c| Ok(c.execute(&lite(sql), params![id, by, at_us, reason])?))
                    .await?
            }
            Store::Postgres(p) => {
                let n = p.execute(sql, &[&id, &by, &at_us, &reason]).await?;
                usize::try_from(n).unwrap_or(usize::MAX)
            }
        };
        Ok(changed > 0)
    }

    /// The number of open flags of each key, by key name.
    pub async fn open_anomaly_counts(&self) -> Result<HashMap<String, u64>, StoreError> {
        const SQL: &str =
            "SELECT key_name, count(*) FROM key_anomalies WHERE state = 'open' GROUP BY key_name";
        let rows: Vec<(String, i64)> = match self {
            Store::Sqlite(s) => {
                s.call(|c| {
                    let mut stmt = c.prepare(SQL)?;
                    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
                    Ok(rows.collect::<Result<Vec<_>, _>>()?)
                })
                .await?
            }
            Store::Postgres(p) => p
                .rows(SQL, &[])
                .await?
                .iter()
                .map(|r| Ok((r.try_get(0)?, r.try_get(1)?)))
                .collect::<Result<_, StoreError>>()?,
        };
        Ok(rows
            .into_iter()
            .map(|(k, n)| (k, u64::try_from(n).unwrap_or(0)))
            .collect())
    }

    /// Adds what a node saw: each subject's first and latest time, keeping the earliest first and the
    /// latest last.
    pub async fn put_key_seen(&self, rows: Vec<SeenRow>) -> Result<(), StoreError> {
        if rows.is_empty() {
            return Ok(());
        }
        let sql = "INSERT INTO key_seen (key_uid, kind, subject, first_us, last_us) VALUES ($1, $2, $3, $4, $4)
                   ON CONFLICT (key_uid, kind, subject) DO UPDATE SET
                     last_us = CASE WHEN excluded.last_us > key_seen.last_us THEN excluded.last_us ELSE key_seen.last_us END,
                     first_us = CASE WHEN excluded.first_us < key_seen.first_us THEN excluded.first_us ELSE key_seen.first_us END";
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    let tx = c.transaction()?;
                    {
                        let mut stmt = tx.prepare(&lite(sql))?;
                        for r in &rows {
                            stmt.execute(params![r.key_uid, r.kind, r.subject, r.at_us])?;
                        }
                    }
                    tx.commit()?;
                    Ok(())
                })
                .await
            }
            Store::Postgres(p) => {
                for r in &rows {
                    p.execute(sql, &[&r.key_uid, &r.kind, &r.subject, &r.at_us])
                        .await?;
                }
                Ok(())
            }
        }
    }

    /// What a key was seen with: `(kind, subject, first_us, last_us)`.
    pub async fn key_seen(&self, key_uid: &str) -> Result<Vec<(String, String, i64, i64)>, StoreError> {
        let uid = key_uid.to_owned();
        let sql = "SELECT kind, subject, first_us, last_us FROM key_seen WHERE key_uid = $1";
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    let mut stmt = c.prepare(&lite(sql))?;
                    let rows =
                        stmt.query_map(params![uid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
                    Ok(rows.collect::<Result<Vec<_>, _>>()?)
                })
                .await
            }
            Store::Postgres(p) => p
                .rows(sql, &[&uid])
                .await?
                .iter()
                .map(|r| Ok((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?, r.try_get(3)?)))
                .collect(),
        }
    }

    /// Sets this node's request count of keys for hours: `(key_uid, hour_us, count)`.
    pub async fn put_key_hours(&self, node: &str, rows: Vec<(String, i64, i64)>) -> Result<(), StoreError> {
        if rows.is_empty() {
            return Ok(());
        }
        let node = node.to_owned();
        let sql = "INSERT INTO key_hours (key_uid, hour_us, node, count) VALUES ($1, $2, $3, $4)
                   ON CONFLICT (key_uid, hour_us, node) DO UPDATE SET count = excluded.count";
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    let tx = c.transaction()?;
                    {
                        let mut stmt = tx.prepare(&lite(sql))?;
                        for (uid, hour, count) in &rows {
                            stmt.execute(params![uid, hour, node, count])?;
                        }
                    }
                    tx.commit()?;
                    Ok(())
                })
                .await
            }
            Store::Postgres(p) => {
                for (uid, hour, count) in &rows {
                    p.execute(sql, &[uid, hour, &node, count]).await?;
                }
                Ok(())
            }
        }
    }

    /// A key's request count per hour from `from_us` on, summed over every node.
    pub async fn key_hours(&self, key_uid: &str, from_us: i64) -> Result<HashMap<i64, i64>, StoreError> {
        let uid = key_uid.to_owned();
        let sql =
            "SELECT hour_us, sum(count) FROM key_hours WHERE key_uid = $1 AND hour_us >= $2 GROUP BY hour_us";
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    let mut stmt = c.prepare(&lite(sql))?;
                    let rows = stmt.query_map(params![uid, from_us], |r| Ok((r.get(0)?, r.get(1)?)))?;
                    Ok(rows.collect::<Result<HashMap<_, _>, _>>()?)
                })
                .await
            }
            Store::Postgres(p) => p
                .rows(
                    "SELECT hour_us, sum(count)::int8 FROM key_hours WHERE key_uid = $1 AND hour_us >= $2 GROUP BY hour_us",
                    &[&uid, &from_us],
                )
                .await?
                .iter()
                .map(|r| Ok((r.try_get(0)?, r.try_get(1)?)))
                .collect(),
        }
    }

    /// Drops history older than `before_us`.
    pub async fn prune_key_history(&self, before_us: i64) -> Result<(), StoreError> {
        let stmts = [
            "DELETE FROM key_hours WHERE hour_us < $1",
            "DELETE FROM key_seen WHERE last_us < $1 AND kind <> 'first'",
        ];
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    for sql in stmts {
                        c.execute(&lite(sql), params![before_us])?;
                    }
                    Ok(())
                })
                .await
            }
            Store::Postgres(p) => {
                for sql in stmts {
                    p.execute(sql, &[&before_us]).await?;
                }
                Ok(())
            }
        }
    }

    /// Forgets every flag and all key history.
    pub async fn clear_anomalies(&self) -> Result<(), StoreError> {
        let stmts = [
            "DELETE FROM key_anomalies",
            "DELETE FROM key_seen",
            "DELETE FROM key_hours",
        ];
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    for sql in stmts {
                        c.execute(sql, [])?;
                    }
                    Ok(())
                })
                .await
            }
            Store::Postgres(p) => {
                for sql in stmts {
                    p.execute(sql, &[]).await?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::pg::{Migrate, PgSettings};

    fn flag(id: &str, subject: &str) -> AnomalyRow {
        AnomalyRow {
            id: id.into(),
            key_uid: "u1".into(),
            key_name: "k".into(),
            owner: Some("alice".into()),
            organization: "o".into(),
            team: "t".into(),
            project: "p".into(),
            kind: "network".into(),
            subject: subject.into(),
            severity: "warning".into(),
            first_us: 10,
            last_us: 10,
            count: 1,
            detail: "{}".into(),
            state: "open".into(),
            dismissed_by: None,
            dismissed_us: None,
            reason: None,
        }
    }

    fn embedded(name: &str) -> Store {
        let dir = std::env::temp_dir().join(format!(
            "antirion-store-{name}-{:x}",
            antirion_core::ResourceUid::random().0
        ));
        Store::open_embedded(&dir).unwrap()
    }

    /// A store on a fresh database of `ANTIRION_TEST_POSTGRES_URL`, when it is set.
    async fn postgres() -> Option<Store> {
        let base = std::env::var("ANTIRION_TEST_POSTGRES_URL").ok()?;
        let name = format!("t_{:x}", antirion_core::ResourceUid::random().0);
        let (client, conn) = tokio_postgres::connect(&base, tokio_postgres::NoTls).await.ok()?;
        let mut set = tokio::task::JoinSet::new();
        set.spawn(async move {
            let _ = conn.await;
        });
        client
            .batch_execute(&format!("CREATE DATABASE {name}"))
            .await
            .ok()?;
        let (prefix, _) = base.rsplit_once('/')?;
        let settings = PgSettings {
            url: format!("{prefix}/{name}"),
            pool_size: 2,
            connect_timeout: Duration::from_secs(5),
            statement_timeout: Duration::from_secs(5),
            migrate: Migrate::Auto,
            migrate_timeout: Duration::from_secs(30),
            binary_version: "0.1.0".into(),
            node_id: "n".into(),
        };
        Some(Store::open_postgres(&settings).await.unwrap())
    }

    async fn flags_behave(store: &Store) {
        let (a, opened) = store.raise_anomaly(flag("a1", "10.0.0.0/24")).await.unwrap();
        assert!(opened && a.count == 1);
        let mut again = flag("a2", "10.0.0.0/24");
        again.last_us = 20;
        again.count = 3;
        let (a, opened) = store.raise_anomaly(again).await.unwrap();
        assert!(!opened);
        assert_eq!((a.id.as_str(), a.count, a.last_us), ("a1", 4, 20));
        store.raise_anomaly(flag("a3", "10.0.1.0/24")).await.unwrap();
        assert_eq!(store.open_anomaly_counts().await.unwrap().get("k"), Some(&2));
        assert!(
            store
                .dismiss_anomaly("a1", "bob", Some("known"), 30)
                .await
                .unwrap()
        );
        assert!(!store.dismiss_anomaly("a1", "bob", None, 31).await.unwrap());
        let (a, opened) = store.raise_anomaly(flag("a4", "10.0.0.0/24")).await.unwrap();
        assert!(
            opened && a.id == "a4",
            "a dismissed flag never takes new occurrences"
        );
        let open = store
            .anomalies(&AnomalyFilter {
                state: Some("open".into()),
                limit: 10,
                ..AnomalyFilter::default()
            })
            .await
            .unwrap();
        assert_eq!(
            open.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            vec!["a4", "a3"]
        );
        let page = store
            .anomalies(&AnomalyFilter {
                before: Some("a4".into()),
                kinds: vec!["network".into()],
                owner: Some("alice".into()),
                limit: 1,
                ..AnomalyFilter::default()
            })
            .await
            .unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].id, "a3");
        assert_eq!(store.anomaly("a1").await.unwrap().unwrap().state, "dismissed");
    }

    async fn history_behaves(store: &Store) {
        let seen = |at| SeenRow {
            key_uid: "u".into(),
            kind: "network".into(),
            subject: "n".into(),
            at_us: at,
        };
        store.put_key_seen(vec![seen(5), seen(9), seen(2)]).await.unwrap();
        assert_eq!(
            store.key_seen("u").await.unwrap(),
            vec![("network".into(), "n".into(), 2, 9)]
        );
        store
            .put_key_hours("a", vec![("u".into(), 100, 3), ("u".into(), 200, 1)])
            .await
            .unwrap();
        store
            .put_key_hours("b", vec![("u".into(), 100, 2)])
            .await
            .unwrap();
        store
            .put_key_hours("a", vec![("u".into(), 100, 4)])
            .await
            .unwrap();
        let hours = store.key_hours("u", 0).await.unwrap();
        assert_eq!(hours.get(&100), Some(&6));
        store.prune_key_history(150).await.unwrap();
        assert!(!store.key_hours("u", 0).await.unwrap().contains_key(&100));
        assert!(store.key_seen("u").await.unwrap().is_empty());
        store.clear_anomalies().await.unwrap();
    }

    async fn marks_behave(store: &Store) {
        store
            .put_use_marks("key", vec![("a".into(), 10), ("b".into(), 5)])
            .await
            .unwrap();
        store
            .put_use_marks("key", vec![("a".into(), 7), ("b".into(), 9), ("b".into(), 8)])
            .await
            .unwrap();
        let marks = store.use_marks("key").await.unwrap();
        assert_eq!(marks.get("a"), Some(&10));
        assert_eq!(marks.get("b"), Some(&9));
        assert!(store.use_marks("user").await.unwrap().is_empty());
        assert!(store.user_activity().await.unwrap().is_empty());
        store.clear_use_marks().await.unwrap();
        assert!(store.use_marks("key").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn flags_history_and_marks_on_the_embedded_store() {
        let store = embedded("people");
        flags_behave(&store).await;
        history_behaves(&store).await;
        marks_behave(&store).await;
    }

    #[tokio::test]
    async fn flags_history_and_marks_on_postgres() {
        let Some(store) = postgres().await else {
            return;
        };
        flags_behave(&store).await;
        history_behaves(&store).await;
        marks_behave(&store).await;
    }
}
