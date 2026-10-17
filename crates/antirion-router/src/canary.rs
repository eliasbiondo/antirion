//! Canary state of this replica: when each canary's ramp started, the candidate's recent outcomes
//! against its error budget, and whether the ramp was held or rolled back. Keyed by the canary's
//! key, so a changed canary starts over.

use std::collections::{HashMap, VecDeque};

use antirion_core::UtcMicros;
use parking_lot::Mutex;

use crate::tree::Canary;

/// Outcomes an error budget needs before it can trip.
pub const MIN_REQUESTS: usize = 20;
/// Outcomes kept per canary, whatever its window.
const MAX_OUTCOMES: usize = 100_000;

/// Where a canary's ramp is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Ramping,
    /// Stopped at the weight it had when the budget tripped.
    Held(u32),
    RolledBack,
}

#[derive(Debug)]
struct Entry {
    started: UtcMicros,
    phase: Phase,
    outcomes: VecDeque<(UtcMicros, bool)>,
}

/// What route state reports of a canary.
#[derive(Clone, Debug, PartialEq)]
pub struct CanaryView {
    /// `ramping`, `held`, `rolled_back` or `complete`.
    pub state: &'static str,
    /// The candidate's share, in percent.
    pub weight: u32,
    /// The candidate's error rate over the budget's window, when it has outcomes there.
    pub error_rate: Option<f64>,
}

/// A transition the error budget caused, for its event.
#[derive(Clone, Debug, PartialEq)]
pub struct Tripped {
    pub rolled_back: bool,
    pub weight: u32,
    pub error_rate: f64,
}

/// Every canary of this replica.
#[derive(Debug, Default)]
pub struct Canaries {
    entries: Mutex<HashMap<String, Entry>>,
}

fn entry<'a>(all: &'a mut HashMap<String, Entry>, key: &str, now: UtcMicros) -> &'a mut Entry {
    all.entry(key.to_owned()).or_insert_with(|| Entry {
        started: now,
        phase: Phase::Ramping,
        outcomes: VecDeque::new(),
    })
}

fn rate(outcomes: &VecDeque<(UtcMicros, bool)>) -> Option<f64> {
    if outcomes.is_empty() {
        return None;
    }
    let failed = outcomes.iter().filter(|(_, ok)| !ok).count();
    #[allow(clippy::cast_precision_loss)]
    let r = failed as f64 / outcomes.len() as f64;
    Some(r)
}

impl Canaries {
    /// Starts the ramp of every canary of a newly applied configuration that is not started, and
    /// forgets the canaries it no longer has.
    pub fn applied<'a>(&self, keys: impl IntoIterator<Item = &'a str>, now: UtcMicros) {
        let keys: Vec<&str> = keys.into_iter().collect();
        let mut all = self.entries.lock();
        all.retain(|k, _| keys.contains(&k.as_str()));
        for k in keys {
            entry(&mut all, k, now);
        }
    }

    /// The candidate's share now, in percent.
    pub fn weight(&self, c: &Canary, now: UtcMicros) -> u32 {
        let mut all = self.entries.lock();
        let e = entry(&mut all, &c.key, now);
        match e.phase {
            Phase::Ramping => c.ramp(now.since(e.started)),
            Phase::Held(w) => w,
            Phase::RolledBack => 0,
        }
    }

    /// Records one outcome of the candidate; returns the transition when it tripped the budget.
    pub fn record(&self, c: &Canary, ok: bool, now: UtcMicros) -> Option<Tripped> {
        let budget = c.budget.as_ref()?;
        let mut all = self.entries.lock();
        let e = entry(&mut all, &c.key, now);
        e.outcomes.push_back((now, ok));
        let horizon = UtcMicros(
            now.0
                .saturating_sub(i64::try_from(budget.window.as_micros()).unwrap_or(i64::MAX)),
        );
        while e.outcomes.front().is_some_and(|(t, _)| *t < horizon) || e.outcomes.len() > MAX_OUTCOMES {
            e.outcomes.pop_front();
        }
        if e.phase != Phase::Ramping || e.outcomes.len() < MIN_REQUESTS {
            return None;
        }
        let error_rate = rate(&e.outcomes)?;
        if error_rate <= budget.max_error_rate {
            return None;
        }
        let weight = c.ramp(now.since(e.started));
        e.phase = if budget.rollback {
            Phase::RolledBack
        } else {
            Phase::Held(weight)
        };
        Some(Tripped {
            rolled_back: budget.rollback,
            weight: if budget.rollback { 0 } else { weight },
            error_rate,
        })
    }

    pub fn view(&self, c: &Canary, now: UtcMicros) -> CanaryView {
        let mut all = self.entries.lock();
        let e = entry(&mut all, &c.key, now);
        let elapsed = now.since(e.started);
        let (state, weight) = match e.phase {
            Phase::Ramping if c.complete(elapsed) => ("complete", c.ramp(elapsed)),
            Phase::Ramping => ("ramping", c.ramp(elapsed)),
            Phase::Held(w) => ("held", w),
            Phase::RolledBack => ("rolled_back", 0),
        };
        let horizon = c.budget.as_ref().map(|b| {
            UtcMicros(
                now.0
                    .saturating_sub(i64::try_from(b.window.as_micros()).unwrap_or(i64::MAX)),
            )
        });
        let recent: VecDeque<(UtcMicros, bool)> = e
            .outcomes
            .iter()
            .filter(|(t, _)| horizon.is_none_or(|h| *t >= h))
            .copied()
            .collect();
        CanaryView {
            state,
            weight,
            error_rate: rate(&recent),
        }
    }

    pub fn reset(&self) {
        self.entries.lock().clear();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::tree::ErrorBudget;

    fn canary(rollback: bool) -> Canary {
        Canary {
            key: "k".into(),
            child: 1,
            steps: vec![(Duration::ZERO, 10), (Duration::from_secs(3600), 50)],
            budget: Some(ErrorBudget {
                max_error_rate: 0.2,
                window: Duration::from_secs(300),
                rollback,
            }),
        }
    }

    const HOUR: i64 = 3_600_000_000;

    #[test]
    fn the_ramp_follows_its_steps() {
        let c = canary(true);
        let book = Canaries::default();
        book.applied(["k"], UtcMicros(0));
        assert_eq!(book.weight(&c, UtcMicros(1)), 10);
        assert_eq!(book.weight(&c, UtcMicros(HOUR)), 50);
        assert_eq!(book.view(&c, UtcMicros(HOUR)).state, "complete");
        book.applied(["other"], UtcMicros(HOUR));
        book.applied(["k"], UtcMicros(HOUR));
        assert_eq!(
            book.weight(&c, UtcMicros(HOUR + 1)),
            10,
            "re-applied: the ramp restarts"
        );
    }

    #[test]
    fn the_budget_rolls_back_or_holds_after_enough_requests() {
        for rollback in [true, false] {
            let c = canary(rollback);
            let book = Canaries::default();
            book.applied(["k"], UtcMicros(0));
            let mut tripped = None;
            for i in 0..MIN_REQUESTS {
                tripped = book.record(&c, i % 2 == 0, UtcMicros(1_000 + i as i64));
                if i + 1 < MIN_REQUESTS {
                    assert!(tripped.is_none(), "not before {MIN_REQUESTS} requests");
                }
            }
            let t = tripped.expect("tripped at the minimum");
            assert_eq!(t.rolled_back, rollback);
            let v = book.view(&c, UtcMicros(2_000));
            assert_eq!(v.state, if rollback { "rolled_back" } else { "held" });
            assert_eq!(v.weight, if rollback { 0 } else { 10 });
            assert_eq!(
                book.weight(&c, UtcMicros(HOUR * 2)),
                v.weight,
                "the ramp no longer moves"
            );
            assert!(
                book.record(&c, false, UtcMicros(3_000)).is_none(),
                "it trips once"
            );
        }
    }
}
