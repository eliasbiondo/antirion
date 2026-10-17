//! Identity records written at most once while they are valid: the first writer of a `(kind, id)`
//! wins on every node, and a record whose expiry has passed may be written again. Used to refuse a
//! single sign-on assertion that was presented before.

use rusqlite::params;

use crate::{IdentityRecord, Store, StoreError};

impl Store {
    /// Writes `rec` unless a record with its kind and id exists that has not expired at `now_us`;
    /// true when it was written. Expired records of the kind are removed on the way.
    pub async fn put_record_once(&self, rec: IdentityRecord, now_us: i64) -> Result<bool, StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    c.execute(
                        "DELETE FROM identity_records WHERE kind = ?1 AND expires_us IS NOT NULL AND expires_us <= ?2",
                        params![rec.kind, now_us],
                    )?;
                    Ok(c.execute(
                        "INSERT INTO identity_records (kind, id, user_name, expires_us, body) VALUES (?1, ?2, ?3, ?4, ?5)
                         ON CONFLICT (kind, id) DO NOTHING",
                        params![rec.kind, rec.id, rec.user, rec.expires_us, rec.body],
                    )? == 1)
                })
                .await
            }
            Store::Postgres(p) => {
                p.execute(
                    "DELETE FROM identity_records WHERE kind = $1 AND expires_us IS NOT NULL AND expires_us <= $2",
                    &[&rec.kind, &now_us],
                )
                .await?;
                p.execute(
                    "INSERT INTO identity_records (kind, id, user_name, expires_us, body) VALUES ($1, $2, $3, $4, $5)
                     ON CONFLICT (kind, id) DO NOTHING",
                    &[&rec.kind, &rec.id, &rec.user, &rec.expires_us, &rec.body],
                )
                .await
                .map(|n| n == 1)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, expires_us: i64) -> IdentityRecord {
        IdentityRecord {
            kind: "once_test".to_owned(),
            id: id.to_owned(),
            user: String::new(),
            expires_us: Some(expires_us),
            body: "{}".to_owned(),
        }
    }

    #[tokio::test]
    async fn the_first_writer_wins_until_the_record_expires() {
        let dir = std::env::temp_dir().join(format!(
            "antirion-store-once-{:x}",
            antirion_core::ResourceUid::random().0
        ));
        let store = Store::open_embedded(&dir).unwrap();
        assert!(store.put_record_once(record("a", 100), 10).await.unwrap());
        assert!(!store.put_record_once(record("a", 200), 50).await.unwrap());
        assert!(store.put_record_once(record("b", 100), 50).await.unwrap());
        assert!(
            store.put_record_once(record("a", 300), 100).await.unwrap(),
            "expired at 100"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
