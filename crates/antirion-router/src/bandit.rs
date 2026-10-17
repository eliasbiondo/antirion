//! Bandit experiments (`algorithm: thompson` on an experiment node): the arms take turns until each
//! has `min_samples_per_arm` feedback samples of the node's metric, then each request goes to the
//! arm whose draw from its Beta posterior (`1 + reward`, `1 + samples − reward`) is highest. With
//! `stop_after`, once one arm's posterior probability of being the best exceeds `1 − delta`, every
//! later request goes to it. What the arms learnt and the stops come from the store (every node
//! reads the same), so this module keeps only a node's copy of them and its turns.

use std::collections::HashMap;

use antirion_core::SplitMix64;
use parking_lot::{Mutex, RwLock};
use serde_json::Value;

/// The bandit of an experiment node.
#[derive(Clone, Debug, PartialEq)]
pub struct BanditSpec {
    /// The feedback metric whose values are the rewards.
    pub metric: String,
    pub min_samples: u64,
    /// `stop_after.delta`, when the bandit stops.
    pub delta: Option<f64>,
    /// The arms it chooses among (child ids); all children with a weight when absent.
    pub arms: Option<Vec<String>>,
}

impl BanditSpec {
    /// Reads the bandit members of a `weighted` node.
    #[must_use]
    pub fn from_node(node: &serde_json::Map<String, Value>) -> BanditSpec {
        BanditSpec {
            metric: node
                .get("metric")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            min_samples: node
                .get("min_samples_per_arm")
                .and_then(Value::as_u64)
                .unwrap_or(10)
                .max(1),
            delta: node
                .get("stop_after")
                .map(|s| s.get("delta").and_then(Value::as_f64).unwrap_or(DEFAULT_DELTA)),
            arms: node
                .get("arms")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect()),
        }
    }
}

/// A bandit of a compiled route: its experiment, its spec and its arms.
#[derive(Clone, Debug)]
pub struct RouteBandit {
    pub experiment: String,
    pub spec: std::sync::Arc<BanditSpec>,
    pub arms: Vec<String>,
}

/// The bandits of a route tree.
#[must_use]
pub fn of_route(route: &crate::tree::CompiledRoute) -> Vec<RouteBandit> {
    route
        .nodes
        .iter()
        .filter_map(|n| match &n.kind {
            crate::tree::NodeKind::Weighted {
                algorithm: crate::tree::Algorithm::Thompson(spec),
                experiment: Some(e),
                ..
            } => {
                let arms = n
                    .children
                    .iter()
                    .filter_map(|&c| route.node(c))
                    .filter(|c| {
                        c.segment
                            .as_ref()
                            .is_some_and(|s| spec.arms.as_ref().map_or(c.weight > 0, |a| a.contains(s)))
                    })
                    .filter_map(|c| c.segment.clone())
                    .collect();
                Some(RouteBandit {
                    experiment: e.name.clone(),
                    spec: std::sync::Arc::clone(spec),
                    arms,
                })
            }
            _ => None,
        })
        .collect()
}

/// `stop_after.delta` when it is not given.
pub const DEFAULT_DELTA: f64 = 0.05;

/// What an arm learnt for a metric.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Tally {
    pub samples: u64,
    /// The summed reward, each sample's in [0, 1].
    pub reward: f64,
}

impl Tally {
    fn posterior(self) -> (f64, f64) {
        let reward = self.reward.clamp(0.0, self.samples as f64);
        (1.0 + reward, 1.0 + self.samples as f64 - reward)
    }
}

type Key = (String, String);

/// The bandits as this node knows them.
#[derive(Debug, Default)]
pub struct BanditState {
    /// By experiment and metric, by arm.
    learnt: RwLock<HashMap<Key, HashMap<String, Tally>>>,
    /// The arm each stopped experiment goes to, by experiment and metric.
    stops: RwLock<HashMap<Key, String>>,
    /// The next turn of each experiment's arms while they learn.
    turns: Mutex<HashMap<String, u64>>,
}

impl BanditState {
    /// Replaces what this node knows with what the store holds.
    pub fn replace(&self, learnt: HashMap<Key, HashMap<String, Tally>>, stops: HashMap<Key, String>) {
        *self.learnt.write() = learnt;
        *self.stops.write() = stops;
    }

    /// What `experiment`'s arms learnt for `metric`, in the order of `arms`.
    #[must_use]
    pub fn tallies(&self, experiment: &str, metric: &str, arms: &[&str]) -> Vec<Tally> {
        let learnt = self.learnt.read();
        let of = learnt.get(&(experiment.to_owned(), metric.to_owned()));
        arms.iter()
            .map(|a| of.and_then(|m| m.get(*a)).copied().unwrap_or_default())
            .collect()
    }

    /// The arm `experiment` stopped on for `metric`.
    #[must_use]
    pub fn stopped(&self, experiment: &str, metric: &str) -> Option<String> {
        self.stops
            .read()
            .get(&(experiment.to_owned(), metric.to_owned()))
            .cloned()
    }

    /// The arm (an index of `arms`) a request goes to. A dry run does not take a turn.
    pub fn choose(
        &self,
        experiment: &str,
        spec: &BanditSpec,
        arms: &[&str],
        rng: &mut SplitMix64,
        dry: bool,
    ) -> Option<usize> {
        if arms.is_empty() {
            return None;
        }
        if let Some(stopped) = self.stopped(experiment, &spec.metric)
            && let Some(i) = arms.iter().position(|a| *a == stopped)
        {
            return Some(i);
        }
        let tallies = self.tallies(experiment, &spec.metric, arms);
        if tallies.iter().any(|t| t.samples < spec.min_samples) {
            let mut turns = self.turns.lock();
            let turn = turns.entry(experiment.to_owned()).or_insert(0);
            let n = u64::try_from(arms.len()).unwrap_or(u64::MAX);
            let i = usize::try_from(*turn % n).unwrap_or(0);
            if !dry {
                *turn = turn.wrapping_add(1);
            }
            return Some(i);
        }
        let draws: Vec<f64> = tallies
            .iter()
            .map(|t| {
                let (a, b) = t.posterior();
                beta(a, b, rng)
            })
            .collect();
        argmax(&draws)
    }

    /// Forgets everything (test mode).
    pub fn clear(&self) {
        self.learnt.write().clear();
        self.stops.write().clear();
        self.turns.lock().clear();
    }
}

fn argmax(v: &[f64]) -> Option<usize> {
    v.iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(i, _)| i)
}

/// The arm whose posterior probability of being the best exceeds `1 − delta`, once every arm has
/// `min_samples`: estimated from `draws` joint draws of a generator seeded with `seed`, so every
/// node reading the same tallies reaches the same answer.
#[must_use]
pub fn stop_arm(tallies: &[Tally], min_samples: u64, delta: f64, draws: u32, seed: u64) -> Option<usize> {
    if tallies.len() < 2 || tallies.iter().any(|t| t.samples < min_samples) {
        return None;
    }
    let mut rng = SplitMix64::new(seed);
    let mut wins = vec![0u32; tallies.len()];
    for _ in 0..draws {
        let sampled: Vec<f64> = tallies
            .iter()
            .map(|t| {
                let (a, b) = t.posterior();
                beta(a, b, &mut rng)
            })
            .collect();
        if let Some(w) = argmax(&sampled).and_then(|i| wins.get_mut(i)) {
            *w += 1;
        }
    }
    let threshold = (1.0 - delta) * f64::from(draws);
    wins.iter().position(|w| f64::from(*w) > threshold)
}

/// A seed for [`stop_arm`] from what the arms learnt.
#[must_use]
pub fn seed_of(tallies: &[Tally]) -> u64 {
    tallies.iter().fold(0x9e37_79b9_7f4a_7c15, |acc, t| {
        acc.rotate_left(17) ^ t.samples ^ t.reward.to_bits()
    })
}

fn uniform(rng: &mut SplitMix64) -> f64 {
    ((rng.next_u64() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
}

fn normal(rng: &mut SplitMix64) -> f64 {
    let (u, v) = (uniform(rng), uniform(rng));
    (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
}

/// A Gamma(`shape`, 1) draw for `shape ≥ 1` (Marsaglia and Tsang).
fn gamma(shape: f64, rng: &mut SplitMix64) -> f64 {
    let d = shape.max(1.0) - 1.0 / 3.0;
    let c = 1.0 / (9.0 * d).sqrt();
    loop {
        let x = normal(rng);
        let v = 1.0 + c * x;
        if v <= 0.0 {
            continue;
        }
        let v = v * v * v;
        let u = uniform(rng);
        if u.ln() < 0.5 * x * x + d - d * v + d * v.ln() {
            return d * v;
        }
    }
}

/// A Beta(`a`, `b`) draw, both at least 1.
fn beta(a: f64, b: f64, rng: &mut SplitMix64) -> f64 {
    let x = gamma(a, rng);
    let y = gamma(b, rng);
    if x + y > 0.0 { x / (x + y) } else { 0.5 }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn spec(min: u64) -> BanditSpec {
        BanditSpec {
            metric: "good".into(),
            min_samples: min,
            delta: Some(0.05),
            arms: None,
        }
    }

    fn learnt(a: Tally, b: Tally) -> HashMap<Key, HashMap<String, Tally>> {
        HashMap::from([(
            ("e".to_owned(), "good".to_owned()),
            HashMap::from([("a".to_owned(), a), ("b".to_owned(), b)]),
        )])
    }

    #[test]
    fn arms_take_turns_until_each_has_its_samples() {
        let s = BanditState::default();
        let mut rng = SplitMix64::new(1);
        let picks: Vec<usize> = (0..4)
            .map(|_| s.choose("e", &spec(3), &["a", "b"], &mut rng, false).unwrap())
            .collect();
        assert_eq!(picks, vec![0, 1, 0, 1]);
        assert_eq!(s.choose("e", &spec(3), &["a", "b"], &mut rng, true), Some(0));
        assert_eq!(
            s.choose("e", &spec(3), &["a", "b"], &mut rng, true),
            Some(0),
            "a dry run takes no turn"
        );
    }

    #[test]
    fn thompson_sampling_favours_the_better_arm() {
        let s = BanditState::default();
        s.replace(
            learnt(
                Tally {
                    samples: 40,
                    reward: 36.0,
                },
                Tally {
                    samples: 40,
                    reward: 8.0,
                },
            ),
            HashMap::new(),
        );
        let mut rng = SplitMix64::new(7);
        let a = (0..1000)
            .filter(|_| s.choose("e", &spec(10), &["a", "b"], &mut rng, false) == Some(0))
            .count();
        assert!(a > 950, "{a}");
    }

    #[test]
    fn a_stopped_experiment_always_goes_to_its_arm() {
        let s = BanditState::default();
        s.replace(
            learnt(Tally::default(), Tally::default()),
            HashMap::from([(("e".to_owned(), "good".to_owned()), "b".to_owned())]),
        );
        let mut rng = SplitMix64::new(3);
        assert!((0..50).all(|_| s.choose("e", &spec(10), &["a", "b"], &mut rng, false) == Some(1)));
    }

    #[test]
    fn stopping_needs_the_samples_and_a_clear_winner() {
        let strong = [
            Tally {
                samples: 20,
                reward: 20.0,
            },
            Tally {
                samples: 20,
                reward: 0.0,
            },
        ];
        let seed = seed_of(&strong);
        assert_eq!(stop_arm(&strong, 10, 0.05, 2000, seed), Some(0));
        assert_eq!(
            stop_arm(&strong, 30, 0.05, 2000, seed),
            None,
            "not enough samples yet"
        );
        let close = [
            Tally {
                samples: 20,
                reward: 10.0,
            },
            Tally {
                samples: 20,
                reward: 11.0,
            },
        ];
        assert_eq!(stop_arm(&close, 10, 0.05, 2000, seed_of(&close)), None);
        assert_eq!(
            stop_arm(&strong, 10, 0.05, 2000, seed),
            stop_arm(&strong, 10, 0.05, 2000, seed),
            "the same tallies stop the same way on every node"
        );
    }

    #[test]
    fn beta_draws_have_the_right_mean() {
        let mut rng = SplitMix64::new(11);
        let n = 20_000;
        let mean = (0..n).map(|_| beta(3.0, 7.0, &mut rng)).sum::<f64>() / f64::from(n);
        assert!((mean - 0.3).abs() < 0.01, "{mean}");
    }

    #[test]
    fn spec_from_a_node() {
        let node = json!({"metric": "good", "min_samples_per_arm": 5, "stop_after": {}, "arms": ["a", "b"]});
        let s = BanditSpec::from_node(node.as_object().unwrap());
        assert_eq!(
            s,
            BanditSpec {
                metric: "good".into(),
                min_samples: 5,
                delta: Some(DEFAULT_DELTA),
                arms: Some(vec!["a".into(), "b".into()])
            }
        );
    }
}
