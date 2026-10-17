//! Provider-scoped objects the gateway names with its own ids: uploaded files and batches. Each
//! row maps a gateway id to the project that owns it, the provider that holds it and that
//! provider's id, with a JSON body of what the gateway keeps about the object.

use rusqlite::{OptionalExtension, params};

use crate::{Store, StoreError, to_i64};

/// One mapped object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpstreamObject {
    /// `file` or `batch`.
    pub kind: String,
    /// The gateway id.
    pub id: String,
    pub project: String,
    pub provider: String,
    pub upstream_id: String,
    pub created_us: i64,
    pub body: String,
}

pub(crate) const MIGRATIONS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS upstream_objects (
        kind TEXT NOT NULL,
        id TEXT NOT NULL,
        project TEXT NOT NULL,
        provider TEXT NOT NULL,
        upstream_id TEXT NOT NULL,
        created_us INTEGER NOT NULL,
        body TEXT NOT NULL,
        PRIMARY KEY (kind, id))",
    "CREATE INDEX IF NOT EXISTS upstream_objects_project ON upstream_objects (kind, project, created_us)",
    "CREATE INDEX IF NOT EXISTS upstream_objects_upstream ON upstream_objects (kind, provider, upstream_id)",
];

pub(crate) const PG_MIGRATION: &str = "CREATE TABLE IF NOT EXISTS upstream_objects (
        kind text NOT NULL,
        id text NOT NULL,
        project text NOT NULL,
        provider text NOT NULL,
        upstream_id text NOT NULL,
        created_us bigint NOT NULL,
        body text NOT NULL,
        PRIMARY KEY (kind, id));
     CREATE INDEX IF NOT EXISTS upstream_objects_project ON upstream_objects (kind, project, created_us);
     CREATE INDEX IF NOT EXISTS upstream_objects_upstream ON upstream_objects (kind, provider, upstream_id);";

const COLUMNS: &str = "kind, id, project, provider, upstream_id, created_us, body";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<UpstreamObject> {
    Ok(UpstreamObject {
        kind: r.get(0)?,
        id: r.get(1)?,
        project: r.get(2)?,
        provider: r.get(3)?,
        upstream_id: r.get(4)?,
        created_us: r.get(5)?,
        body: r.get(6)?,
    })
}

fn pg_row(r: &tokio_postgres::Row) -> Result<UpstreamObject, StoreError> {
    Ok(UpstreamObject {
        kind: r.try_get(0)?,
        id: r.try_get(1)?,
        project: r.try_get(2)?,
        provider: r.try_get(3)?,
        upstream_id: r.try_get(4)?,
        created_us: r.try_get(5)?,
        body: r.try_get(6)?,
    })
}

impl Store {
    /// Writes an object, replacing one with the same kind and id.
    pub async fn put_object(&self, o: UpstreamObject) -> Result<(), StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    c.execute(
                        &format!(
                            "INSERT INTO upstream_objects ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                             ON CONFLICT (kind, id) DO UPDATE SET body = excluded.body"
                        ),
                        params![
                            o.kind,
                            o.id,
                            o.project,
                            o.provider,
                            o.upstream_id,
                            o.created_us,
                            o.body
                        ],
                    )?;
                    Ok(())
                })
                .await
            }
            Store::Postgres(p) => p
                .execute(
                    &format!(
                        "INSERT INTO upstream_objects ({COLUMNS}) VALUES ($1, $2, $3, $4, $5, $6, $7)
                         ON CONFLICT (kind, id) DO UPDATE SET body = excluded.body"
                    ),
                    &[
                        &o.kind,
                        &o.id,
                        &o.project,
                        &o.provider,
                        &o.upstream_id,
                        &o.created_us,
                        &o.body,
                    ],
                )
                .await
                .map(|_| ()),
        }
    }

    pub async fn object(&self, kind: &str, id: &str) -> Result<Option<UpstreamObject>, StoreError> {
        let (kind, id) = (kind.to_owned(), id.to_owned());
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    Ok(c.query_row(
                        &format!("SELECT {COLUMNS} FROM upstream_objects WHERE kind = ?1 AND id = ?2"),
                        params![kind, id],
                        row,
                    )
                    .optional()?)
                })
                .await
            }
            Store::Postgres(p) => p
                .rows(
                    &format!("SELECT {COLUMNS} FROM upstream_objects WHERE kind = $1 AND id = $2"),
                    &[&kind, &id],
                )
                .await?
                .first()
                .map(pg_row)
                .transpose(),
        }
    }

    /// The object a provider's own id maps to.
    pub async fn object_by_upstream(
        &self,
        kind: &str,
        provider: &str,
        upstream_id: &str,
    ) -> Result<Option<UpstreamObject>, StoreError> {
        let (kind, provider, upstream_id) = (kind.to_owned(), provider.to_owned(), upstream_id.to_owned());
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    Ok(c.query_row(
                        &format!(
                            "SELECT {COLUMNS} FROM upstream_objects
                             WHERE kind = ?1 AND provider = ?2 AND upstream_id = ?3 ORDER BY created_us LIMIT 1"
                        ),
                        params![kind, provider, upstream_id],
                        row,
                    )
                    .optional()?)
                })
                .await
            }
            Store::Postgres(p) => p
                .rows(
                    &format!(
                        "SELECT {COLUMNS} FROM upstream_objects
                         WHERE kind = $1 AND provider = $2 AND upstream_id = $3 ORDER BY created_us LIMIT 1"
                    ),
                    &[&kind, &provider, &upstream_id],
                )
                .await?
                .first()
                .map(pg_row)
                .transpose(),
        }
    }

    /// A project's objects of a kind, newest first, created before `before_us` when given.
    pub async fn objects(
        &self,
        kind: &str,
        project: &str,
        before_us: Option<i64>,
        limit: u64,
    ) -> Result<Vec<UpstreamObject>, StoreError> {
        let (kind, project) = (kind.to_owned(), project.to_owned());
        let before = before_us.unwrap_or(i64::MAX);
        let limit = to_i64(limit);
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    let mut st = c.prepare(&format!(
                        "SELECT {COLUMNS} FROM upstream_objects WHERE kind = ?1 AND project = ?2 AND created_us < ?3
                         ORDER BY created_us DESC, id DESC LIMIT ?4"
                    ))?;
                    let rows = st.query_map(params![kind, project, before, limit], row)?;
                    Ok(rows.collect::<Result<Vec<_>, _>>()?)
                })
                .await
            }
            Store::Postgres(p) => p
                .rows(
                    &format!(
                        "SELECT {COLUMNS} FROM upstream_objects WHERE kind = $1 AND project = $2 AND created_us < $3
                         ORDER BY created_us DESC, id DESC LIMIT $4"
                    ),
                    &[&kind, &project, &before, &limit],
                )
                .await?
                .iter()
                .map(pg_row)
                .collect(),
        }
    }

    pub async fn delete_object(&self, kind: &str, id: &str) -> Result<bool, StoreError> {
        let (kind, id) = (kind.to_owned(), id.to_owned());
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    Ok(c.execute(
                        "DELETE FROM upstream_objects WHERE kind = ?1 AND id = ?2",
                        params![kind, id],
                    )? > 0)
                })
                .await
            }
            Store::Postgres(p) => p
                .execute(
                    "DELETE FROM upstream_objects WHERE kind = $1 AND id = $2",
                    &[&kind, &id],
                )
                .await
                .map(|n| n > 0),
        }
    }

    /// Forgets every mapped object (test mode).
    pub async fn clear_objects(&self) -> Result<(), StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(|c| {
                    c.execute("DELETE FROM upstream_objects", [])?;
                    Ok(())
                })
                .await
            }
            Store::Postgres(p) => p.execute("DELETE FROM upstream_objects", &[]).await.map(|_| ()),
        }
    }

    /// Replaces an object's body only if it is still `old`: one writer wins across nodes.
    pub async fn swap_object(&self, kind: &str, id: &str, old: &str, new: &str) -> Result<bool, StoreError> {
        let (kind, id, old, new) = (kind.to_owned(), id.to_owned(), old.to_owned(), new.to_owned());
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    Ok(c.execute(
                        "UPDATE upstream_objects SET body = ?4 WHERE kind = ?1 AND id = ?2 AND body = ?3",
                        params![kind, id, old, new],
                    )? > 0)
                })
                .await
            }
            Store::Postgres(p) => p
                .execute(
                    "UPDATE upstream_objects SET body = $4 WHERE kind = $1 AND id = $2 AND body = $3",
                    &[&kind, &id, &old, &new],
                )
                .await
                .map(|n| n > 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(kind: &str, id: &str, project: &str, up: &str, at: i64) -> UpstreamObject {
        UpstreamObject {
            kind: kind.into(),
            id: id.into(),
            project: project.into(),
            provider: "p".into(),
            upstream_id: up.into(),
            created_us: at,
            body: "{}".into(),
        }
    }

    #[tokio::test]
    async fn objects_are_mapped_listed_swapped_and_deleted() {
        let dir = std::env::temp_dir().join(format!(
            "antirion-store-objects-{:x}",
            antirion_core::ResourceUid::random().0
        ));
        let store = Store::open_embedded(&dir).unwrap();
        store.put_object(obj("file", "f1", "a", "up-1", 1)).await.unwrap();
        store.put_object(obj("file", "f2", "a", "up-2", 2)).await.unwrap();
        store.put_object(obj("file", "f3", "b", "up-3", 3)).await.unwrap();
        assert_eq!(
            store.object("file", "f1").await.unwrap().unwrap().upstream_id,
            "up-1"
        );
        assert_eq!(
            store
                .object_by_upstream("file", "p", "up-2")
                .await
                .unwrap()
                .unwrap()
                .id,
            "f2"
        );
        let listed: Vec<String> = store
            .objects("file", "a", None, 10)
            .await
            .unwrap()
            .into_iter()
            .map(|o| o.id)
            .collect();
        assert_eq!(listed, ["f2", "f1"]);
        assert_eq!(store.objects("file", "a", Some(2), 10).await.unwrap().len(), 1);
        assert!(store.swap_object("file", "f1", "{}", "{\"a\":1}").await.unwrap());
        assert!(!store.swap_object("file", "f1", "{}", "{\"a\":2}").await.unwrap());
        assert!(store.delete_object("file", "f1").await.unwrap());
        assert!(store.object("file", "f1").await.unwrap().is_none());
    }
}
