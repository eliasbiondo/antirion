//! Requests that wait for a decision: access requests on Models and change requests on the
//! configuration. Each row has a kind, an id, the subject it is about (a Model, or empty), its
//! state, its author and a JSON body; every update is a compare-and-set on `version`, so two
//! deciders on different nodes cannot both win.

use rusqlite::{OptionalExtension, params};

use crate::{Store, StoreError, to_i64};

/// One stored request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingRequest {
    /// `access` or `change`.
    pub kind: String,
    pub id: String,
    /// The Model of an access request; empty for a change request.
    pub subject: String,
    pub state: String,
    pub author: String,
    pub created_us: i64,
    pub version: i64,
    pub body: String,
}

/// What a list selects.
#[derive(Clone, Debug, Default)]
pub struct RequestFilter {
    pub subject: Option<String>,
    /// Any of these states; every state when empty.
    pub states: Vec<String>,
    pub author: Option<String>,
    /// Only ids after this one (ids sort by creation).
    pub after: Option<String>,
    pub limit: u64,
}

pub(crate) const MIGRATIONS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS pending_requests (
        kind TEXT NOT NULL,
        id TEXT NOT NULL,
        subject TEXT NOT NULL,
        state TEXT NOT NULL,
        author TEXT NOT NULL,
        created_us INTEGER NOT NULL,
        version INTEGER NOT NULL,
        body TEXT NOT NULL,
        PRIMARY KEY (kind, id))",
    "CREATE INDEX IF NOT EXISTS pending_requests_subject ON pending_requests (kind, subject, id)",
];

pub(crate) const PG_MIGRATION: &str = "CREATE TABLE IF NOT EXISTS pending_requests (
        kind text NOT NULL,
        id text NOT NULL,
        subject text NOT NULL,
        state text NOT NULL,
        author text NOT NULL,
        created_us bigint NOT NULL,
        version bigint NOT NULL,
        body text NOT NULL,
        PRIMARY KEY (kind, id));
     CREATE INDEX IF NOT EXISTS pending_requests_subject ON pending_requests (kind, subject, id);";

const COLUMNS: &str = "kind, id, subject, state, author, created_us, version, body";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<PendingRequest> {
    Ok(PendingRequest {
        kind: r.get(0)?,
        id: r.get(1)?,
        subject: r.get(2)?,
        state: r.get(3)?,
        author: r.get(4)?,
        created_us: r.get(5)?,
        version: r.get(6)?,
        body: r.get(7)?,
    })
}

fn pg_row(r: &tokio_postgres::Row) -> Result<PendingRequest, StoreError> {
    Ok(PendingRequest {
        kind: r.try_get(0)?,
        id: r.try_get(1)?,
        subject: r.try_get(2)?,
        state: r.try_get(3)?,
        author: r.try_get(4)?,
        created_us: r.try_get(5)?,
        version: r.try_get(6)?,
        body: r.try_get(7)?,
    })
}

/// A statement written with `$n` parameters in SQLite's explicit `?n` form.
fn lite(sql: &str) -> String {
    sql.replace('$', "?")
}

/// The list query's `WHERE` clause and its text parameters, numbered from `$3` (`$1` is the
/// kind, `$2` the limit).
fn conditions(f: &RequestFilter) -> (String, Vec<String>) {
    let mut sql = String::from("kind = $1");
    let mut args = Vec::new();
    let next = |sql: &mut String, args: &mut Vec<String>, clause: &str, value: &str| {
        args.push(value.to_owned());
        sql.push_str(&clause.replace('#', &format!("${}", args.len() + 2)));
    };
    if let Some(s) = &f.subject {
        next(&mut sql, &mut args, " AND subject = #", s);
    }
    if let Some(a) = &f.author {
        next(&mut sql, &mut args, " AND author = #", a);
    }
    if let Some(a) = &f.after {
        next(&mut sql, &mut args, " AND id > #", a);
    }
    if !f.states.is_empty() {
        let mut parts = Vec::new();
        for s in &f.states {
            args.push(s.clone());
            parts.push(format!("${}", args.len() + 2));
        }
        sql.push_str(&format!(" AND state IN ({})", parts.join(", ")));
    }
    (sql, args)
}

impl Store {
    /// Inserts a new request; `false` when one with the same kind and id exists.
    pub async fn insert_request(&self, r: PendingRequest) -> Result<bool, StoreError> {
        let sql = format!(
            "INSERT INTO pending_requests ({COLUMNS}) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (kind, id) DO NOTHING"
        );
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    Ok(c.execute(
                        &lite(&sql),
                        params![
                            r.kind,
                            r.id,
                            r.subject,
                            r.state,
                            r.author,
                            r.created_us,
                            r.version,
                            r.body
                        ],
                    )? > 0)
                })
                .await
            }
            Store::Postgres(p) => p
                .execute(
                    &sql,
                    &[
                        &r.kind,
                        &r.id,
                        &r.subject,
                        &r.state,
                        &r.author,
                        &r.created_us,
                        &r.version,
                        &r.body,
                    ],
                )
                .await
                .map(|n| n > 0),
        }
    }

    pub async fn request(&self, kind: &str, id: &str) -> Result<Option<PendingRequest>, StoreError> {
        let (kind, id) = (kind.to_owned(), id.to_owned());
        let sql = format!("SELECT {COLUMNS} FROM pending_requests WHERE kind = $1 AND id = $2");
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| Ok(c.query_row(&lite(&sql), params![kind, id], row).optional()?))
                    .await
            }
            Store::Postgres(p) => p.rows(&sql, &[&kind, &id]).await?.first().map(pg_row).transpose(),
        }
    }

    /// Requests of a kind in id order (creation order), as the filter selects.
    pub async fn requests(&self, kind: &str, f: &RequestFilter) -> Result<Vec<PendingRequest>, StoreError> {
        let (cond, args) = conditions(f);
        let sql = format!("SELECT {COLUMNS} FROM pending_requests WHERE {cond} ORDER BY id LIMIT $2");
        let kind = kind.to_owned();
        let limit = to_i64(f.limit);
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    let mut all: Vec<&dyn rusqlite::ToSql> = vec![&kind, &limit];
                    all.extend(args.iter().map(|a| a as &dyn rusqlite::ToSql));
                    let mut st = c.prepare(&lite(&sql))?;
                    let rows = st.query_map(all.as_slice(), row)?;
                    Ok(rows.collect::<Result<Vec<_>, _>>()?)
                })
                .await
            }
            Store::Postgres(p) => {
                let mut all: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = vec![&kind, &limit];
                all.extend(
                    args.iter()
                        .map(|a| a as &(dyn tokio_postgres::types::ToSql + Sync)),
                );
                p.rows(&sql, &all).await?.iter().map(pg_row).collect()
            }
        }
    }

    /// Writes a request's new state and body if its version is still `version`; the stored
    /// version becomes `version + 1`. `false` when another writer moved it first.
    pub async fn update_request(
        &self,
        kind: &str,
        id: &str,
        version: i64,
        state: &str,
        body: &str,
    ) -> Result<bool, StoreError> {
        let (kind, id, state, body) = (kind.to_owned(), id.to_owned(), state.to_owned(), body.to_owned());
        let sql = "UPDATE pending_requests SET state = $4, body = $5, version = version + 1
                   WHERE kind = $1 AND id = $2 AND version = $3";
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| Ok(c.execute(&lite(sql), params![kind, id, version, state, body])? > 0))
                    .await
            }
            Store::Postgres(p) => p
                .execute(sql, &[&kind, &id, &version, &state, &body])
                .await
                .map(|n| n > 0),
        }
    }

    /// Forgets every request (test mode).
    pub async fn clear_requests(&self) -> Result<(), StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(|c| {
                    c.execute("DELETE FROM pending_requests", [])?;
                    Ok(())
                })
                .await
            }
            Store::Postgres(p) => p.execute("DELETE FROM pending_requests", &[]).await.map(|_| ()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(id: &str, subject: &str, state: &str, author: &str) -> PendingRequest {
        PendingRequest {
            kind: "access".into(),
            id: id.into(),
            subject: subject.into(),
            state: state.into(),
            author: author.into(),
            created_us: 1,
            version: 0,
            body: "{}".into(),
        }
    }

    #[tokio::test]
    async fn requests_are_listed_filtered_and_swapped() {
        let dir = std::env::temp_dir().join(format!(
            "antirion-store-requests-{:x}",
            antirion_core::ResourceUid::random().0
        ));
        let store = Store::open_embedded(&dir).unwrap();
        assert!(store.insert_request(req("a1", "m", "open", "u1")).await.unwrap());
        assert!(!store.insert_request(req("a1", "m", "open", "u1")).await.unwrap());
        store
            .insert_request(req("a2", "m", "approved", "u2"))
            .await
            .unwrap();
        store.insert_request(req("a3", "n", "open", "u1")).await.unwrap();
        let ids = |v: Vec<PendingRequest>| v.into_iter().map(|r| r.id).collect::<Vec<_>>();
        let f = |subject: Option<&str>, states: &[&str], author: Option<&str>, after: Option<&str>| {
            RequestFilter {
                subject: subject.map(str::to_owned),
                states: states.iter().map(|s| (*s).to_owned()).collect(),
                author: author.map(str::to_owned),
                after: after.map(str::to_owned),
                limit: 10,
            }
        };
        assert_eq!(
            ids(store
                .requests("access", &f(Some("m"), &[], None, None))
                .await
                .unwrap()),
            ["a1", "a2"]
        );
        assert_eq!(
            ids(store
                .requests("access", &f(None, &["open"], None, None))
                .await
                .unwrap()),
            ["a1", "a3"]
        );
        assert_eq!(
            ids(store
                .requests("access", &f(None, &[], Some("u1"), Some("a1")))
                .await
                .unwrap()),
            ["a3"]
        );
        assert!(
            store
                .update_request("access", "a1", 0, "approved", "{\"x\":1}")
                .await
                .unwrap()
        );
        assert!(
            !store
                .update_request("access", "a1", 0, "denied", "{}")
                .await
                .unwrap()
        );
        let a1 = store.request("access", "a1").await.unwrap().unwrap();
        assert_eq!((a1.state.as_str(), a1.version), ("approved", 1));
        store.clear_requests().await.unwrap();
        assert!(store.request("access", "a2").await.unwrap().is_none());
    }
}
