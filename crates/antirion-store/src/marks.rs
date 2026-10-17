//! When things were last used, merged across nodes: each node writes the times it saw (batched, off
//! the request path) and a row only ever moves forward. Rows are `(kind, id)`: kind `key` with the
//! key's incarnation id. Also the activity of admin users read from their sessions and tokens.

use std::collections::HashMap;

use rusqlite::params;

use crate::{Store, StoreError};

pub(crate) const MIGRATIONS: &[&str] = &["CREATE TABLE IF NOT EXISTS use_marks (
        kind TEXT NOT NULL,
        id TEXT NOT NULL,
        at_us INTEGER NOT NULL,
        PRIMARY KEY (kind, id))"];

pub(crate) const PG_MIGRATION: &str = "CREATE TABLE IF NOT EXISTS use_marks (
        kind text NOT NULL,
        id text NOT NULL,
        at_us bigint NOT NULL,
        PRIMARY KEY (kind, id));";

impl Store {
    /// Records that each `(id, at_us)` of `kind` was used at that time, keeping the later of the
    /// stored and the given time.
    pub async fn put_use_marks(&self, kind: &str, marks: Vec<(String, i64)>) -> Result<(), StoreError> {
        if marks.is_empty() {
            return Ok(());
        }
        let kind = kind.to_owned();
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    let tx = c.transaction()?;
                    {
                        let mut stmt = tx.prepare(
                            "INSERT INTO use_marks (kind, id, at_us) VALUES (?1, ?2, ?3)
                             ON CONFLICT (kind, id) DO UPDATE SET at_us = max(at_us, excluded.at_us)",
                        )?;
                        for (id, at) in &marks {
                            stmt.execute(params![kind, id, at])?;
                        }
                    }
                    tx.commit()?;
                    Ok(())
                })
                .await
            }
            Store::Postgres(p) => {
                let (ids, ats): (Vec<String>, Vec<i64>) = marks.into_iter().unzip();
                p.execute(
                    "INSERT INTO use_marks (kind, id, at_us)
                     SELECT $1, r.id, max(r.at_us) FROM unnest($2::text[], $3::int8[]) AS r(id, at_us) GROUP BY r.id
                     ON CONFLICT (kind, id) DO UPDATE SET at_us = GREATEST(use_marks.at_us, excluded.at_us)",
                    &[&kind, &ids, &ats],
                )
                .await
                .map(|_| ())
            }
        }
    }

    /// Every stored mark of `kind`, by id.
    pub async fn use_marks(&self, kind: &str) -> Result<HashMap<String, i64>, StoreError> {
        let kind = kind.to_owned();
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    let mut stmt = c.prepare("SELECT id, at_us FROM use_marks WHERE kind = ?1")?;
                    let rows = stmt.query_map(params![kind], |r| Ok((r.get(0)?, r.get(1)?)))?;
                    Ok(rows.collect::<Result<HashMap<_, _>, _>>()?)
                })
                .await
            }
            Store::Postgres(p) => p
                .rows("SELECT id, at_us FROM use_marks WHERE kind = $1", &[&kind])
                .await?
                .iter()
                .map(|r| Ok((r.try_get(0)?, r.try_get(1)?)))
                .collect(),
        }
    }

    /// Forgets every mark.
    pub async fn clear_use_marks(&self) -> Result<(), StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(|c| {
                    c.execute("DELETE FROM use_marks", [])?;
                    Ok(())
                })
                .await
            }
            Store::Postgres(p) => p.execute("DELETE FROM use_marks", &[]).await.map(|_| ()),
        }
    }

    /// The latest use of any session or admin token of each user that has one.
    pub async fn user_activity(&self) -> Result<HashMap<String, i64>, StoreError> {
        const SQL: &str = "SELECT user_name, max(at) FROM (
                SELECT user_name, last_used_us AS at FROM admin_sessions WHERE user_name IS NOT NULL
                UNION ALL
                SELECT user_name, last_used_us AS at FROM admin_tokens WHERE last_used_us IS NOT NULL
            ) AS used GROUP BY user_name";
        match self {
            Store::Sqlite(s) => {
                s.call(|c| {
                    let mut stmt = c.prepare(SQL)?;
                    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
                    Ok(rows.collect::<Result<HashMap<_, _>, _>>()?)
                })
                .await
            }
            Store::Postgres(p) => p
                .rows(SQL, &[])
                .await?
                .iter()
                .map(|r| Ok((r.try_get(0)?, r.try_get(1)?)))
                .collect(),
        }
    }
}
