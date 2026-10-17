//! What bandit experiments learn from feedback: per experiment, arm and metric, the samples and
//! their summed reward (in millionths), added to by the node that accepted each feedback; and the
//! arm an experiment stopped on, where the first node to write it wins. Every node reads both, so
//! the nodes of a cluster route on the same evidence.

use crate::budgets::{Arg, Col};
use crate::{Store, StoreError};

pub(crate) const MIGRATIONS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS bandit_rewards (
        experiment TEXT NOT NULL,
        arm TEXT NOT NULL,
        metric TEXT NOT NULL,
        samples INTEGER NOT NULL,
        reward_micros INTEGER NOT NULL,
        PRIMARY KEY (experiment, arm, metric))",
    "CREATE TABLE IF NOT EXISTS bandit_stops (
        experiment TEXT NOT NULL,
        metric TEXT NOT NULL,
        arm TEXT NOT NULL,
        PRIMARY KEY (experiment, metric))",
];

pub(crate) const PG_MIGRATION: &str = "CREATE TABLE IF NOT EXISTS bandit_rewards (
        experiment text NOT NULL,
        arm text NOT NULL,
        metric text NOT NULL,
        samples bigint NOT NULL,
        reward_micros bigint NOT NULL,
        PRIMARY KEY (experiment, arm, metric));
     CREATE TABLE IF NOT EXISTS bandit_stops (
        experiment text NOT NULL,
        metric text NOT NULL,
        arm text NOT NULL,
        PRIMARY KEY (experiment, metric));";

/// What an arm of an experiment has learnt for a metric.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArmReward {
    pub experiment: String,
    pub arm: String,
    pub metric: String,
    pub samples: i64,
    /// The summed reward, each sample's in [0, 1], in millionths.
    pub reward_micros: i64,
}

/// The arm an experiment stopped on, for a metric.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BanditStop {
    pub experiment: String,
    pub metric: String,
    pub arm: String,
}

impl Store {
    /// Adds one sample of `reward_micros` (0 to 1 000 000) to an arm.
    pub async fn add_bandit_reward(
        &self,
        experiment: &str,
        arm: &str,
        metric: &str,
        reward_micros: i64,
    ) -> Result<(), StoreError> {
        self.run_statement(
            "INSERT INTO bandit_rewards (experiment, arm, metric, samples, reward_micros) VALUES ($1, $2, $3, 1, $4)
             ON CONFLICT (experiment, arm, metric) DO UPDATE SET
                samples = bandit_rewards.samples + 1,
                reward_micros = bandit_rewards.reward_micros + excluded.reward_micros",
            vec![experiment.into(), arm.into(), metric.into(), Arg::from(reward_micros)],
        )
        .await
        .map(|_| ())
    }

    /// Every arm's rewards.
    pub async fn bandit_rewards(&self) -> Result<Vec<ArmReward>, StoreError> {
        let rows = self
            .query_cells(
                "SELECT experiment, arm, metric, samples, reward_micros FROM bandit_rewards",
                Vec::new(),
                &[Col::Text, Col::Text, Col::Text, Col::Int, Col::Int],
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|mut c| ArmReward {
                experiment: c.text(),
                arm: c.text(),
                metric: c.text(),
                samples: c.int(),
                reward_micros: c.int(),
            })
            .collect())
    }

    /// Stops an experiment on `arm` for `metric`, unless it already stopped.
    pub async fn stop_bandit(&self, experiment: &str, metric: &str, arm: &str) -> Result<(), StoreError> {
        self.run_statement(
            "INSERT INTO bandit_stops (experiment, metric, arm) VALUES ($1, $2, $3)
             ON CONFLICT (experiment, metric) DO NOTHING",
            vec![experiment.into(), metric.into(), arm.into()],
        )
        .await
        .map(|_| ())
    }

    /// Every stopped experiment.
    pub async fn bandit_stops(&self) -> Result<Vec<BanditStop>, StoreError> {
        let rows = self
            .query_cells(
                "SELECT experiment, metric, arm FROM bandit_stops",
                Vec::new(),
                &[Col::Text, Col::Text, Col::Text],
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|mut c| BanditStop {
                experiment: c.text(),
                metric: c.text(),
                arm: c.text(),
            })
            .collect())
    }

    /// Forgets every reward and stop (test mode).
    pub async fn clear_bandits(&self) -> Result<(), StoreError> {
        self.run_statement("DELETE FROM bandit_rewards", Vec::new())
            .await?;
        self.run_statement("DELETE FROM bandit_stops", Vec::new())
            .await
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::pg::{Migrate, PgSettings};

    async fn behaves(s: &Store) {
        s.add_bandit_reward("e", "a", "good", 1_000_000).await.unwrap();
        s.add_bandit_reward("e", "a", "good", 0).await.unwrap();
        s.add_bandit_reward("e", "b", "good", 500_000).await.unwrap();
        let mut got = s.bandit_rewards().await.unwrap();
        got.sort_by(|x, y| x.arm.cmp(&y.arm));
        assert_eq!((got[0].samples, got[0].reward_micros), (2, 1_000_000));
        assert_eq!((got[1].samples, got[1].reward_micros), (1, 500_000));
        s.stop_bandit("e", "good", "a").await.unwrap();
        s.stop_bandit("e", "good", "b").await.unwrap();
        assert_eq!(s.bandit_stops().await.unwrap()[0].arm, "a", "the first stop wins");
        s.clear_bandits().await.unwrap();
        assert!(s.bandit_rewards().await.unwrap().is_empty());
        assert!(s.bandit_stops().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn rewards_and_stops_on_the_embedded_store() {
        let dir = std::env::temp_dir().join(format!(
            "antirion-store-bandits-{:x}",
            antirion_core::ResourceUid::random().0
        ));
        behaves(&Store::open_embedded(&dir).unwrap()).await;
    }

    #[tokio::test]
    async fn rewards_and_stops_on_postgres() {
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
