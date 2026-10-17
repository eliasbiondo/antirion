//! What the cache console keeps beyond configuration: each firing of an invalidation rule (a
//! scheduled firing is claimed by its `once` key, so one node of a cluster fires it), and per-node
//! daily tallies of semantic hits, their summed similarity and false-positive reports.

use crate::budgets::{Cells, Col};
use crate::{Store, StoreError};

/// The firings of one rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleFirings {
    pub rule: String,
    /// Firings at or after the `since` of the read.
    pub recent: i64,
    pub last_fired_us: Option<i64>,
    /// What the last firing dropped.
    pub last_dropped: i64,
}

/// One node's semantic counts of a group on a day.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticTally {
    pub group: String,
    /// Days since the Unix epoch (UTC).
    pub day: i64,
    pub hits: i64,
    /// The summed similarity of the hits, in millionths.
    pub similarity_micros: i64,
    pub reports: i64,
}

pub(crate) const MIGRATIONS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS cache_rule_firings (
        id TEXT PRIMARY KEY,
        rule TEXT NOT NULL,
        fired_us INTEGER NOT NULL,
        trigger_event TEXT NOT NULL,
        dropped INTEGER NOT NULL,
        once_key TEXT UNIQUE)",
    "CREATE INDEX IF NOT EXISTS cache_rule_firings_rule ON cache_rule_firings (rule, fired_us)",
    "CREATE TABLE IF NOT EXISTS cache_semantic_tallies (
        grp TEXT NOT NULL,
        day INTEGER NOT NULL,
        node TEXT NOT NULL,
        hits INTEGER NOT NULL,
        similarity_micros INTEGER NOT NULL,
        reports INTEGER NOT NULL,
        PRIMARY KEY (grp, day, node))",
];

pub(crate) const PG_MIGRATION: &str = "CREATE TABLE IF NOT EXISTS cache_rule_firings (
        id text PRIMARY KEY,
        rule text NOT NULL,
        fired_us bigint NOT NULL,
        trigger_event text NOT NULL,
        dropped bigint NOT NULL,
        once_key text UNIQUE);
     CREATE INDEX IF NOT EXISTS cache_rule_firings_rule ON cache_rule_firings (rule, fired_us);
     CREATE TABLE IF NOT EXISTS cache_semantic_tallies (
        grp text NOT NULL,
        day bigint NOT NULL,
        node text NOT NULL,
        hits bigint NOT NULL,
        similarity_micros bigint NOT NULL,
        reports bigint NOT NULL,
        PRIMARY KEY (grp, day, node));";

impl Store {
    /// Records a firing; with `once`, only the first firing under that key is recorded. Whether this
    /// one was.
    pub async fn record_rule_firing(
        &self,
        id: &str,
        rule: &str,
        fired_us: i64,
        trigger: &str,
        dropped: i64,
        once: Option<&str>,
    ) -> Result<bool, StoreError> {
        let n = self
            .run_statement(
                "INSERT INTO cache_rule_firings (id, rule, fired_us, trigger_event, dropped, once_key)
                 VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
                vec![
                    id.into(),
                    rule.into(),
                    fired_us.into(),
                    trigger.into(),
                    dropped.into(),
                    once.map(str::to_owned).into(),
                ],
            )
            .await?;
        Ok(n == 1)
    }

    /// Sets what a recorded firing dropped.
    pub async fn set_rule_firing_dropped(&self, id: &str, dropped: i64) -> Result<(), StoreError> {
        self.run_statement(
            "UPDATE cache_rule_firings SET dropped = $2 WHERE id = $1",
            vec![id.into(), dropped.into()],
        )
        .await
        .map(|_| ())
    }

    /// Every rule that fired, with its firings since `since_us` and its last firing.
    pub async fn rule_firings(&self, since_us: i64) -> Result<Vec<RuleFirings>, StoreError> {
        let rows = self
            .query_cells(
                "SELECT f.rule,
                        CAST(SUM(CASE WHEN f.fired_us >= $1 THEN 1 ELSE 0 END) AS BIGINT),
                        CAST(MAX(f.fired_us) AS BIGINT),
                        CAST((SELECT g.dropped FROM cache_rule_firings g WHERE g.rule = f.rule
                              ORDER BY g.fired_us DESC, g.id DESC LIMIT 1) AS BIGINT)
                 FROM cache_rule_firings f GROUP BY f.rule ORDER BY f.rule",
                vec![since_us.into()],
                &[Col::Text, Col::Int, Col::Int, Col::Int],
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|mut c: Cells| RuleFirings {
                rule: c.text(),
                recent: c.int(),
                last_fired_us: c.opt_int(),
                last_dropped: c.int(),
            })
            .collect())
    }

    /// Adds one node's counts to its stored tallies.
    pub async fn add_semantic_tallies(&self, node: &str, rows: Vec<SemanticTally>) -> Result<(), StoreError> {
        for r in rows {
            self.run_statement(
                "INSERT INTO cache_semantic_tallies (grp, day, node, hits, similarity_micros, reports)
                 VALUES ($1, $2, $3, $4, $5, $6)
                 ON CONFLICT (grp, day, node) DO UPDATE SET
                    hits = cache_semantic_tallies.hits + excluded.hits,
                    similarity_micros = cache_semantic_tallies.similarity_micros + excluded.similarity_micros,
                    reports = cache_semantic_tallies.reports + excluded.reports",
                vec![
                    r.group.into(),
                    r.day.into(),
                    node.into(),
                    r.hits.into(),
                    r.similarity_micros.into(),
                    r.reports.into(),
                ],
            )
            .await?;
        }
        Ok(())
    }

    /// Each group's counts from day `since_day` on, every node's (`day` is `since_day`).
    pub async fn semantic_tallies(&self, since_day: i64) -> Result<Vec<SemanticTally>, StoreError> {
        let rows = self
            .query_cells(
                "SELECT grp, CAST(SUM(hits) AS BIGINT), CAST(SUM(similarity_micros) AS BIGINT),
                        CAST(SUM(reports) AS BIGINT)
                 FROM cache_semantic_tallies WHERE day >= $1 GROUP BY grp ORDER BY grp",
                vec![since_day.into()],
                &[Col::Text, Col::Int, Col::Int, Col::Int],
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|mut c| SemanticTally {
                group: c.text(),
                day: since_day,
                hits: c.int(),
                similarity_micros: c.int(),
                reports: c.int(),
            })
            .collect())
    }

    /// Forgets firings before `fired_before_us` and tallies before day `before_day`.
    pub async fn prune_cache_console(&self, fired_before_us: i64, before_day: i64) -> Result<(), StoreError> {
        self.run_statement(
            "DELETE FROM cache_rule_firings WHERE fired_us < $1",
            vec![fired_before_us.into()],
        )
        .await?;
        self.run_statement(
            "DELETE FROM cache_semantic_tallies WHERE day < $1",
            vec![before_day.into()],
        )
        .await
        .map(|_| ())
    }

    /// Forgets every firing and tally (test mode).
    pub async fn clear_cache_console(&self) -> Result<(), StoreError> {
        for table in ["cache_rule_firings", "cache_semantic_tallies"] {
            self.run_statement(&format!("DELETE FROM {table}"), Vec::new())
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::pg::{Migrate, PgSettings};

    fn embedded() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "antirion-store-cache-{:x}",
            antirion_core::ResourceUid::random().0
        ));
        Store::open_embedded(&dir).unwrap()
    }

    async fn exercise(s: &Store) {
        s.clear_cache_console().await.unwrap();
        assert!(
            s.record_rule_firing("f1", "r", 10, "manual", 2, None)
                .await
                .unwrap()
        );
        assert!(
            s.record_rule_firing("f2", "r", 20, "schedule", 0, Some("r@1"))
                .await
                .unwrap()
        );
        assert!(
            !s.record_rule_firing("f3", "r", 20, "schedule", 0, Some("r@1"))
                .await
                .unwrap()
        );
        s.set_rule_firing_dropped("f2", 5).await.unwrap();
        let got = s.rule_firings(15).await.unwrap();
        assert_eq!(
            got,
            vec![RuleFirings {
                rule: "r".into(),
                recent: 1,
                last_fired_us: Some(20),
                last_dropped: 5,
            }]
        );
        let t = |node_hits: i64| SemanticTally {
            group: "gateway".into(),
            day: 100,
            hits: node_hits,
            similarity_micros: node_hits * 970_000,
            reports: 1,
        };
        s.add_semantic_tallies("a", vec![t(2)]).await.unwrap();
        s.add_semantic_tallies("a", vec![t(1)]).await.unwrap();
        s.add_semantic_tallies("b", vec![t(1)]).await.unwrap();
        let sums = s.semantic_tallies(100).await.unwrap();
        assert_eq!(sums.len(), 1);
        assert_eq!((sums[0].hits, sums[0].reports), (4, 3));
        assert!(s.semantic_tallies(101).await.unwrap().is_empty());
        s.prune_cache_console(15, 101).await.unwrap();
        assert_eq!(s.rule_firings(0).await.unwrap()[0].recent, 1);
        assert!(s.semantic_tallies(0).await.unwrap().is_empty());
        s.clear_cache_console().await.unwrap();
    }

    #[tokio::test]
    async fn firings_and_tallies_on_the_embedded_store() {
        exercise(&embedded()).await;
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

    #[tokio::test]
    async fn firings_and_tallies_on_postgres() {
        if let Some(store) = postgres().await {
            exercise(&store).await;
        }
    }
}
