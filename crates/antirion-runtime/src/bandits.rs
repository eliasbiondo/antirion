//! Bandit experiments learn from feedback: a value of a bandit's metric on a record served by one
//! of its arms becomes a reward in [0, 1] (true is 1; a number is placed between its metric's `min`
//! and `max`) added in the store, and every node copies the store's rewards and stops into its
//! routing state every second, and at once after it wrote one. The copy also decides stops: once
//! an arm's posterior probability of being the best exceeds `1 − delta`, the experiment is stopped
//! on it in the store, where the first node to write wins.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use antirion_obs::feedback::MetricKind;
use antirion_router::bandit::{RouteBandit, Tally, of_route, seed_of, stop_arm};
use serde_json::Value;

use crate::{Runtime, Snapshot};

/// How often a node reads the store's rewards.
const EVERY: Duration = Duration::from_secs(1);
/// Joint draws estimating each arm's probability of being the best.
const DRAWS: u32 = 4000;

/// The bandits of a configuration.
fn bandits(snap: &Snapshot) -> Vec<RouteBandit> {
    snap.routing
        .routes
        .values()
        .chain(snap.routing.model_routes.values())
        .flat_map(|r| of_route(&r.tree))
        .collect()
}

/// A feedback value as a reward in [0, 1].
fn reward(kind: &MetricKind, value: &Value) -> Option<f64> {
    match kind {
        MetricKind::Boolean => value.as_bool().map(|b| if b { 1.0 } else { 0.0 }),
        MetricKind::Number {
            min: Some(min),
            max: Some(max),
        } if max > min => value.as_f64().map(|v| ((v - min) / (max - min)).clamp(0.0, 1.0)),
        _ => None,
    }
}

impl Runtime {
    /// Learns from feedback `value` of `metric` on a record that `experiment` sent to `arm`, when
    /// a bandit of `snap` learns from that metric.
    pub async fn bandit_feedback(
        &self,
        snap: &Snapshot,
        experiment: &str,
        arm: &str,
        metric: &str,
        value: &Value,
    ) {
        let learns = bandits(snap).iter().any(|b| {
            b.experiment == experiment && b.spec.metric == metric && b.arms.iter().any(|a| a == arm)
        });
        if !learns {
            return;
        }
        let Some(r) = snap
            .obs
            .feedback
            .metrics
            .get(metric)
            .and_then(|k| reward(k, value))
        else {
            return;
        };
        #[allow(clippy::cast_possible_truncation)]
        let micros = (r * 1_000_000.0).round() as i64;
        match self
            .store
            .add_bandit_reward(experiment, arm, metric, micros)
            .await
        {
            Ok(()) => self.refresh_bandits(snap).await,
            Err(e) => tracing::warn!(error = %e, "a bandit reward could not be stored"),
        }
    }

    /// Copies the store's rewards and stops into the routing state, and stops the experiments
    /// whose best arm is clear.
    pub async fn refresh_bandits(&self, snap: &Snapshot) {
        let (Ok(rewards), Ok(stops)) = (self.store.bandit_rewards().await, self.store.bandit_stops().await)
        else {
            return;
        };
        let mut learnt: HashMap<(String, String), HashMap<String, Tally>> = HashMap::new();
        for r in rewards {
            learnt.entry((r.experiment, r.metric)).or_default().insert(
                r.arm,
                Tally {
                    samples: u64::try_from(r.samples).unwrap_or(0),
                    reward: r.reward_micros as f64 / 1_000_000.0,
                },
            );
        }
        let mut stopped: HashMap<(String, String), String> = stops
            .into_iter()
            .map(|s| ((s.experiment, s.metric), s.arm))
            .collect();
        for b in bandits(snap) {
            let key = (b.experiment.clone(), b.spec.metric.clone());
            let Some(delta) = b.spec.delta.filter(|_| !stopped.contains_key(&key)) else {
                continue;
            };
            let tallies: Vec<Tally> = b
                .arms
                .iter()
                .map(|a| {
                    learnt
                        .get(&key)
                        .and_then(|m| m.get(a))
                        .copied()
                        .unwrap_or_default()
                })
                .collect();
            let Some(best) = stop_arm(&tallies, b.spec.min_samples, delta, DRAWS, seed_of(&tallies))
                .and_then(|i| b.arms.get(i))
            else {
                continue;
            };
            if self.store.stop_bandit(&key.0, &key.1, best).await.is_ok()
                && let Ok(all) = self.store.bandit_stops().await
            {
                stopped = all
                    .into_iter()
                    .map(|s| ((s.experiment, s.metric), s.arm))
                    .collect();
            }
        }
        self.state.routing.bandits.replace(learnt, stopped);
    }
}

/// Copies the store's bandit state every second while the configuration has a bandit.
pub async fn run(rt: Arc<Runtime>) {
    loop {
        tokio::select! {
            () = rt.shutdown.cancelled() => return,
            () = tokio::time::sleep(EVERY) => {}
        }
        let snap = rt.gens.load();
        if !bandits(&snap).is_empty() {
            rt.refresh_bandits(&snap).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn rewards_from_metrics() {
        assert_eq!(reward(&MetricKind::Boolean, &json!(true)), Some(1.0));
        assert_eq!(reward(&MetricKind::Boolean, &json!(false)), Some(0.0));
        let stars = MetricKind::Number {
            min: Some(1.0),
            max: Some(5.0),
        };
        assert_eq!(reward(&stars, &json!(4)), Some(0.75));
        assert_eq!(reward(&stars, &json!(9)), Some(1.0));
        assert_eq!(
            reward(
                &MetricKind::Number {
                    min: None,
                    max: Some(5.0)
                },
                &json!(4)
            ),
            None
        );
        assert_eq!(reward(&MetricKind::Text { max_bytes: 10 }, &json!("x")), None);
    }
}
