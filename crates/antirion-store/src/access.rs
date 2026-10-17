//! Admin-plane access state: sessions (stored by the digest of their id, so the store never holds a
//! usable session id) and role bindings held by individual admin tokens.

use rusqlite::{OptionalExtension, params};

use crate::{Store, StoreError};

/// An admin session: started with an admin token (`kid`), the bootstrap credential, or a user's
/// sign-in (`user:<name>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRecord {
    /// Hex SHA-256 of the session id.
    pub id_digest: String,
    /// The kid of the token the session was started with, `bootstrap`, or `user:<name>`.
    pub kid: String,
    pub created_us: i64,
    pub last_used_us: i64,
    /// The user the session acts for, when it has one.
    pub user: Option<String>,
    /// How the session was started (method, factor, trust, organization, client), as JSON.
    pub facts: String,
}

/// A role binding whose principal is one admin token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenBindingRecord {
    pub id: String,
    pub kid: String,
    pub role: String,
    pub scope: String,
    pub created_at: String,
}

/// Columns added after the first release, with their declarations.
pub(crate) const ADDED_COLUMNS: &[(&str, &str, &str)] = &[
    ("admin_sessions", "user_name", "TEXT"),
    ("admin_sessions", "facts", "TEXT NOT NULL DEFAULT '{}'"),
    ("admin_tokens", "access", "TEXT"),
    ("admin_tokens", "last_used_us", "INTEGER"),
];

pub(crate) const MIGRATIONS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS admin_sessions (
        id_digest TEXT PRIMARY KEY,
        kid TEXT NOT NULL,
        created_us INTEGER NOT NULL,
        last_used_us INTEGER NOT NULL)",
    "CREATE TABLE IF NOT EXISTS token_bindings (
        id TEXT PRIMARY KEY,
        kid TEXT NOT NULL,
        role TEXT NOT NULL,
        scope TEXT NOT NULL,
        created_at TEXT NOT NULL)",
    "CREATE INDEX IF NOT EXISTS token_bindings_kid ON token_bindings (kid)",
];

/// Indexes over the added columns, created once the columns exist.
pub(crate) const LATER_INDEXES: &[&str] = &[
    "CREATE INDEX IF NOT EXISTS admin_sessions_user ON admin_sessions (user_name)",
    "CREATE INDEX IF NOT EXISTS admin_sessions_kid ON admin_sessions (kid)",
];

fn session_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRecord> {
    Ok(SessionRecord {
        id_digest: r.get(0)?,
        kid: r.get(1)?,
        created_us: r.get(2)?,
        last_used_us: r.get(3)?,
        user: r.get(4)?,
        facts: r.get(5)?,
    })
}

fn binding_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<TokenBindingRecord> {
    Ok(TokenBindingRecord {
        id: r.get(0)?,
        kid: r.get(1)?,
        role: r.get(2)?,
        scope: r.get(3)?,
        created_at: r.get(4)?,
    })
}

impl Store {
    pub async fn put_session(&self, rec: SessionRecord) -> Result<(), StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    c.execute(
                        "INSERT INTO admin_sessions (id_digest, kid, created_us, last_used_us, user_name, facts)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![
                            rec.id_digest,
                            rec.kid,
                            rec.created_us,
                            rec.last_used_us,
                            rec.user,
                            rec.facts
                        ],
                    )?;
                    Ok(())
                })
                .await
            }
            Store::Postgres(p) => p.put_session(rec).await,
        }
    }

    pub async fn session(&self, id_digest: String) -> Result<Option<SessionRecord>, StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    Ok(c.query_row(
                        "SELECT id_digest, kid, created_us, last_used_us, user_name, facts FROM admin_sessions
                         WHERE id_digest = ?1",
                        [id_digest],
                        session_row,
                    )
                    .optional()?)
                })
                .await
            }
            Store::Postgres(p) => p.session(id_digest).await,
        }
    }

    /// Records a use of the session at `now_us`.
    pub async fn touch_session(&self, id_digest: String, now_us: i64) -> Result<(), StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    c.execute(
                        "UPDATE admin_sessions SET last_used_us = ?2 WHERE id_digest = ?1",
                        params![id_digest, now_us],
                    )?;
                    Ok(())
                })
                .await
            }
            Store::Postgres(p) => p
                .execute(
                    "UPDATE admin_sessions SET last_used_us = $2 WHERE id_digest = $1",
                    &[&id_digest, &now_us],
                )
                .await
                .map(|_| ()),
        }
    }

    pub async fn delete_session(&self, id_digest: String) -> Result<(), StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    c.execute("DELETE FROM admin_sessions WHERE id_digest = ?1", [id_digest])?;
                    Ok(())
                })
                .await
            }
            Store::Postgres(p) => p
                .execute("DELETE FROM admin_sessions WHERE id_digest = $1", &[&id_digest])
                .await
                .map(|_| ()),
        }
    }

    pub async fn put_token_binding(&self, rec: TokenBindingRecord) -> Result<(), StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    c.execute(
                        "INSERT OR IGNORE INTO token_bindings (id, kid, role, scope, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![rec.id, rec.kid, rec.role, rec.scope, rec.created_at],
                    )?;
                    Ok(())
                })
                .await
            }
            Store::Postgres(p) => p.put_token_binding(rec).await,
        }
    }

    /// The bindings of one token, or of every token when `kid` is `None`.
    pub async fn token_bindings(&self, kid: Option<String>) -> Result<Vec<TokenBindingRecord>, StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    let mut stmt = c.prepare(
                        "SELECT id, kid, role, scope, created_at FROM token_bindings
                         WHERE ?1 IS NULL OR kid = ?1 ORDER BY id",
                    )?;
                    let rows = stmt.query_map([kid], binding_row)?;
                    Ok(rows.collect::<Result<Vec<_>, _>>()?)
                })
                .await
            }
            Store::Postgres(p) => p.token_bindings(kid).await,
        }
    }

    /// Deletes one binding; true when it existed.
    pub async fn delete_token_binding(&self, id: String) -> Result<bool, StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| Ok(c.execute("DELETE FROM token_bindings WHERE id = ?1", [id])? > 0))
                    .await
            }
            Store::Postgres(p) => p
                .execute("DELETE FROM token_bindings WHERE id = $1", &[&id])
                .await
                .map(|n| n > 0),
        }
    }

    /// The sessions acting for `user`, or started with credential `kid`.
    pub async fn sessions_of(
        &self,
        user: Option<String>,
        kid: Option<String>,
    ) -> Result<Vec<SessionRecord>, StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    let mut stmt = c.prepare(
                        "SELECT id_digest, kid, created_us, last_used_us, user_name, facts FROM admin_sessions
                         WHERE (?1 IS NOT NULL AND user_name = ?1) OR (?2 IS NOT NULL AND kid = ?2)
                         ORDER BY created_us, id_digest",
                    )?;
                    let rows = stmt.query_map(params![user, kid], session_row)?;
                    Ok(rows.collect::<Result<Vec<_>, _>>()?)
                })
                .await
            }
            Store::Postgres(p) => p
                .rows(
                    "SELECT id_digest, kid, created_us, last_used_us, user_name, facts FROM admin_sessions
                     WHERE ($1::text IS NOT NULL AND user_name = $1) OR ($2::text IS NOT NULL AND kid = $2)
                     ORDER BY created_us, id_digest",
                    &[&user, &kid],
                )
                .await?
                .iter()
                .map(crate::pg::ops::session_of)
                .collect(),
        }
    }

    /// Deletes these sessions; returns how many existed.
    pub async fn delete_sessions(&self, digests: Vec<String>) -> Result<u64, StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    let mut n = 0;
                    for d in &digests {
                        n += c.execute("DELETE FROM admin_sessions WHERE id_digest = ?1", [d])?;
                    }
                    Ok(u64::try_from(n).unwrap_or(0))
                })
                .await
            }
            Store::Postgres(p) => {
                p.execute(
                    "DELETE FROM admin_sessions WHERE id_digest = ANY($1)",
                    &[&digests],
                )
                .await
            }
        }
    }

    /// Removes a token's sessions and bindings (the token was deleted).
    pub async fn forget_token(&self, kid: String) -> Result<(), StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    c.execute("DELETE FROM admin_sessions WHERE kid = ?1", [&kid])?;
                    c.execute("DELETE FROM token_bindings WHERE kid = ?1", [&kid])?;
                    Ok(())
                })
                .await
            }
            Store::Postgres(p) => {
                p.execute("DELETE FROM admin_sessions WHERE kid = $1", &[&kid])
                    .await?;
                p.execute("DELETE FROM token_bindings WHERE kid = $1", &[&kid])
                    .await
                    .map(|_| ())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "antirion-store-access-{:x}",
            antirion_core::ResourceUid::random().0
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[tokio::test]
    async fn sessions_and_token_bindings() {
        let store = Store::open_embedded(&temp_dir()).unwrap();
        let s = SessionRecord {
            id_digest: "d".into(),
            kid: "k".into(),
            created_us: 1,
            last_used_us: 1,
            user: Some("alice".into()),
            facts: "{}".into(),
        };
        store.put_session(s.clone()).await.unwrap();
        let other = SessionRecord {
            id_digest: "e".into(),
            kid: "user:alice".into(),
            ..s.clone()
        };
        store.put_session(other).await.unwrap();
        assert_eq!(
            store.sessions_of(Some("alice".into()), None).await.unwrap().len(),
            2
        );
        assert_eq!(store.sessions_of(None, Some("k".into())).await.unwrap().len(), 1);
        assert_eq!(
            store.delete_sessions(vec!["e".into(), "x".into()]).await.unwrap(),
            1
        );
        store.touch_session("d".into(), 5).await.unwrap();
        assert_eq!(store.session("d".into()).await.unwrap().unwrap().last_used_us, 5);
        let b = TokenBindingRecord {
            id: "rb".into(),
            kid: "k".into(),
            role: "viewer".into(),
            scope: "team/t".into(),
            created_at: "t".into(),
        };
        store.put_token_binding(b.clone()).await.unwrap();
        assert_eq!(
            store.token_bindings(Some("k".into())).await.unwrap(),
            vec![b.clone()]
        );
        assert_eq!(store.token_bindings(None).await.unwrap().len(), 1);
        store.forget_token("k".into()).await.unwrap();
        assert!(store.session("d".into()).await.unwrap().is_none());
        assert!(store.token_bindings(None).await.unwrap().is_empty());
        store.delete_session("d".into()).await.unwrap();
        assert!(!store.delete_token_binding("rb".into()).await.unwrap());
    }
}
