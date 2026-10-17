//! Claims on records that must exist once across every node (an idempotency key in progress): a
//! claim inserts the record unless a live one holds its `(kind, id)`; an expired one is replaced.

use rusqlite::params;

use crate::{IdentityRecord, Store, StoreError};

impl Store {
    /// Inserts `rec` unless a record of its kind and id that has not expired by `now_us` exists;
    /// true when this call wrote it.
    pub async fn claim_record(&self, rec: IdentityRecord, now_us: i64) -> Result<bool, StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    Ok(c.execute(
                        "INSERT INTO identity_records (kind, id, user_name, expires_us, body) VALUES (?1, ?2, ?3, ?4, ?5)
                         ON CONFLICT (kind, id) DO UPDATE SET user_name = excluded.user_name,
                            expires_us = excluded.expires_us, body = excluded.body
                         WHERE identity_records.expires_us IS NOT NULL AND identity_records.expires_us <= ?6",
                        params![rec.kind, rec.id, rec.user, rec.expires_us, rec.body, now_us],
                    )? == 1)
                })
                .await
            }
            Store::Postgres(p) => p
                .execute(
                    "INSERT INTO identity_records (kind, id, user_name, expires_us, body) VALUES ($1, $2, $3, $4, $5)
                     ON CONFLICT (kind, id) DO UPDATE SET user_name = excluded.user_name,
                        expires_us = excluded.expires_us, body = excluded.body
                     WHERE identity_records.expires_us IS NOT NULL AND identity_records.expires_us <= $6",
                    &[&rec.kind, &rec.id, &rec.user, &rec.expires_us, &rec.body, &now_us],
                )
                .await
                .map(|n| n == 1),
        }
    }

    /// Deletes the records of `kind` that expired by `now_us`; returns how many.
    pub async fn purge_expired(&self, kind: &str, now_us: i64) -> Result<u64, StoreError> {
        let kind = kind.to_owned();
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    Ok(c.execute(
                        "DELETE FROM identity_records WHERE kind = ?1 AND expires_us IS NOT NULL AND expires_us <= ?2",
                        params![kind, now_us],
                    )? as u64)
                })
                .await
            }
            Store::Postgres(p) => {
                p.execute(
                    "DELETE FROM identity_records WHERE kind = $1 AND expires_us IS NOT NULL AND expires_us <= $2",
                    &[&kind, &now_us],
                )
                .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{IdentityRecord, Store};

    fn rec(body: &str, expires_us: i64) -> IdentityRecord {
        IdentityRecord {
            kind: "idempotency".into(),
            id: "k".into(),
            user: "u".into(),
            expires_us: Some(expires_us),
            body: body.into(),
        }
    }

    #[tokio::test]
    async fn a_live_claim_holds_and_an_expired_one_is_replaced() {
        let dir = std::env::temp_dir().join(format!(
            "antirion-store-claim-{:x}",
            antirion_core::ResourceUid::random().0
        ));
        let store = Store::open_embedded(&dir).unwrap();
        assert!(store.claim_record(rec("first", 100), 10).await.unwrap());
        assert!(!store.claim_record(rec("second", 200), 50).await.unwrap());
        assert_eq!(
            store.record("idempotency", "k").await.unwrap().unwrap().body,
            "first"
        );
        assert!(store.claim_record(rec("third", 300), 100).await.unwrap());
        assert_eq!(
            store.record("idempotency", "k").await.unwrap().unwrap().body,
            "third"
        );
        assert_eq!(store.purge_expired("idempotency", 299).await.unwrap(), 0);
        assert_eq!(store.purge_expired("idempotency", 300).await.unwrap(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
