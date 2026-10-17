//! The in-process counter store: standalone mode, and each node's local share in degraded mode.
//! One lock covers every counter and reservation, so an admission is atomic by construction.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use antirion_core::Clock;
use parking_lot::Mutex;

use super::{
    AdmitDecision, AdmitRequest, AttemptRequest, BudgetChange, CounterStore, CounterView, ExpiredReservation,
    NegativeSpend, RefusalKind, ReservationId, ResvState, SettleOutcome, SettleRequest, StoreError,
    StoreRefusal, state_key,
};
use crate::counter::{CounterId, Hold, HoldMode, Rule};
use crate::math::{self, BudgetState, Fit, GcraState, WindowState};

#[derive(Debug)]
struct Record {
    state: ResvState,
    node: String,
    /// Each hold with the sub-window its charge went to (sliding windows).
    holds: Vec<(Hold, i64)>,
    attempts: BTreeSet<u16>,
    keep_until: Option<i64>,
}

#[derive(Debug, Default)]
struct Inner {
    gcra: HashMap<CounterId, GcraState>,
    windows: HashMap<CounterId, WindowState>,
    gauges: HashMap<CounterId, u64>,
    budgets: HashMap<(CounterId, i64), BudgetState>,
    resv: HashMap<ReservationId, Record>,
    leases: HashMap<String, i64>,
}

enum Write {
    Gcra(CounterId, GcraState),
    Window(CounterId, WindowState),
    Gauge(CounterId, u64),
    Budget((CounterId, i64), BudgetState),
    Nothing,
}

/// An admission of `ρ` that must also leave `extra` free: it fits only when `ρ + extra` does
/// (`with_extra`), and a refusal for want of that room is one that may fit later.
fn beside<S, T>(alone: Fit<S>, extra: u128, with_extra: impl FnOnce() -> Fit<T>) -> Fit<S> {
    match alone {
        Fit::Admit(s) if extra > 0 => match with_extra() {
            Fit::Admit(_) => Fit::Admit(s),
            Fit::Wait(w) => Fit::Wait(w),
            Fit::Exceeds => Fit::Wait(None),
        },
        other => other,
    }
}

impl Inner {
    /// What the headroom holds right after hold `index` have left.
    fn headroom(&self, holds: &[Hold], index: usize, now: i64) -> u128 {
        holds
            .iter()
            .skip(index.saturating_add(1))
            .take_while(|p| p.mode == HoldMode::Headroom)
            .fold(0u128, |a, p| a.saturating_add(self.view(p, now).remaining))
    }

    /// Every hold fits (or is not consulted): the writes and sub-windows; otherwise every refusal.
    fn try_take(&self, holds: &[Hold], now: i64) -> Result<Vec<(Write, i64)>, Vec<StoreRefusal>> {
        let mut writes = Vec::with_capacity(holds.len());
        let mut refusals = Vec::new();
        for (index, h) in holds.iter().enumerate() {
            let enforce = h.mode == HoldMode::Enforce;
            let extra = if enforce {
                self.headroom(holds, index, now)
            } else {
                0
            };
            let mut refuse = |kind, wait_us| {
                refusals.push(StoreRefusal { index, kind, wait_us });
                (Write::Nothing, 0)
            };
            let w = match (&h.rule, h.mode) {
                (_, HoldMode::Track | HoldMode::Headroom) => (Write::Nothing, 0),
                (Rule::Gcra(g), _) => {
                    let cur = self.gcra.get(&h.counter).copied();
                    if enforce {
                        match beside(g.admit(cur, now, h.amount), extra, || {
                            g.admit(cur, now, h.amount.saturating_add(extra))
                        }) {
                            Fit::Admit(s) => (Write::Gcra(h.counter, s), 0),
                            Fit::Wait(w) => refuse(RefusalKind::Rate, w),
                            Fit::Exceeds => refuse(RefusalKind::Exceeds, None),
                        }
                    } else {
                        (Write::Gcra(h.counter, g.force(cur, now, h.amount)), 0)
                    }
                }
                (Rule::Window(win), _) => {
                    let cur = self.windows.get(&h.counter).cloned().unwrap_or_default();
                    if enforce {
                        match beside(win.admit(cur.clone(), now, h.amount), extra, || {
                            win.admit(cur, now, h.amount.saturating_add(extra))
                        }) {
                            Fit::Admit((s, k)) => (Write::Window(h.counter, s), k),
                            Fit::Wait(w) => refuse(RefusalKind::Rate, w),
                            Fit::Exceeds => refuse(RefusalKind::Exceeds, None),
                        }
                    } else {
                        let (s, k) = win.force(cur, now, h.amount);
                        (Write::Window(h.counter, s), k)
                    }
                }
                (Rule::Concurrency { limit }, _) => {
                    let g = self.gauges.get(&h.counter).copied().unwrap_or(0);
                    if enforce {
                        match math::concurrency_admit(*limit, g, h.amount.saturating_add(extra)) {
                            Fit::Admit(_) => (
                                Write::Gauge(
                                    h.counter,
                                    g.saturating_add(u64::try_from(h.amount).unwrap_or(u64::MAX)),
                                ),
                                0,
                            ),
                            Fit::Wait(w) => refuse(RefusalKind::Rate, w),
                            Fit::Exceeds => refuse(RefusalKind::Exceeds, None),
                        }
                    } else {
                        let n = g.saturating_add(u64::try_from(h.amount).unwrap_or(u64::MAX));
                        (Write::Gauge(h.counter, n), 0)
                    }
                }
                (Rule::Rolling(win), _) => {
                    let cur = self.windows.get(&h.counter).cloned().unwrap_or_default();
                    if enforce {
                        match win.admit(cur, now, h.amount) {
                            Fit::Admit((s, k)) => (Write::Window(h.counter, s), k),
                            Fit::Wait(w) => refuse(RefusalKind::Budget, w),
                            Fit::Exceeds => refuse(RefusalKind::Budget, None),
                        }
                    } else {
                        let (s, k) = win.force(cur, now, h.amount);
                        (Write::Window(h.counter, s), k)
                    }
                }
                (Rule::Budget { amount, .. }, _) => {
                    let key = state_key(h);
                    let cur = self.budgets.get(&key).copied().unwrap_or_default();
                    if enforce {
                        match math::budget_admit(*amount, cur, h.amount) {
                            Fit::Admit(s) => (Write::Budget(key, s), 0),
                            _ => refuse(RefusalKind::Budget, None),
                        }
                    } else {
                        let s = BudgetState {
                            spent: cur.spent,
                            reserved: cur.reserved.saturating_add(h.amount),
                        };
                        (Write::Budget(key, s), 0)
                    }
                }
            };
            writes.push(w);
        }
        if refusals.is_empty() {
            Ok(writes)
        } else {
            Err(refusals)
        }
    }

    fn commit(&mut self, writes: Vec<(Write, i64)>) {
        for (w, _) in writes {
            match w {
                Write::Gcra(id, s) => {
                    self.gcra.insert(id, s);
                }
                Write::Window(id, s) => {
                    self.windows.insert(id, s);
                }
                Write::Gauge(id, g) => {
                    self.gauges.insert(id, g);
                }
                Write::Budget(key, s) => {
                    self.budgets.insert(key, s);
                }
                Write::Nothing => {}
            }
        }
    }

    fn view(&self, h: &Hold, now: i64) -> CounterView {
        match &h.rule {
            Rule::Gcra(g) => {
                let (remaining, full_in_us) = g.view(self.gcra.get(&h.counter).copied(), now);
                CounterView {
                    remaining,
                    full_in_us,
                    ..CounterView::default()
                }
            }
            Rule::Window(w) => {
                let empty = WindowState::new();
                let (remaining, full_in_us) = w.view(self.windows.get(&h.counter).unwrap_or(&empty), now);
                CounterView {
                    remaining,
                    full_in_us,
                    ..CounterView::default()
                }
            }
            Rule::Concurrency { limit } => {
                let g = self.gauges.get(&h.counter).copied().unwrap_or(0);
                CounterView {
                    remaining: u128::from(limit.saturating_sub(g)),
                    ..CounterView::default()
                }
            }
            Rule::Rolling(w) => {
                let empty = WindowState::new();
                let s = self.windows.get(&h.counter).unwrap_or(&empty);
                let (remaining, full_in_us) = w.view(s, now);
                CounterView {
                    remaining,
                    full_in_us,
                    spent: w.live_sum(s, now),
                    reserved: 0,
                }
            }
            Rule::Budget { amount, .. } => {
                let s = self.budgets.get(&state_key(h)).copied().unwrap_or_default();
                CounterView {
                    remaining: amount.saturating_sub(s.spent.saturating_add(s.reserved)),
                    full_in_us: 0,
                    spent: s.spent,
                    reserved: s.reserved,
                }
            }
        }
    }

    /// Replaces one hold by `actual`; the budget's spend before and after, for budgets.
    fn settle_hold(&mut self, h: &Hold, slot: i64, actual: u128, now: i64) -> Option<(u128, u128)> {
        if h.mode == HoldMode::Headroom {
            return None;
        }
        match &h.rule {
            Rule::Gcra(g) => {
                let cur = self.gcra.get(&h.counter).copied();
                self.gcra.insert(h.counter, g.settle(cur, now, h.amount, actual));
                None
            }
            Rule::Window(w) => {
                let s = self.windows.entry(h.counter).or_default();
                w.settle(s, now, slot, h.amount, actual);
                None
            }
            Rule::Concurrency { .. } => {
                let g = self.gauges.entry(h.counter).or_insert(0);
                *g = g.saturating_sub(u64::try_from(h.amount).unwrap_or(u64::MAX));
                None
            }
            Rule::Rolling(w) => {
                let s = self.windows.entry(h.counter).or_default();
                let before = w.live_sum(s, now);
                if h.mode == HoldMode::Track {
                    w.settle(s, now, w.slot(now), 0, actual);
                } else {
                    w.settle(s, now, slot, h.amount, actual);
                }
                Some((before, w.live_sum(s, now)))
            }
            Rule::Budget { .. } => {
                let s = self.budgets.entry(state_key(h)).or_default();
                let before = s.spent;
                let rho = if h.mode == HoldMode::Track { 0 } else { h.amount };
                *s = math::budget_settle(*s, rho, actual);
                Some((before, s.spent))
            }
        }
    }

    fn sweep(&mut self, now: i64) {
        self.resv
            .retain(|_, r| r.keep_until.is_none_or(|until| now < until));
        self.gauges.retain(|_, g| *g > 0);
        self.windows.retain(|_, w| !w.is_empty());
    }
}

/// Counters in process memory, on the gateway clock.
#[derive(Debug)]
pub struct MemoryCounters {
    clock: Arc<dyn Clock>,
    inner: Mutex<Inner>,
}

impl MemoryCounters {
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>) -> MemoryCounters {
        MemoryCounters {
            clock,
            inner: Mutex::new(Inner::default()),
        }
    }

    fn now(&self) -> i64 {
        self.clock.now().0
    }

    /// Forgets every counter and reservation (test-mode reset).
    pub fn clear(&self) {
        *self.inner.lock() = Inner::default();
    }

    /// Drops closed reservations past their idempotency window and counters in their initial
    /// state. A partly drained counter is never dropped.
    pub fn sweep(&self) {
        let now = self.now();
        self.inner.lock().sweep(now);
    }

    fn admit_sync(&self, req: &AdmitRequest<'_>) -> AdmitDecision {
        let now = self.now();
        let mut inner = self.inner.lock();
        if let Some(r) = inner.resv.get(&req.uid) {
            return AdmitDecision::Replayed(r.state);
        }
        match inner.try_take(req.holds, now) {
            Err(refusals) => AdmitDecision::Refused { refusals },
            Ok(writes) => {
                let slots: Vec<i64> = writes.iter().map(|(_, k)| *k).collect();
                inner.commit(writes);
                let views = req.holds.iter().map(|h| inner.view(h, now)).collect();
                inner.resv.insert(
                    req.uid,
                    Record {
                        state: ResvState::Held,
                        node: req.node.to_owned(),
                        holds: req.holds.iter().cloned().zip(slots).collect(),
                        attempts: BTreeSet::new(),
                        keep_until: None,
                    },
                );
                AdmitDecision::Admitted { views }
            }
        }
    }

    fn admit_attempt_sync(&self, req: &AttemptRequest<'_>) -> AdmitDecision {
        let (uid, attempt, holds) = (req.uid, req.attempt, req.holds);
        let now = self.now();
        let mut inner = self.inner.lock();
        match inner.resv.get(&uid) {
            None => return AdmitDecision::Replayed(ResvState::Cancelled),
            Some(r) if r.state != ResvState::Held || r.attempts.contains(&attempt) => {
                return AdmitDecision::Replayed(r.state);
            }
            Some(_) => {}
        }
        match inner.try_take(holds, now) {
            Err(refusals) => AdmitDecision::Refused { refusals },
            Ok(writes) => {
                let slots: Vec<i64> = writes.iter().map(|(_, k)| *k).collect();
                inner.commit(writes);
                let views = holds.iter().map(|h| inner.view(h, now)).collect();
                if let Some(r) = inner.resv.get_mut(&uid) {
                    r.attempts.insert(attempt);
                    r.holds.extend(holds.iter().cloned().zip(slots));
                }
                AdmitDecision::Admitted { views }
            }
        }
    }

    fn reconcile_sync(&self, req: &SettleRequest<'_>) -> Result<SettleOutcome, StoreError> {
        let (uid, actual, keep_us) = (req.uid, req.actual, req.keep_us);
        let now = self.now();
        let keep = now.saturating_add(i64::try_from(keep_us).unwrap_or(i64::MAX));
        let mut inner = self.inner.lock();
        let holds = match inner.resv.get(&uid) {
            None => {
                inner.resv.insert(
                    uid,
                    Record {
                        state: ResvState::Cancelled,
                        node: String::new(),
                        holds: Vec::new(),
                        attempts: BTreeSet::new(),
                        keep_until: Some(keep),
                    },
                );
                return Ok(SettleOutcome::Noop(None));
            }
            Some(r) if r.state != ResvState::Held => return Ok(SettleOutcome::Noop(Some(r.state))),
            Some(r) => r.holds.clone(),
        };
        if actual.len() != holds.len() {
            return Err(StoreError::Protocol(format!(
                "{} actual amounts for {} holds",
                actual.len(),
                holds.len()
            )));
        }
        let mut budgets = Vec::new();
        for (index, ((h, slot), a)) in holds.iter().zip(actual).enumerate() {
            if let Some((before, after)) = inner.settle_hold(h, *slot, *a, now) {
                budgets.push(BudgetChange { index, before, after });
            }
        }
        if let Some(r) = inner.resv.get_mut(&uid) {
            r.state = ResvState::Settled;
            r.keep_until = Some(keep);
        }
        Ok(SettleOutcome::Settled { budgets })
    }

    fn reap_sync(&self, max: usize) -> Vec<ExpiredReservation> {
        let now = self.now();
        let mut inner = self.inner.lock();
        inner.sweep(now);
        let lapsed =
            |node: &str, leases: &HashMap<String, i64>| leases.get(node).is_some_and(|until| *until <= now);
        let due: Vec<ReservationId> = inner
            .resv
            .iter()
            .filter(|(_, r)| r.state == ResvState::Held && lapsed(&r.node, &inner.leases))
            .map(|(id, _)| *id)
            .take(max)
            .collect();
        let mut out = Vec::with_capacity(due.len());
        for uid in due {
            let Some(r) = inner.resv.get(&uid) else { continue };
            let holds = r.holds.clone();
            let node = r.node.clone();
            for (h, slot) in &holds {
                inner.settle_hold(h, *slot, h.amount, now);
            }
            if let Some(r) = inner.resv.get_mut(&uid) {
                r.state = ResvState::Expired;
            }
            out.push(ExpiredReservation {
                uid,
                node,
                holds: holds.into_iter().map(|(h, _)| h).collect(),
            });
        }
        out
    }
}

impl CounterStore for MemoryCounters {
    async fn admit(&self, req: &AdmitRequest<'_>) -> Result<AdmitDecision, StoreError> {
        Ok(self.admit_sync(req))
    }

    async fn admit_attempt(&self, req: &AttemptRequest<'_>) -> Result<AdmitDecision, StoreError> {
        Ok(self.admit_attempt_sync(req))
    }

    async fn reconcile(&self, req: &SettleRequest<'_>) -> Result<SettleOutcome, StoreError> {
        self.reconcile_sync(req)
    }

    async fn heartbeat(&self, node: &str, ttl_us: u64) -> Result<(), StoreError> {
        let until = self
            .now()
            .saturating_add(i64::try_from(ttl_us).unwrap_or(i64::MAX));
        self.inner.lock().leases.insert(node.to_owned(), until);
        Ok(())
    }

    async fn reap(&self, max: usize) -> Result<Vec<ExpiredReservation>, StoreError> {
        Ok(self.reap_sync(max))
    }

    async fn read(&self, holds: &[Hold]) -> Result<Vec<CounterView>, StoreError> {
        let now = self.now();
        let inner = self.inner.lock();
        Ok(holds.iter().map(|h| inner.view(h, now)).collect())
    }

    async fn seed_budget(&self, hold: &Hold, spent: u128) -> Result<(), StoreError> {
        let mut inner = self.inner.lock();
        let s = inner.budgets.entry(state_key(hold)).or_default();
        s.spent = s.spent.max(spent);
        Ok(())
    }

    async fn seed_window(&self, hold: &Hold, slots: &[(i64, u128)]) -> Result<(), StoreError> {
        let mut inner = self.inner.lock();
        let s = inner.windows.entry(hold.counter).or_default();
        for (k, v) in slots {
            let cur = s.entry(*k).or_insert(0);
            *cur = (*cur).max(*v);
        }
        Ok(())
    }

    async fn adjust_budget(
        &self,
        hold: &Hold,
        delta: i128,
    ) -> Result<Result<u128, NegativeSpend>, StoreError> {
        let now = self.now();
        let mut inner = self.inner.lock();
        if let Rule::Rolling(w) = &hold.rule {
            let s = inner.windows.entry(hold.counter).or_default();
            return Ok(w.adjust(s, now, delta).ok_or(NegativeSpend));
        }
        let s = inner.budgets.entry(state_key(hold)).or_default();
        let next = i128::try_from(s.spent)
            .ok()
            .and_then(|v| v.checked_add(delta))
            .filter(|v| *v >= 0)
            .and_then(|v| u128::try_from(v).ok());
        Ok(match next {
            Some(v) => {
                s.spent = v;
                Ok(v)
            }
            None => Err(NegativeSpend),
        })
    }

    async fn ping(&self) -> Result<(), StoreError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
