//! The cross-request retry budget of each deployment on this node: over a sliding window, the
//! retries onto a deployment (attempts that re-send a request the deployment already failed) may
//! not exceed `max(min_per_second × window, ratio × first attempts)`. A request's first attempt on
//! a deployment, whether it is the request's first attempt or a move from another deployment, is
//! a first attempt there.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use antirion_core::UtcMicros;
use parking_lot::Mutex;
use serde_json::Value;

/// The window the budget is computed over.
pub const WINDOW: Duration = Duration::from_secs(10);

const BUCKETS: i64 = 10;
const BUCKET_US: i64 = 1_000_000;

/// `resilience.retry_budget`.
#[derive(Clone, Debug, PartialEq)]
pub struct BudgetSettings {
    pub ratio: f64,
    pub min_per_second: u32,
}

impl Default for BudgetSettings {
    fn default() -> BudgetSettings {
        BudgetSettings {
            ratio: 0.2,
            min_per_second: 10,
        }
    }
}

impl BudgetSettings {
    /// Reads `resilience.retry_budget` from a resolved document.
    #[must_use]
    pub fn from_doc(doc: &Value) -> BudgetSettings {
        let d = BudgetSettings::default();
        let b = doc.pointer("/resilience/retry_budget");
        BudgetSettings {
            ratio: b
                .and_then(|b| b.get("ratio"))
                .and_then(Value::as_f64)
                .unwrap_or(d.ratio),
            min_per_second: b
                .and_then(|b| b.get("min_per_second"))
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(d.min_per_second),
        }
    }

    /// The retries allowed in a window holding `first` first attempts.
    #[must_use]
    pub fn allowance(&self, first: u64) -> u64 {
        let floor = u64::from(self.min_per_second).saturating_mul(WINDOW.as_secs());
        let share = (self.ratio.clamp(0.0, 1.0) * first as f64).floor() as u64;
        floor.max(share)
    }
}

#[derive(Clone, Copy, Debug)]
struct Bucket {
    index: i64,
    first: u64,
    retries: u64,
}

#[derive(Debug, Default)]
struct Window {
    buckets: VecDeque<Bucket>,
}

impl Window {
    fn prune(&mut self, now: UtcMicros) -> i64 {
        let index = now.0.div_euclid(BUCKET_US);
        while self.buckets.front().is_some_and(|b| b.index <= index - BUCKETS) {
            self.buckets.pop_front();
        }
        index
    }

    fn add(&mut self, now: UtcMicros, first: u64, retries: u64) {
        let index = self.prune(now);
        match self.buckets.back_mut() {
            Some(b) if b.index == index => {
                b.first = b.first.saturating_add(first);
                b.retries = b.retries.saturating_add(retries);
            }
            _ => self.buckets.push_back(Bucket {
                index,
                first,
                retries,
            }),
        }
    }

    fn totals(&self) -> (u64, u64) {
        self.buckets.iter().fold((0, 0), |(f, r), b| {
            (f.saturating_add(b.first), r.saturating_add(b.retries))
        })
    }
}

/// The retry budgets of every deployment of this node.
#[derive(Debug, Default)]
pub struct RetryBudget {
    windows: Mutex<HashMap<String, Window>>,
}

impl RetryBudget {
    /// Counts a first attempt of a request on deployment `d`.
    pub fn first(&self, d: &str, now: UtcMicros) {
        let mut w = self.windows.lock();
        w.entry(d.to_owned()).or_default().add(now, 1, 0);
    }

    /// Takes one retry onto deployment `d` from its budget; false when the budget is spent.
    pub fn retry(&self, d: &str, s: &BudgetSettings, now: UtcMicros) -> bool {
        let mut w = self.windows.lock();
        let window = w.entry(d.to_owned()).or_default();
        window.prune(now);
        let (first, retries) = window.totals();
        if retries >= s.allowance(first) {
            return false;
        }
        window.add(now, 0, 1);
        true
    }

    /// Forgets every window.
    pub fn clear(&self) {
        self.windows.lock().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: f64) -> UtcMicros {
        UtcMicros((secs * 1e6) as i64)
    }

    #[test]
    fn retries_are_capped_by_the_floor_and_the_ratio() {
        let s = BudgetSettings {
            ratio: 0.2,
            min_per_second: 1,
        };
        let b = RetryBudget::default();
        let allowed = (0..30).filter(|_| b.retry("a", &s, at(100.0))).count();
        assert_eq!(allowed, 10, "the floor is min_per_second over the window");
        for _ in 0..100 {
            b.first("a", at(100.5));
        }
        let more = (0..30).filter(|_| b.retry("a", &s, at(101.0))).count();
        assert_eq!(more, 10, "0.2 x 100 first attempts = 20 retries in the window");
        assert!(b.retry("b", &s, at(101.0)), "budgets are per deployment");
    }

    #[test]
    fn the_window_slides() {
        let s = BudgetSettings {
            ratio: 0.0,
            min_per_second: 1,
        };
        let b = RetryBudget::default();
        assert_eq!((0..20).filter(|_| b.retry("a", &s, at(50.0))).count(), 10);
        assert!(!b.retry("a", &s, at(59.5)));
        assert!(b.retry("a", &s, at(60.0)), "the first bucket left the window");
    }

    #[test]
    fn settings_from_the_document() {
        let s = BudgetSettings::from_doc(
            &serde_json::json!({"resilience": {"retry_budget": {"min_per_second": 0}}}),
        );
        assert_eq!(s.min_per_second, 0);
        assert!((s.ratio - 0.2).abs() < f64::EPSILON);
        assert_eq!(s.allowance(0), 0);
        assert_eq!(BudgetSettings::default().allowance(1000), 200);
    }
}
