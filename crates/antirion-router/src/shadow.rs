//! Shadow comparisons of this replica: for each mirroring node, whether each copy's answer diverged
//! from the served one, and how its latency and cost compared, over the last 24 hours.

use std::collections::{HashMap, VecDeque};

use antirion_core::UtcMicros;
use parking_lot::Mutex;

/// How long comparisons count.
const WINDOW_US: i64 = 24 * 3_600 * 1_000_000;
/// Comparisons kept per node, whatever their age.
const MAX_SAMPLES: usize = 100_000;

/// One copy compared with the answer the client got.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Compared {
    pub at: UtcMicros,
    pub divergent: bool,
    /// The copy's latency minus the served answer's, in milliseconds.
    pub latency_delta_ms: f64,
    /// The copy's cost minus the served answer's, in USD.
    pub cost_delta_usd: f64,
}

/// What route state reports of a mirror.
#[derive(Clone, Debug, PartialEq)]
pub struct ShadowView {
    pub comparisons: u64,
    pub divergence_rate: Option<f64>,
    pub latency_delta_p50_ms: Option<f64>,
    /// The summed cost difference over the window.
    pub cost_delta_usd: f64,
}

#[derive(Debug, Default)]
pub struct Shadows {
    entries: Mutex<HashMap<String, VecDeque<Compared>>>,
}

fn trim(samples: &mut VecDeque<Compared>, now: UtcMicros) {
    let horizon = now.0.saturating_sub(WINDOW_US);
    while samples.front().is_some_and(|s| s.at.0 < horizon) || samples.len() > MAX_SAMPLES {
        samples.pop_front();
    }
}

impl Shadows {
    /// Records a comparison for the mirroring node `node`.
    pub fn record(&self, node: &str, c: Compared) {
        let mut all = self.entries.lock();
        let samples = all.entry(node.to_owned()).or_default();
        samples.push_back(c);
        trim(samples, c.at);
    }

    pub fn view(&self, node: &str, now: UtcMicros) -> ShadowView {
        let mut all = self.entries.lock();
        let Some(samples) = all.get_mut(node) else {
            return ShadowView {
                comparisons: 0,
                divergence_rate: None,
                latency_delta_p50_ms: None,
                cost_delta_usd: 0.0,
            };
        };
        trim(samples, now);
        let n = samples.len();
        let divergent = samples.iter().filter(|s| s.divergent).count();
        let mut deltas: Vec<f64> = samples.iter().map(|s| s.latency_delta_ms).collect();
        deltas.sort_by(f64::total_cmp);
        #[allow(clippy::cast_precision_loss)]
        ShadowView {
            comparisons: n as u64,
            divergence_rate: (n > 0).then(|| divergent as f64 / n as f64),
            latency_delta_p50_ms: deltas.get(n / 2).copied(),
            cost_delta_usd: samples.iter().map(|s| s.cost_delta_usd).sum(),
        }
    }

    pub fn reset(&self) {
        self.entries.lock().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparisons_are_summarised_over_a_day() {
        let s = Shadows::default();
        for i in 0..10 {
            s.record(
                "n",
                Compared {
                    at: UtcMicros(i),
                    divergent: i % 2 == 0,
                    latency_delta_ms: i as f64,
                    cost_delta_usd: 0.5,
                },
            );
        }
        let v = s.view("n", UtcMicros(10));
        assert_eq!(v.comparisons, 10);
        assert_eq!(v.divergence_rate, Some(0.5));
        assert_eq!(v.latency_delta_p50_ms, Some(5.0));
        assert!((v.cost_delta_usd - 5.0).abs() < 1e-9);
        assert_eq!(s.view("n", UtcMicros(WINDOW_US + 100)).comparisons, 0);
    }
}
