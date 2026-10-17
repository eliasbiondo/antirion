//! The rolling window of scores a `strong_fraction` node calibrates its threshold on: the last
//! `window` scores in arrival order, and the same scores sorted, so the order statistic is read
//! without sorting on every request.

use std::collections::VecDeque;

use super::Calibration;

/// The scores one node observed on this replica.
#[derive(Clone, Debug, Default)]
pub struct Window {
    arrival: VecDeque<f64>,
    sorted: Vec<f64>,
}

fn position(sorted: &[f64], w: f64) -> usize {
    sorted.partition_point(|x| x.total_cmp(&w).is_lt())
}

impl Window {
    /// The threshold for a target strong share `p`: the initial threshold until `min_samples`
    /// scores, then the score `w_(k)` with `k = min(m, ⌊(1 − p)·m⌋ + 1)` of the `m` sorted scores.
    #[must_use]
    pub fn threshold(&self, p: f64, cal: &Calibration) -> f64 {
        let m = self.sorted.len();
        if m == 0 || m < cal.min_samples {
            return cal.initial;
        }
        let k = (((1.0 - p) * m as f64).floor() as usize + 1).min(m);
        self.sorted.get(k - 1).copied().unwrap_or(cal.initial)
    }

    /// Adds a score, dropping the oldest beyond `window`.
    pub fn observe(&mut self, w: f64, window: usize) {
        self.arrival.push_back(w);
        let at = position(&self.sorted, w);
        self.sorted.insert(at, w);
        while self.arrival.len() > window.max(1) {
            let Some(old) = self.arrival.pop_front() else {
                break;
            };
            let at = position(&self.sorted, old);
            if self.sorted.get(at).is_some_and(|x| x.total_cmp(&old).is_eq()) {
                self.sorted.remove(at);
            }
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.arrival.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.arrival.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAL: Calibration = Calibration {
        window: 100,
        min_samples: 10,
        initial: 0.5,
    };

    #[test]
    fn initial_threshold_until_min_samples_then_the_order_statistic() {
        let mut w = Window::default();
        for i in 0..9 {
            w.observe(f64::from(i) / 10.0, CAL.window);
        }
        assert_eq!(w.threshold(0.3, &CAL), 0.5);
        w.observe(0.9, CAL.window);
        let t = w.threshold(0.3, &CAL);
        assert!((t - 0.7).abs() < 1e-9, "{t}");
        let above = (0..10).filter(|i| f64::from(*i) / 10.0 >= t).count();
        assert_eq!(above, 3, "exactly ⌈p·m⌉ scores are at or above τ");
    }

    #[test]
    fn the_window_forgets_its_oldest_scores() {
        let mut w = Window::default();
        for _ in 0..5 {
            w.observe(0.9, 3);
        }
        for _ in 0..3 {
            w.observe(0.1, 3);
        }
        assert_eq!(w.len(), 3);
        let cal = Calibration {
            min_samples: 1,
            ..CAL
        };
        assert_eq!(w.threshold(0.5, &cal), 0.1);
    }
}
