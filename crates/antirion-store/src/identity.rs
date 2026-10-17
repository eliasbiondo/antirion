//! Per-user identity state that is not configuration: local passwords, second factors, recovery
//! codes, sign-in challenges, single-use email tokens, invitations and preferences. Each record is a
//! JSON body under `(kind, id)`, owned by one user so that deleting the user can delete them all.
//! Single-use records are taken atomically (deleted and returned in one statement), so two nodes
//! can never both use one.

use rusqlite::{OptionalExtension, params};

use crate::{Store, StoreError};

/// One record of a user's identity state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityRecord {
    pub kind: String,
    pub id: String,
    pub user: String,
    /// When it stops being valid; the owner checks it against its clock.
    pub expires_us: Option<i64>,
    pub body: String,
}

pub(crate) const MIGRATIONS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS identity_records (
        kind TEXT NOT NULL,
        id TEXT NOT NULL,
        user_name TEXT NOT NULL,
        expires_us INTEGER,
        body TEXT NOT NULL,
        PRIMARY KEY (kind, id))",
    "CREATE INDEX IF NOT EXISTS identity_records_user ON identity_records (user_name, kind)",
];

const COLUMNS: &str = "kind, id, user_name, expires_us, body";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<IdentityRecord> {
    Ok(IdentityRecord {
        kind: r.get(0)?,
        id: r.get(1)?,
        user: r.get(2)?,
        expires_us: r.get(3)?,
        body: r.get(4)?,
    })
}

fn pg_row(r: &tokio_postgres::Row) -> Result<IdentityRecord, StoreError> {
    Ok(IdentityRecord {
        kind: r.try_get(0)?,
        id: r.try_get(1)?,
        user: r.try_get(2)?,
        expires_us: r.try_get(3)?,
        body: r.try_get(4)?,
    })
}

impl Store {
    /// Writes a record, replacing one with the same kind and id.
    pub async fn put_record(&self, rec: IdentityRecord) -> Result<(), StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    c.execute(
                        "INSERT INTO identity_records (kind, id, user_name, expires_us, body) VALUES (?1, ?2, ?3, ?4, ?5)
                         ON CONFLICT (kind, id) DO UPDATE SET user_name = excluded.user_name,
                            expires_us = excluded.expires_us, body = excluded.body",
                        params![rec.kind, rec.id, rec.user, rec.expires_us, rec.body],
                    )?;
                    Ok(())
                })
                .await
            }
            Store::Postgres(p) => p
                .execute(
                    "INSERT INTO identity_records (kind, id, user_name, expires_us, body) VALUES ($1, $2, $3, $4, $5)
                     ON CONFLICT (kind, id) DO UPDATE SET user_name = excluded.user_name,
                        expires_us = excluded.expires_us, body = excluded.body",
                    &[&rec.kind, &rec.id, &rec.user, &rec.expires_us, &rec.body],
                )
                .await
                .map(|_| ()),
        }
    }

    pub async fn record(&self, kind: &str, id: &str) -> Result<Option<IdentityRecord>, StoreError> {
        let (kind, id) = (kind.to_owned(), id.to_owned());
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    Ok(c.query_row(
                        &format!("SELECT {COLUMNS} FROM identity_records WHERE kind = ?1 AND id = ?2"),
                        params![kind, id],
                        row,
                    )
                    .optional()?)
                })
                .await
            }
            Store::Postgres(p) => p
                .rows(
                    &format!("SELECT {COLUMNS} FROM identity_records WHERE kind = $1 AND id = $2"),
                    &[&kind, &id],
                )
                .await?
                .first()
                .map(pg_row)
                .transpose(),
        }
    }

    /// Deletes a record and returns it, at most once across every node.
    pub async fn take_record(&self, kind: &str, id: &str) -> Result<Option<IdentityRecord>, StoreError> {
        let (kind, id) = (kind.to_owned(), id.to_owned());
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    Ok(c.query_row(
                        &format!(
                            "DELETE FROM identity_records WHERE kind = ?1 AND id = ?2 RETURNING {COLUMNS}"
                        ),
                        params![kind, id],
                        row,
                    )
                    .optional()?)
                })
                .await
            }
            Store::Postgres(p) => p
                .rows(
                    &format!("DELETE FROM identity_records WHERE kind = $1 AND id = $2 RETURNING {COLUMNS}"),
                    &[&kind, &id],
                )
                .await?
                .first()
                .map(pg_row)
                .transpose(),
        }
    }

    /// Replaces a record's body only while it still is `old`; true when it was replaced.
    pub async fn swap_record(&self, kind: &str, id: &str, old: &str, new: &str) -> Result<bool, StoreError> {
        let (kind, id, old, new) = (kind.to_owned(), id.to_owned(), old.to_owned(), new.to_owned());
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    Ok(c.execute(
                        "UPDATE identity_records SET body = ?4 WHERE kind = ?1 AND id = ?2 AND body = ?3",
                        params![kind, id, old, new],
                    )? == 1)
                })
                .await
            }
            Store::Postgres(p) => p
                .execute(
                    "UPDATE identity_records SET body = $4 WHERE kind = $1 AND id = $2 AND body = $3",
                    &[&kind, &id, &old, &new],
                )
                .await
                .map(|n| n == 1),
        }
    }

    /// The records of one kind, of one user when `user` is set, oldest id first.
    pub async fn records(&self, kind: &str, user: Option<&str>) -> Result<Vec<IdentityRecord>, StoreError> {
        let (kind, user) = (kind.to_owned(), user.map(str::to_owned));
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    let mut stmt = c.prepare(&format!(
                        "SELECT {COLUMNS} FROM identity_records
                         WHERE kind = ?1 AND (?2 IS NULL OR user_name = ?2) ORDER BY id"
                    ))?;
                    let rows = stmt.query_map(params![kind, user], row)?;
                    Ok(rows.collect::<Result<Vec<_>, _>>()?)
                })
                .await
            }
            Store::Postgres(p) => p
                .rows(
                    &format!(
                        "SELECT {COLUMNS} FROM identity_records
                         WHERE kind = $1 AND ($2::text IS NULL OR user_name = $2) ORDER BY id"
                    ),
                    &[&kind, &user],
                )
                .await?
                .iter()
                .map(pg_row)
                .collect(),
        }
    }

    /// Deletes one record; true when it existed.
    pub async fn delete_record(&self, kind: &str, id: &str) -> Result<bool, StoreError> {
        Ok(self.take_record(kind, id).await?.is_some())
    }

    /// Deletes a user's records of these kinds (every kind when `kinds` is empty); returns how many.
    pub async fn delete_records_of(&self, user: &str, kinds: &[&str]) -> Result<u64, StoreError> {
        let user = user.to_owned();
        let kinds: Vec<String> = kinds.iter().map(|k| (*k).to_owned()).collect();
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    let n = if kinds.is_empty() {
                        c.execute("DELETE FROM identity_records WHERE user_name = ?1", [&user])?
                    } else {
                        let mut n = 0;
                        for k in &kinds {
                            n += c.execute(
                                "DELETE FROM identity_records WHERE user_name = ?1 AND kind = ?2",
                                params![user, k],
                            )?;
                        }
                        n
                    };
                    Ok(u64::try_from(n).unwrap_or(0))
                })
                .await
            }
            Store::Postgres(p) => {
                if kinds.is_empty() {
                    p.execute("DELETE FROM identity_records WHERE user_name = $1", &[&user])
                        .await
                } else {
                    p.execute(
                        "DELETE FROM identity_records WHERE user_name = $1 AND kind = ANY($2)",
                        &[&user, &kinds],
                    )
                    .await
                }
            }
        }
    }

    /// Records a use of an admin token.
    pub async fn touch_admin_token(&self, kid: String, now_us: i64) -> Result<(), StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    c.execute(
                        "UPDATE admin_tokens SET last_used_us = ?2 WHERE kid = ?1",
                        params![kid, now_us],
                    )?;
                    Ok(())
                })
                .await
            }
            Store::Postgres(p) => p
                .execute(
                    "UPDATE admin_tokens SET last_used_us = $2 WHERE kid = $1",
                    &[&kid, &now_us],
                )
                .await
                .map(|_| ()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(kind: &str, id: &str, user: &str, body: &str) -> IdentityRecord {
        IdentityRecord {
            kind: kind.into(),
            id: id.into(),
            user: user.into(),
            expires_us: Some(10),
            body: body.into(),
        }
    }

    #[tokio::test]
    async fn records_are_kept_taken_once_and_swapped() {
        let dir = std::env::temp_dir().join(format!(
            "antirion-store-identity-{:x}",
            antirion_core::ResourceUid::random().0
        ));
        let store = Store::open_embedded(&dir).unwrap();
        store
            .put_record(rec("factor", "f1", "alice", "{}"))
            .await
            .unwrap();
        store
            .put_record(rec("factor", "f2", "alice", "{}"))
            .await
            .unwrap();
        store
            .put_record(rec("challenge", "c1", "alice", "{}"))
            .await
            .unwrap();
        store.put_record(rec("factor", "f3", "bob", "{}")).await.unwrap();
        assert_eq!(store.records("factor", Some("alice")).await.unwrap().len(), 2);
        assert_eq!(store.records("factor", None).await.unwrap().len(), 3);
        assert_eq!(
            store.record("challenge", "c1").await.unwrap(),
            Some(rec("challenge", "c1", "alice", "{}"))
        );
        assert!(store.take_record("challenge", "c1").await.unwrap().is_some());
        assert!(store.take_record("challenge", "c1").await.unwrap().is_none());
        assert!(
            store
                .swap_record("factor", "f1", "{}", "{\"a\":1}")
                .await
                .unwrap()
        );
        assert!(
            !store
                .swap_record("factor", "f1", "{}", "{\"a\":2}")
                .await
                .unwrap()
        );
        assert_eq!(store.delete_records_of("alice", &["factor"]).await.unwrap(), 2);
        assert_eq!(store.delete_records_of("bob", &[]).await.unwrap(), 1);
        store.close();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
