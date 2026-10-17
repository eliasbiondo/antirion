//! Exact counter arithmetic shared by every counter store. The Lua library of the Valkey store
//! mirrors these functions on decimal-string integers; a differential test keeps them equal.
//!
//! GCRA keeps `avail` = available units × `T` (the window in µs), so refill is `+ L` per µs and
//! nothing is ever divided on the decision path. Times are microseconds since the Unix epoch.

#![deny(clippy::arithmetic_side_effects)]

use std::collections::BTreeMap;

/// The wait a concurrency refusal reports.
pub const CONCURRENCY_WAIT_US: u64 = 1_000_000;

/// Whether a reservation fits a counter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fit<S> {
    /// It fits; the state to store if every counter of the request fits.
    Admit(S),
    /// It does not fit now; microseconds until it would (absent for budgets).
    Wait(Option<u64>),
    /// It can never fit.
    Exceeds,
}

fn ceil_div_i(n: i128, d: i128) -> i128 {
    if d <= 0 {
        return i128::MAX;
    }
    let q = n.checked_div(d).unwrap_or(i128::MAX);
    if n.checked_rem(d).unwrap_or(0) > 0 {
        q.saturating_add(1)
    } else {
        q
    }
}

fn to_u64(v: i128) -> u64 {
    u64::try_from(v.max(0)).unwrap_or(u64::MAX)
}

/// GCRA parameters: `limit` units per `per_us`, at most `burst` at once.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Gcra {
    pub limit: u64,
    pub per_us: u64,
    pub burst: u64,
}

/// GCRA state. `burst` and `per_us` are the burst and window in force at the last update: a
/// changed window keeps the available units (the amount is rescaled to the new `T`), and a changed
/// burst moves the available amount by the difference, so the amount already consumed is kept
/// (lowering a limit below current usage refuses the next request). `per_us` is 0 for a state
/// written before the window was recorded.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct GcraState {
    pub avail: i128,
    pub at_us: i64,
    pub burst: u64,
    pub per_us: u64,
}

impl Gcra {
    fn t(self) -> i128 {
        i128::from(self.per_us)
    }

    fn cap(self) -> i128 {
        i128::from(self.burst).saturating_mul(self.t())
    }

    /// The available amount (× T) at `now`: a first sight is a full bucket; a changed burst is
    /// applied, then refill up to `B·T`.
    #[must_use]
    pub fn refilled(self, s: Option<GcraState>, now: i64) -> i128 {
        let Some(s) = s else {
            return self.cap();
        };
        let mut avail = s.avail;
        if s.per_us != 0 && s.per_us != self.per_us {
            avail = avail
                .saturating_mul(self.t())
                .checked_div(i128::from(s.per_us))
                .unwrap_or(avail);
        }
        let change = i128::from(self.burst).saturating_sub(i128::from(s.burst));
        let avail = avail.saturating_add(change.saturating_mul(self.t()));
        let dt = i128::from(now.saturating_sub(s.at_us).max(0));
        avail
            .saturating_add(dt.saturating_mul(i128::from(self.limit)))
            .min(self.cap())
    }

    /// Whether `rho` units fit at `now`.
    #[must_use]
    pub fn admit(self, s: Option<GcraState>, now: i64, rho: u128) -> Fit<GcraState> {
        if rho > u128::from(self.burst) {
            return Fit::Exceeds;
        }
        let a = self.refilled(s, now);
        let need = i128::try_from(rho).unwrap_or(i128::MAX).saturating_mul(self.t());
        if need <= a {
            Fit::Admit(self.state(a.saturating_sub(need), now))
        } else {
            let wait = ceil_div_i(need.saturating_sub(a), i128::from(self.limit));
            Fit::Wait(Some(to_u64(wait)))
        }
    }

    fn state(self, avail: i128, now: i64) -> GcraState {
        GcraState {
            avail,
            at_us: now,
            burst: self.burst,
            per_us: self.per_us,
        }
    }

    /// Takes `rho` units whatever is available (shadow holds, extra charges).
    #[must_use]
    pub fn force(self, s: Option<GcraState>, now: i64, rho: u128) -> GcraState {
        self.settle(s, now, 0, rho)
    }

    /// Replaces a hold of `rho` by `actual`: a refund capped at the burst, or an extra charge that
    /// may leave a debt.
    #[must_use]
    pub fn settle(self, s: Option<GcraState>, now: i64, rho: u128, actual: u128) -> GcraState {
        let a = self.refilled(s, now);
        let t = self.t();
        let units = |v: u128| i128::try_from(v).unwrap_or(i128::MAX).saturating_mul(t);
        let next = if actual <= rho {
            a.saturating_add(units(rho.saturating_sub(actual)))
                .min(self.cap())
        } else {
            a.saturating_sub(units(actual.saturating_sub(rho)))
        };
        self.state(next, now)
    }

    /// Whole units available at `now` (0 when in debt), and microseconds until the bucket is full.
    #[must_use]
    pub fn view(self, s: Option<GcraState>, now: i64) -> (u128, u64) {
        let a = self.refilled(s, now);
        let remaining = u128::try_from(a.max(0).checked_div(self.t()).unwrap_or(0)).unwrap_or(0);
        let missing = self.cap().saturating_sub(a);
        let full_in = if missing <= 0 || self.limit == 0 {
            0
        } else {
            to_u64(ceil_div_i(missing, i128::from(self.limit)))
        };
        (remaining, full_in)
    }
}

/// Sliding-window parameters: `limit` units in any window of `per_us`, counted in `precision`
/// sub-windows.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Window {
    pub limit: u64,
    pub per_us: u64,
    pub precision: u32,
}

/// Charges by sub-window index.
pub type WindowState = BTreeMap<i64, u128>;

impl Window {
    /// The sub-window a charge made at `now` lives in: `floor(now · P / T)`.
    #[must_use]
    pub fn slot(self, now: i64) -> i64 {
        let n = i128::from(now).saturating_mul(i128::from(self.precision));
        let k = n.checked_div_euclid(i128::from(self.per_us)).unwrap_or(0);
        i64::try_from(k).unwrap_or(i64::MAX)
    }

    /// The first instant at which charges of sub-window `k` have expired: `(k+1)·T/P + T`,
    /// rounded up to the microsecond.
    #[must_use]
    pub fn expiry(self, k: i64) -> i64 {
        let t = i128::from(self.per_us);
        let p = i128::from(self.precision);
        let n = i128::from(k)
            .saturating_add(1)
            .saturating_mul(t)
            .saturating_add(t.saturating_mul(p));
        i64::try_from(ceil_div_i(n, p)).unwrap_or(i64::MAX)
    }

    /// Drops expired sub-windows.
    pub fn prune(self, s: &mut WindowState, now: i64) {
        s.retain(|k, v| *v > 0 && now < self.expiry(*k));
    }

    fn sum(s: &WindowState) -> u128 {
        s.values().fold(0u128, |a, v| a.saturating_add(*v))
    }

    /// Whether `rho` fits at `now`; on admit, the pruned state with the charge and its sub-window.
    #[must_use]
    pub fn admit(self, mut s: WindowState, now: i64, rho: u128) -> Fit<(WindowState, i64)> {
        if rho > u128::from(self.limit) {
            return Fit::Exceeds;
        }
        self.prune(&mut s, now);
        let sum = Self::sum(&s);
        let limit = u128::from(self.limit);
        if sum.saturating_add(rho) <= limit {
            let k = self.slot(now);
            let slot = s.entry(k).or_insert(0);
            *slot = slot.saturating_add(rho);
            return Fit::Admit((s, k));
        }
        let mut left = sum;
        for (k, v) in &s {
            left = left.saturating_sub(*v);
            if left.saturating_add(rho) <= limit {
                let wait = i128::from(self.expiry(*k)).saturating_sub(i128::from(now));
                return Fit::Wait(Some(to_u64(wait)));
            }
        }
        Fit::Wait(Some(self.per_us))
    }

    /// Takes `rho` whatever is available, in the sub-window of `now`.
    #[must_use]
    pub fn force(self, mut s: WindowState, now: i64, rho: u128) -> (WindowState, i64) {
        self.prune(&mut s, now);
        let k = self.slot(now);
        let slot = s.entry(k).or_insert(0);
        *slot = slot.saturating_add(rho);
        (s, k)
    }

    /// Replaces a hold of `rho` in sub-window `k` by `actual`.
    pub fn settle(self, s: &mut WindowState, now: i64, k: i64, rho: u128, actual: u128) {
        let slot = s.entry(k).or_insert(0);
        if actual <= rho {
            *slot = slot.saturating_sub(rho.saturating_sub(actual));
        } else {
            *slot = slot.saturating_add(actual.saturating_sub(rho));
        }
        self.prune(s, now);
    }

    /// When sub-window `k` starts: `ceil(k·T/P)`.
    #[must_use]
    pub fn slot_start(self, k: i64) -> i64 {
        let n = i128::from(k).saturating_mul(i128::from(self.per_us));
        i64::try_from(ceil_div_i(n, i128::from(self.precision))).unwrap_or(i64::MAX)
    }

    /// When the oldest sub-window whose charges have not expired at `now` started.
    #[must_use]
    pub fn live_from(self, now: i64) -> i64 {
        self.slot_start(self.slot(now).saturating_sub(i64::from(self.precision)))
    }

    /// The unexpired charges at `now`.
    #[must_use]
    pub fn live_sum(self, s: &WindowState, now: i64) -> u128 {
        s.iter()
            .filter(|(k, _)| now < self.expiry(**k))
            .fold(0u128, |a, (_, v)| a.saturating_add(*v))
    }

    /// Adds `delta` to the charges at `now`: a charge goes to the sub-window of `now`, a credit
    /// comes off the oldest unexpired sub-windows first. `None`, with `s` unchanged, when the
    /// credit is larger than what is charged.
    pub fn adjust(self, s: &mut WindowState, now: i64, delta: i128) -> Option<u128> {
        self.prune(s, now);
        if delta >= 0 {
            let slot = s.entry(self.slot(now)).or_insert(0);
            *slot = slot.saturating_add(delta.unsigned_abs());
            return Some(Self::sum(s));
        }
        let mut credit = delta.unsigned_abs();
        if credit > Self::sum(s) {
            return None;
        }
        for v in s.values_mut() {
            let take = credit.min(*v);
            *v = v.saturating_sub(take);
            credit = credit.saturating_sub(take);
        }
        self.prune(s, now);
        Some(Self::sum(s))
    }

    /// Units available at `now`, and microseconds until every unexpired charge has expired.
    #[must_use]
    pub fn view(self, s: &WindowState, now: i64) -> (u128, u64) {
        let mut live = s.clone();
        self.prune(&mut live, now);
        let remaining = u128::from(self.limit).saturating_sub(Self::sum(&live));
        let last = live.keys().next_back().map_or(0, |k| {
            to_u64(i128::from(self.expiry(*k)).saturating_sub(i128::from(now)))
        });
        (remaining, last)
    }
}

/// Whether `rho` more requests fit a concurrency gauge of `limit`.
#[must_use]
pub fn concurrency_admit(limit: u64, in_use: u64, rho: u128) -> Fit<u64> {
    if limit == 0 {
        return Fit::Exceeds;
    }
    let rho = u64::try_from(rho).unwrap_or(u64::MAX);
    match in_use.checked_add(rho) {
        Some(g) if g <= limit => Fit::Admit(g),
        _ => Fit::Wait(Some(CONCURRENCY_WAIT_US)),
    }
}

/// A budget counter of one period.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct BudgetState {
    pub spent: u128,
    pub reserved: u128,
}

/// Whether `rho` fits a budget of `amount`: `spent + reserved + ρ ≤ amount`.
#[must_use]
pub fn budget_admit(amount: u128, s: BudgetState, rho: u128) -> Fit<BudgetState> {
    let total = s.spent.saturating_add(s.reserved).saturating_add(rho);
    if total <= amount {
        Fit::Admit(BudgetState {
            spent: s.spent,
            reserved: s.reserved.saturating_add(rho),
        })
    } else {
        Fit::Wait(None)
    }
}

/// Replaces a reservation of `rho` by the billed `actual`.
#[must_use]
pub fn budget_settle(s: BudgetState, rho: u128, actual: u128) -> BudgetState {
    BudgetState {
        spent: s.spent.saturating_add(actual),
        reserved: s.reserved.saturating_sub(rho),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::arithmetic_side_effects)]

    use proptest::prelude::*;

    use super::*;

    const S: i64 = 1_000_000;

    fn take(g: Gcra, s: Option<GcraState>, now: i64, rho: u128) -> GcraState {
        match g.admit(s, now, rho) {
            Fit::Admit(n) => n,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn gcra_admits_exactly_the_remaining_amount() {
        let g = Gcra {
            limit: 10,
            per_us: 3600 * S as u64,
            burst: 10,
        };
        let mut s = None;
        for _ in 0..10 {
            s = Some(take(g, s, 0, 1));
        }
        assert!(matches!(g.admit(s, 0, 1), Fit::Wait(Some(_))));
        let g2 = Gcra {
            limit: 600,
            per_us: 86_400 * S as u64,
            burst: 600,
        };
        let s2 = take(g2, None, 0, 300);
        let s2 = take(g2, Some(s2), 0, 300);
        assert!(matches!(g2.admit(Some(s2), 0, 300), Fit::Wait(_)));
    }

    #[test]
    fn gcra_waits_and_boundaries() {
        let g = Gcra {
            limit: 1,
            per_us: 10 * S as u64,
            burst: 1,
        };
        let s = take(g, None, 0, 1);
        assert_eq!(g.admit(Some(s), 0, 1), Fit::Wait(Some(10 * S as u64)));
        assert!(matches!(g.admit(Some(s), 10 * S - 1, 1), Fit::Wait(Some(1))));
        assert!(matches!(g.admit(Some(s), 10 * S, 1), Fit::Admit(_)));
        let burst1 = Gcra {
            limit: 4,
            per_us: 4 * S as u64,
            burst: 1,
        };
        let mut st = None;
        let mut admitted = 0;
        for i in 0..8 {
            if let Fit::Admit(n) = burst1.admit(st, i * S / 8, 1) {
                st = Some(n);
                admitted += 1;
            }
        }
        assert!(admitted <= 2);
        assert_eq!(
            Gcra {
                limit: 0,
                per_us: S as u64,
                burst: 0
            }
            .admit(None, 0, 1),
            Fit::Exceeds
        );
        assert_eq!(g.admit(None, 0, 2), Fit::Exceeds);
    }

    #[test]
    fn a_changed_window_keeps_the_units_already_used() {
        let hour = Gcra {
            limit: 2,
            per_us: 3600 * S as u64,
            burst: 2,
        };
        let two_hours = Gcra {
            per_us: 7200 * S as u64,
            ..hour
        };
        let used = take(hour, take(hour, None, 0, 1).into(), 0, 1);
        assert_eq!(two_hours.view(Some(used), 0).0, 0);
        assert!(matches!(two_hours.admit(Some(used), 0, 1), Fit::Wait(_)));
        let one = take(hour, None, 0, 1);
        assert_eq!(two_hours.view(Some(one), 0).0, 1);
        let unknown = GcraState { per_us: 0, ..one };
        assert_eq!(
            two_hours.view(Some(unknown), 0).0,
            0,
            "an unrecorded window is taken as unchanged"
        );
    }

    #[test]
    fn gcra_view_after_reservation() {
        let g = Gcra {
            limit: 10,
            per_us: 60 * S as u64,
            burst: 10,
        };
        let s = take(g, None, 0, 1);
        assert_eq!(g.view(Some(s), 0), (9, 6 * S as u64));
    }

    #[test]
    fn gcra_refunds_cap_and_charges_go_into_debt() {
        let g = Gcra {
            limit: 1000,
            per_us: 86_400 * S as u64,
            burst: 1000,
        };
        let s = take(g, None, 0, 300);
        let refunded = g.settle(Some(s), 0, 300, 10);
        assert_eq!(g.view(Some(refunded), 0).0, 990);
        let charged = g.settle(Some(s), 0, 300, 1500);
        assert_eq!(g.view(Some(charged), 0).0, 0);
        assert!(charged.avail < 0);
        let full = g.settle(None, 0, 300, 0);
        assert_eq!(full.avail, g.cap());
    }

    #[test]
    fn gcra_burst_changes_keep_state() {
        let two = Gcra {
            limit: 2,
            per_us: 3600 * S as u64,
            burst: 2,
        };
        let s = take(two, take_some(two), 0, 1);
        let three = Gcra {
            limit: 3,
            per_us: 3600 * S as u64,
            burst: 3,
        };
        let s3 = take(three, Some(s), 0, 1);
        assert!(matches!(three.admit(Some(s3), 0, 1), Fit::Wait(_)));
        let one = Gcra {
            limit: 1,
            per_us: 3600 * S as u64,
            burst: 1,
        };
        assert!(matches!(one.admit(Some(s3), 0, 1), Fit::Wait(_)));
    }

    fn take_some(g: Gcra) -> Option<GcraState> {
        Some(take(g, None, 0, 1))
    }

    #[test]
    fn window_boundaries() {
        let w = Window {
            limit: 5,
            per_us: 10 * S as u64,
            precision: 10,
        };
        let mut s = WindowState::new();
        for _ in 0..5 {
            match w.admit(s, 0, 1) {
                Fit::Admit((n, _)) => s = n,
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(w.admit(s.clone(), 5 * S, 1), Fit::Wait(Some(6 * S as u64)));
        assert!(matches!(w.admit(s.clone(), 11 * S - 1, 1), Fit::Wait(Some(1))));
        assert!(matches!(w.admit(s.clone(), 11 * S + S / 10, 1), Fit::Admit(_)));
        assert_eq!(w.admit(s.clone(), 0, 6), Fit::Exceeds);
        assert_eq!(w.view(&s, 0), (0, 11 * S as u64));
    }

    #[test]
    fn concurrency_and_budgets() {
        assert_eq!(concurrency_admit(2, 1, 1), Fit::Admit(2));
        assert_eq!(concurrency_admit(2, 2, 1), Fit::Wait(Some(CONCURRENCY_WAIT_US)));
        assert_eq!(concurrency_admit(0, 0, 1), Fit::Exceeds);
        let b = BudgetState {
            spent: 0,
            reserved: 0,
        };
        let Fit::Admit(b) = budget_admit(3, b, 3) else {
            panic!()
        };
        assert_eq!(budget_admit(3, b, 1), Fit::Wait(None));
        let b = budget_settle(b, 3, 1);
        assert_eq!(
            b,
            BudgetState {
                spent: 1,
                reserved: 0
            }
        );
        assert!(matches!(
            budget_admit(0, BudgetState::default(), 1),
            Fit::Wait(None)
        ));
    }

    proptest! {
        #[test]
        fn gcra_never_admits_more_than_burst_plus_refill(
            limit in 1u64..50, per_s in 1i64..20, burst in 1u64..50,
            steps in proptest::collection::vec((0i64..2_000_000, 1u128..10), 1..200),
        ) {
            let g = Gcra { limit, per_us: (per_s * S) as u64, burst };
            let mut s = None;
            let mut now = 0i64;
            let mut admitted: u128 = 0;
            for (dt, rho) in steps {
                now += dt;
                if let Fit::Admit(n) = g.admit(s, now, rho) {
                    s = Some(n);
                    admitted += rho;
                }
            }
            let bound = u128::from(burst) + (u128::from(limit) * now as u128) / (per_s as u128 * S as u128);
            prop_assert!(admitted <= bound, "{admitted} > {bound}");
        }

        #[test]
        fn gcra_wait_is_sufficient_and_tight(
            limit in 1u64..50, per_s in 1i64..20, burst in 1u64..50, used in 1u128..50, rho in 1u128..50, at in 0i64..5_000_000,
        ) {
            let g = Gcra { limit, per_us: (per_s * S) as u64, burst };
            let s = g.force(None, 0, used);
            if let Fit::Wait(Some(w)) = g.admit(Some(s), at, rho) {
                prop_assert!(matches!(g.admit(Some(s), at + w as i64, rho), Fit::Admit(_)));
                prop_assert!(w == 0 || !matches!(g.admit(Some(s), at + w as i64 - 1, rho), Fit::Admit(_)));
            }
        }

        #[test]
        fn gcra_exact_refunds(limit in 1u64..1000, burst in 1u64..1000, rho in 1u128..1000, actual in 0u128..1000) {
            let g = Gcra { limit, per_us: 86_400 * S as u64, burst };
            if let Fit::Admit(s) = g.admit(None, 0, rho) {
                let settled = g.settle(Some(s), 0, rho, actual);
                let expect = (i128::from(burst) - actual as i128) * g.t();
                prop_assert_eq!(settled.avail, expect.min(g.cap()));
            }
        }

        #[test]
        fn window_never_exceeds_limit_in_any_window(
            limit in 1u64..30, per_s in 1i64..10, precision in 1u32..20,
            steps in proptest::collection::vec((0i64..3_000_000, 1u128..5), 1..200),
        ) {
            let w = Window { limit, per_us: (per_s * S) as u64, precision };
            let mut s = WindowState::new();
            let mut now = 0i64;
            let mut log: Vec<(i64, u128)> = Vec::new();
            for (dt, rho) in steps {
                now += dt;
                if let Fit::Admit((n, _)) = w.admit(s.clone(), now, rho) {
                    s = n;
                    log.push((now, rho));
                }
            }
            for (t0, _) in &log {
                let total: u128 = log.iter().filter(|(t, _)| *t >= *t0 && *t < t0 + per_s * S).map(|(_, r)| r).sum();
                prop_assert!(total <= u128::from(limit));
            }
        }
    }
}
