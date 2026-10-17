//! Degraded mode: the counter store's health, the node's local share of each counter while the
//! store is unavailable, and the replay queue of settlements that could not reach it.

use std::collections::VecDeque;

use parking_lot::Mutex;

use crate::counter::{Hold, Rule};
use crate::store::ReservationId;

/// Consecutive good probes before the store is used again.
pub const PROBES_TO_RECOVER: u8 = 3;

/// Why a request was evaluated in a degraded mode (the log record's `degraded` cause).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum DegradedCause {
    LocalShare,
    FailOpen,
    /// A settlement was queued because the store did not answer.
    StoreTimeout,
}

impl DegradedCause {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            DegradedCause::LocalShare => "local_share",
            DegradedCause::FailOpen => "fail_open",
            DegradedCause::StoreTimeout => "store_timeout",
        }
    }
}

/// The degraded components of one request: `limits` and `budgets` in `x-antirion-degraded`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Degraded {
    pub limits: Option<DegradedCause>,
    pub budgets: Option<DegradedCause>,
}

impl Degraded {
    pub(crate) fn mark(&mut self, budget: bool, cause: DegradedCause) {
        let slot = if budget {
            &mut self.budgets
        } else {
            &mut self.limits
        };
        if slot.is_none() {
            *slot = Some(cause);
        }
    }

    /// Adds the components `other` names that this one does not.
    pub fn merge(&mut self, other: Degraded) {
        self.limits = self.limits.or(other.limits);
        self.budgets = self.budgets.or(other.budgets);
    }

    /// Each degraded component with its cause.
    #[must_use]
    pub fn causes(&self) -> Vec<(&'static str, &'static str)> {
        let mut v = Vec::new();
        if let Some(c) = self.limits {
            v.push(("limits", c.as_str()));
        }
        if let Some(c) = self.budgets {
            v.push(("budgets", c.as_str()));
        }
        v
    }

    /// The component names for `x-antirion-degraded`.
    #[must_use]
    pub fn components(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.limits.is_some() {
            v.push("limits");
        }
        if self.budgets.is_some() {
            v.push("budgets");
        }
        v
    }

    #[must_use]
    pub fn is_degraded(&self) -> bool {
        self.limits.is_some() || self.budgets.is_some()
    }
}

/// Whether request-path commands go to the store: down from the first failure until
/// [`PROBES_TO_RECOVER`] probes succeed in a row.
#[derive(Debug, Default)]
pub(crate) struct Health {
    state: Mutex<(bool, u8)>,
}

impl Health {
    pub(crate) fn is_down(&self) -> bool {
        self.state.lock().0
    }

    pub(crate) fn mark_down(&self) {
        *self.state.lock() = (true, 0);
    }

    /// Records a probe; true when the store is up afterwards.
    pub(crate) fn probed(&self, ok: bool) -> bool {
        let mut s = self.state.lock();
        if !s.0 {
            return true;
        }
        if ok {
            s.1 = s.1.saturating_add(1);
            if s.1 >= PROBES_TO_RECOVER {
                *s = (false, 0);
                return true;
            }
        } else {
            s.1 = 0;
        }
        false
    }
}

/// The local form of a hold for a node's share `1/m` of the quota, and the factor its amounts
/// are multiplied by. Quotas are kept and amounts multiplied by `m`, so a fractional share stays
/// exact; concurrency takes `floor(L/m)` slots instead.
#[must_use]
pub fn local_share(h: &Hold, m: u32, budget_remaining: Option<u128>) -> Option<(Hold, u128)> {
    let m = u128::from(m.max(1));
    let mut local = h.clone();
    let scale = match &h.rule {
        Rule::Gcra(_) | Rule::Window(_) => m,
        Rule::Concurrency { limit } => {
            local.rule = Rule::Concurrency {
                limit: u64::try_from(u128::from(*limit) / m).unwrap_or(0),
            };
            1
        }
        Rule::Budget { period, .. } => {
            local.rule = Rule::Budget {
                amount: budget_remaining?,
                period: *period,
            };
            m
        }
        Rule::Rolling(w) => {
            local.rule = Rule::Rolling(crate::math::Window {
                limit: u64::try_from(budget_remaining?).unwrap_or(u64::MAX),
                ..*w
            });
            m
        }
    };
    local.amount = h.amount.saturating_mul(scale);
    Some((local, scale))
}

/// A settlement waiting for the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Queued {
    Reconcile {
        uid: ReservationId,
        node: String,
        group: u128,
        holds: Vec<Hold>,
        actual: Vec<u128>,
    },
    /// Consumption the shared store did not see: a soft budget's spend, or what a local share
    /// admitted while the store was down. Replayed as an admission of `hold` and its settlement
    /// under a derived id, so a retry never charges twice.
    Track {
        uid: ReservationId,
        node: String,
        group: u128,
        hold: Hold,
        actual: u128,
    },
}

/// A bounded queue; when full, the oldest entry is dropped and counted.
#[derive(Debug)]
pub(crate) struct ReplayQueue {
    items: Mutex<VecDeque<Queued>>,
    capacity: usize,
    dropped: Mutex<u64>,
}

impl ReplayQueue {
    pub(crate) fn new(capacity: usize) -> ReplayQueue {
        ReplayQueue {
            items: Mutex::new(VecDeque::new()),
            capacity: capacity.max(1),
            dropped: Mutex::new(0),
        }
    }

    pub(crate) fn push(&self, q: Queued) {
        let mut items = self.items.lock();
        if items.len() >= self.capacity {
            items.pop_front();
            *self.dropped.lock() += 1;
        }
        items.push_back(q);
    }

    pub(crate) fn pop(&self) -> Option<Queued> {
        self.items.lock().pop_front()
    }

    pub(crate) fn push_front(&self, q: Queued) {
        self.items.lock().push_front(q);
    }

    pub(crate) fn len(&self) -> usize {
        self.items.lock().len()
    }

    pub(crate) fn dropped(&self) -> u64 {
        *self.dropped.lock()
    }
}

#[cfg(test)]
mod tests {
    use antirion_core::{ResourceUid, UtcMicros};

    use super::*;
    use crate::counter::{CounterId, HoldMode};
    use crate::level::Level;
    use crate::math::Gcra;
    use crate::period::PeriodBounds;

    fn hold(rule: Rule, amount: u128) -> Hold {
        Hold {
            counter: CounterId::derive("t", Level::Key, ResourceUid(1), "", "x", "", &[]),
            group: 0,
            rule,
            amount,
            mode: HoldMode::Enforce,
        }
    }

    #[test]
    fn shares() {
        let g = hold(
            Rule::Gcra(Gcra {
                limit: 10,
                per_us: 1,
                burst: 10,
            }),
            1,
        );
        let (l, s) = local_share(&g, 2, None).unwrap();
        assert_eq!((l.amount, s), (2, 2));
        let c = hold(Rule::Concurrency { limit: 2 }, 1);
        let (l, _) = local_share(&c, 3, None).unwrap();
        assert_eq!(l.rule, Rule::Concurrency { limit: 0 });
        let b = hold(
            Rule::Budget {
                amount: 10,
                period: PeriodBounds {
                    start: UtcMicros(0),
                    end: None,
                },
            },
            2,
        );
        assert!(local_share(&b, 2, None).is_none());
        let (l, _) = local_share(&b, 2, Some(7)).unwrap();
        assert_eq!(
            (l.amount, l.rule.clone()),
            (
                4,
                Rule::Budget {
                    amount: 7,
                    period: PeriodBounds {
                        start: UtcMicros(0),
                        end: None
                    }
                }
            )
        );
    }

    #[test]
    fn health_needs_three_good_probes() {
        let h = Health::default();
        assert!(!h.is_down());
        h.mark_down();
        assert!(!h.probed(true));
        assert!(!h.probed(false));
        assert!(!h.probed(true));
        assert!(!h.probed(true));
        assert!(h.probed(true));
        assert!(!h.is_down());
    }

    #[test]
    fn queue_is_bounded() {
        let q = ReplayQueue::new(2);
        for i in 0..3 {
            q.push(Queued::Reconcile {
                uid: ReservationId(i),
                node: "n".into(),
                group: 0,
                holds: vec![],
                actual: vec![],
            });
        }
        assert_eq!((q.len(), q.dropped()), (2, 1));
        assert!(matches!(
            q.pop(),
            Some(Queued::Reconcile {
                uid: ReservationId(1),
                ..
            })
        ));
        let mut d = Degraded::default();
        d.mark(false, DegradedCause::LocalShare);
        d.mark(false, DegradedCause::FailOpen);
        assert_eq!(d.limits, Some(DegradedCause::LocalShare));
        assert_eq!(d.components(), vec!["limits"]);
    }
}
