//! Provider rate-limit pressure history: per node, provider credential and minute, the peak use of
//! its upstream request and token quotas. Each node adds its own rows, keeping the larger value
//! when a minute is written again; reads see every node's rows.

use crate::budgets::{Arg, Col};
use crate::{Store, StoreError};

pub(crate) const MIGRATIONS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS provider_pressure (
        node_id TEXT NOT NULL,
        provider TEXT NOT NULL,
        credential TEXT NOT NULL,
        minute_us INTEGER NOT NULL,
        req_used INTEGER,
        req_limit INTEGER,
        tok_used INTEGER,
        tok_limit INTEGER,
        PRIMARY KEY (node_id, provider, credential, minute_us))
    ",
    "CREATE INDEX IF NOT EXISTS provider_pressure_minute ON provider_pressure (minute_us)",
];

pub(crate) const PG_MIGRATION: &str = "CREATE TABLE IF NOT EXISTS provider_pressure (
        node_id text NOT NULL,
        provider text NOT NULL,
        credential text NOT NULL,
        minute_us bigint NOT NULL,
        req_used bigint,
        req_limit bigint,
        tok_used bigint,
        tok_limit bigint,
        PRIMARY KEY (node_id, provider, credential, minute_us));
     CREATE INDEX IF NOT EXISTS provider_pressure_minute ON provider_pressure (minute_us);";

/// One credential's peak use in one minute, as one node saw it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PressureRow {
    pub provider: String,
    pub credential: String,
    pub minute_us: i64,
    pub req_used: Option<i64>,
    pub req_limit: Option<i64>,
    pub tok_used: Option<i64>,
    pub tok_limit: Option<i64>,
}

/// The larger of the stored and the written value of `col`, a missing value being the smallest.
fn larger(col: &str) -> String {
    format!(
        "{col} = CASE WHEN excluded.{col} IS NULL OR (provider_pressure.{col} IS NOT NULL \
         AND provider_pressure.{col} >= excluded.{col}) THEN provider_pressure.{col} ELSE excluded.{col} END"
    )
}

impl Store {
    /// Adds a node's minutes, keeping the larger values of a minute written before.
    pub async fn add_pressure(&self, node_id: &str, rows: &[PressureRow]) -> Result<(), StoreError> {
        let sql = format!(
            "INSERT INTO provider_pressure (node_id, provider, credential, minute_us, req_used, req_limit, tok_used, tok_limit)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (node_id, provider, credential, minute_us) DO UPDATE SET {}, {}, {}, {}",
            larger("req_used"),
            larger("req_limit"),
            larger("tok_used"),
            larger("tok_limit"),
        );
        for r in rows {
            self.run_statement(
                &sql,
                vec![
                    node_id.into(),
                    r.provider.as_str().into(),
                    r.credential.as_str().into(),
                    Arg::from(r.minute_us),
                    Arg::from(r.req_used),
                    Arg::from(r.req_limit),
                    Arg::from(r.tok_used),
                    Arg::from(r.tok_limit),
                ],
            )
            .await?;
        }
        Ok(())
    }

    /// Every node's minutes in `[from_us, to_us)`.
    pub async fn pressure_between(&self, from_us: i64, to_us: i64) -> Result<Vec<PressureRow>, StoreError> {
        let rows = self
            .query_cells(
                "SELECT provider, credential, minute_us, req_used, req_limit, tok_used, tok_limit
                 FROM provider_pressure WHERE minute_us >= $1 AND minute_us < $2",
                vec![Arg::from(from_us), Arg::from(to_us)],
                &[
                    Col::Text,
                    Col::Text,
                    Col::Int,
                    Col::Int,
                    Col::Int,
                    Col::Int,
                    Col::Int,
                ],
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|mut c| PressureRow {
                provider: c.text(),
                credential: c.text(),
                minute_us: c.int(),
                req_used: c.opt_int(),
                req_limit: c.opt_int(),
                tok_used: c.opt_int(),
                tok_limit: c.opt_int(),
            })
            .collect())
    }

    /// Removes the minutes before `before_us`; the number removed.
    pub async fn expire_pressure(&self, before_us: i64) -> Result<u64, StoreError> {
        self.run_statement(
            "DELETE FROM provider_pressure WHERE minute_us < $1",
            vec![Arg::from(before_us)],
        )
        .await
    }

    /// Forgets the whole history (test mode).
    pub async fn clear_pressure(&self) -> Result<(), StoreError> {
        self.run_statement("DELETE FROM provider_pressure", Vec::new())
            .await
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::pg::{Migrate, PgSettings};

    fn row(minute_us: i64, req_used: Option<i64>) -> PressureRow {
        PressureRow {
            provider: "p".into(),
            credential: "default".into(),
            minute_us,
            req_used,
            req_limit: Some(500),
            ..PressureRow::default()
        }
    }

    async fn behaves(s: &Store) {
        s.add_pressure("n1", &[row(60, Some(100)), row(120, Some(5))])
            .await
            .unwrap();
        s.add_pressure("n1", &[row(60, Some(50)), row(120, None)])
            .await
            .unwrap();
        s.add_pressure("n2", &[row(60, Some(300))]).await.unwrap();
        let mut got = s.pressure_between(0, 120).await.unwrap();
        got.sort_by_key(|r| r.req_used);
        assert_eq!(got.len(), 2, "one row per node and minute");
        assert_eq!(got[0].req_used, Some(100), "a minute keeps its larger value");
        assert_eq!(got[1].req_used, Some(300));
        assert_eq!(got[0].tok_used, None);
        assert_eq!(s.pressure_between(120, 180).await.unwrap()[0].req_used, Some(5));
        assert_eq!(s.expire_pressure(120).await.unwrap(), 2);
        s.clear_pressure().await.unwrap();
        assert!(s.pressure_between(0, 1_000).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn minutes_on_the_embedded_store() {
        let dir = std::env::temp_dir().join(format!(
            "antirion-store-pressure-{:x}",
            antirion_core::ResourceUid::random().0
        ));
        behaves(&Store::open_embedded(&dir).unwrap()).await;
    }

    #[tokio::test]
    async fn minutes_on_postgres() {
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
        behaves(&Store::open_postgres(&settings).await.unwrap()).await;
    }
}
