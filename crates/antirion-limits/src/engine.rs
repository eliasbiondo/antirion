//! The reservation engine the data plane calls: admit at stage `admit`, per-attempt holds before
//! each attempt, settle before the last response byte. Every store call has a deadline; a store
//! that misses it is unavailable and each counter follows its `on_store_unavailable`.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use antirion_core::{Clock, UtcMicros};
use antirion_error::GatewayError;
use http::{HeaderName, HeaderValue};
use parking_lot::Mutex;

use crate::counter::{CounterId, Hold, HoldMode, Rule};
use crate::decl::OnUnavailable;
use crate::degraded::{DegradedCause, Health, Queued, ReplayQueue, local_share};
use crate::headers;
use crate::money::DEC_ONE;
use crate::plan::{AdmitPlan, PlannedHold, Unit};
use crate::refusal::{refusal_error, unavailable_error};
use crate::reservation::LocalPart;
pub use crate::reservation::{
    AttemptRefused, BudgetAlert, BudgetSpent, Reservation, SettleReport, Settlement,
};
use crate::settings::HeaderSettings;
use crate::store::memory::MemoryCounters;
use crate::store::{
    AdmitDecision, AdmitRequest, AttemptRequest, CounterStore, CounterView, ExpiredReservation, RefusalKind,
    ReservationId, ResvState, SettleOutcome, SettleRequest, StoreError, StoreRefusal, state_key,
};

/// Reservations a refused concurrency admission expires before it is tried once more: a crashed
/// node's holds are released as soon as its lease runs out, not when the reaper job next runs.
const REAP_ON_REFUSAL: usize = 100;

/// The engine's settings.
#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// This node's id: reservations record it, and its heartbeat is their lease.
    pub node: String,
    pub lease_ttl: Duration,
    /// The deadline of every store call.
    pub store_timeout: Duration,
    /// Settlements kept for replay while the store is down.
    pub replay_capacity: usize,
}

/// The reservation engine over a counter store.
#[derive(Debug)]
pub struct LimitsEngine<S> {
    store: S,
    local: MemoryCounters,
    cfg: EngineConfig,
    /// `limits.lease_ttl` in microseconds; `cfg.lease_ttl` until a configuration sets it.
    lease_us: AtomicU64,
    known_nodes: AtomicU32,
    health: Health,
    queue: ReplayQueue,
    synced: Mutex<HashMap<(CounterId, i64), u128>>,
}

fn actual_of(h: &PlannedHold, s: &Settlement) -> u128 {
    match &h.info.unit {
        Unit::Requests { successful_only } => u128::from(!*successful_only || s.success),
        Unit::Tokens(w) | Unit::BudgetTokens(w) => w.charge(&s.usage).unwrap_or(u128::MAX),
        Unit::Concurrency => 0,
        Unit::Usd => s.billed.units(),
    }
}

/// Whether waiting can never help, whatever the share of the quota.
fn exceeds_globally(h: &Hold) -> bool {
    match &h.rule {
        Rule::Gcra(g) => h.amount > u128::from(g.burst),
        Rule::Window(w) => h.amount > u128::from(w.limit),
        Rule::Concurrency { limit } => *limit == 0,
        Rule::Budget { .. } | Rule::Rolling(_) => false,
    }
}

fn derived(uid: ReservationId, n: u128) -> ReservationId {
    ReservationId(uid.0 ^ n.wrapping_add(1).rotate_right(16))
}

impl<S: CounterStore> LimitsEngine<S> {
    #[must_use]
    pub fn new(store: S, clock: Arc<dyn Clock>, cfg: EngineConfig) -> LimitsEngine<S> {
        LimitsEngine {
            store,
            local: MemoryCounters::new(clock),
            queue: ReplayQueue::new(cfg.replay_capacity),
            lease_us: AtomicU64::new(u64::try_from(cfg.lease_ttl.as_micros()).unwrap_or(u64::MAX)),
            cfg,
            known_nodes: AtomicU32::new(1),
            health: Health::default(),
            synced: Mutex::new(HashMap::new()),
        }
    }

    #[must_use]
    pub fn store(&self) -> &S {
        &self.store
    }

    /// The node count that local shares divide by.
    pub fn set_known_nodes(&self, n: u32) {
        self.known_nodes.store(n.max(1), Ordering::Relaxed);
    }

    #[must_use]
    pub fn is_store_down(&self) -> bool {
        self.health.is_down()
    }

    /// Settlements waiting for the store, and how many were dropped because the queue was full.
    #[must_use]
    pub fn queued(&self) -> (usize, u64) {
        (self.queue.len(), self.queue.dropped())
    }

    async fn call<T>(&self, f: impl Future<Output = Result<T, StoreError>>) -> Result<T, StoreError> {
        if self.health.is_down() {
            return Err(StoreError::Unavailable("marked down".into()));
        }
        match tokio::time::timeout(self.cfg.store_timeout, f).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => {
                self.health.mark_down();
                Err(e)
            }
            Err(_) => {
                self.health.mark_down();
                Err(StoreError::Unavailable("timed out".into()))
            }
        }
    }

    fn remember(&self, holds: &[PlannedHold], views: &[CounterView]) {
        let mut synced = self.synced.lock();
        for (h, v) in holds.iter().zip(views) {
            if h.info.unit.is_budget() {
                synced.insert(state_key(&h.hold), v.remaining);
            }
        }
    }

    fn refused(
        &self,
        holds: &[PlannedHold],
        refusals: &[StoreRefusal],
        views: &[CounterView],
        now: UtcMicros,
        hs: &HeaderSettings,
    ) -> GatewayError {
        let mut e = refusal_error(holds, refusals, views, now);
        if hs.ietf && views.len() == holds.len() {
            let states = headers::policy_states(holds, views);
            if let Some((policy, current)) = headers::ietf(&states, hs.max_policies)
                && let (Ok(p), Ok(c)) = (HeaderValue::from_str(&policy), HeaderValue::from_str(&current))
            {
                e = e
                    .header(HeaderName::from_static(headers::RATELIMIT_POLICY), p)
                    .header(HeaderName::from_static(headers::RATELIMIT), c);
            }
        }
        e
    }

    /// Admission (stage `admit`): every hold of the plan, or none. A plan without holds makes no
    /// store call.
    pub async fn admit(&self, plan: AdmitPlan) -> Result<Reservation, GatewayError> {
        let mut r = Reservation::new(&plan, &self.cfg.node);
        if plan.holds.is_empty() {
            return Ok(r);
        }
        let holds = plan.store_holds();
        let req = AdmitRequest {
            uid: plan.uid,
            node: &self.cfg.node,
            group: plan.group,
            holds: &holds,
        };
        let mut decision = self.call(self.store.admit(&req)).await;
        if let Ok(AdmitDecision::Refused { refusals }) = &decision
            && refusals.iter().any(|f| {
                plan.holds
                    .get(f.index)
                    .is_some_and(|h| matches!(h.info.unit, Unit::Concurrency))
            })
            && self
                .call(self.store.reap(REAP_ON_REFUSAL))
                .await
                .is_ok_and(|expired| !expired.is_empty())
        {
            decision = self.call(self.store.admit(&req)).await;
        }
        match decision {
            Ok(AdmitDecision::Admitted { views }) => {
                self.remember(&plan.holds, &views);
                r.holds = plan.holds;
                r.views = views;
                r.remote = true;
                Ok(r)
            }
            Ok(AdmitDecision::Replayed(ResvState::Held)) => {
                r.views = self.call(self.store.read(&holds)).await.unwrap_or_default();
                r.holds = plan.holds;
                r.remote = true;
                Ok(r)
            }
            Ok(AdmitDecision::Replayed(_)) => Err(unavailable_error()),
            Ok(AdmitDecision::Refused { refusals }) => {
                let views = self.call(self.store.read(&holds)).await.unwrap_or_default();
                Err(self.refused(&plan.holds, &refusals, &views, plan.admitted_at, &plan.headers))
            }
            Err(_) => {
                let (uid, now) = (plan.uid, plan.admitted_at);
                self.degrade(&mut r, plan.holds, uid, now)
                    .await
                    .map_err(|d| *d.error)?;
                Ok(r)
            }
        }
    }

    /// Takes `holds` on the local store, each by its `on_store_unavailable`.
    async fn degrade(
        &self,
        r: &mut Reservation,
        holds: Vec<PlannedHold>,
        uid: ReservationId,
        now: UtcMicros,
    ) -> Result<(), AttemptRefused> {
        let m = self.known_nodes.load(Ordering::Relaxed);
        let mut part = Vec::new();
        for ph in holds {
            let budget = ph.info.unit.is_budget();
            if ph.hold.mode == HoldMode::Track {
                r.degraded.mark(true, DegradedCause::StoreTimeout);
                r.offline.push(ph);
                continue;
            }
            match ph.info.on_unavailable {
                OnUnavailable::FailOpen => r.degraded.mark(budget, DegradedCause::FailOpen),
                OnUnavailable::FailClosed => {
                    return Err(AttemptRefused {
                        wait: Some(Duration::from_secs(1)),
                        error: Box::new(unavailable_error()),
                    });
                }
                OnUnavailable::LocalShare => {
                    let synced = self.synced.lock().get(&state_key(&ph.hold)).copied();
                    let Some((local, scale)) = local_share(&ph.hold, m, synced) else {
                        return Err(AttemptRefused {
                            wait: Some(Duration::from_secs(1)),
                            error: Box::new(unavailable_error()),
                        });
                    };
                    r.degraded.mark(budget, DegradedCause::LocalShare);
                    part.push((ph, local, scale));
                }
            }
        }
        if part.is_empty() {
            return Ok(());
        }
        let local: Vec<Hold> = part.iter().map(|(_, l, _)| l.clone()).collect();
        let req = AdmitRequest {
            uid,
            node: &self.cfg.node,
            group: r.group,
            holds: &local,
        };
        match self.local.admit(&req).await {
            Ok(AdmitDecision::Refused { refusals }) => {
                let planned: Vec<PlannedHold> = part.iter().map(|(p, _, _)| p.clone()).collect();
                let refusals: Vec<StoreRefusal> = refusals
                    .into_iter()
                    .map(|mut f| {
                        let global = planned.get(f.index).is_some_and(|p| exceeds_globally(&p.hold));
                        if f.kind == RefusalKind::Exceeds && !global {
                            f.kind = RefusalKind::Rate;
                            f.wait_us = Some(1_000_000);
                        }
                        f
                    })
                    .collect();
                let wait = refusals
                    .iter()
                    .filter_map(|f| f.wait_us)
                    .max()
                    .map(Duration::from_micros);
                let mut error = self.refused(&planned, &refusals, &[], now, &r.headers);
                if let Ok(v) = http::HeaderValue::from_str(&r.degraded.components().join(", ")) {
                    error = error.header(http::HeaderName::from_static(antirion_core::headers::DEGRADED), v);
                }
                Err(AttemptRefused {
                    wait,
                    error: Box::new(error),
                })
            }
            _ => {
                r.local.push(LocalPart { uid, holds: part });
                Ok(())
            }
        }
    }

    /// Per-attempt holds (route-tree nodes, the deployment), all or nothing. A refusal makes this
    /// position ineligible for the attempt, not the request.
    pub async fn admit_attempt(
        &self,
        r: &mut Reservation,
        attempt: u16,
        holds: Vec<PlannedHold>,
    ) -> Result<(), AttemptRefused> {
        if holds.is_empty() {
            return Ok(());
        }
        let store_holds: Vec<Hold> = holds.iter().map(|h| h.hold.clone()).collect();
        let now = r.admitted_at;
        if !r.remote && r.holds.is_empty() && r.local.is_empty() && r.offline.is_empty() {
            return self.open_with_attempt(r, holds, &store_holds).await;
        }
        if r.remote {
            let req = AttemptRequest {
                uid: r.uid,
                group: r.group,
                attempt,
                holds: &store_holds,
            };
            match self.call(self.store.admit_attempt(&req)).await {
                Ok(AdmitDecision::Admitted { views }) => {
                    r.holds.extend(holds);
                    r.views.extend(views);
                    return Ok(());
                }
                Ok(AdmitDecision::Replayed(ResvState::Held)) => {
                    let views = self.call(self.store.read(&store_holds)).await.unwrap_or_default();
                    r.views.extend(
                        views
                            .into_iter()
                            .chain(std::iter::repeat(CounterView::default()))
                            .take(holds.len()),
                    );
                    r.holds.extend(holds);
                    return Ok(());
                }
                Ok(AdmitDecision::Replayed(_)) => {
                    return Err(AttemptRefused {
                        wait: None,
                        error: Box::new(unavailable_error()),
                    });
                }
                Ok(AdmitDecision::Refused { refusals }) => {
                    let wait = if refusals.iter().all(|f| f.kind == RefusalKind::Rate) {
                        refusals
                            .iter()
                            .filter_map(|f| f.wait_us)
                            .max()
                            .map(Duration::from_micros)
                    } else {
                        None
                    };
                    let views = self.call(self.store.read(&store_holds)).await.unwrap_or_default();
                    let error = Box::new(self.refused(&holds, &refusals, &views, now, &r.headers));
                    return Err(AttemptRefused { wait, error });
                }
                Err(_) => {}
            }
        }
        let uid = derived(r.uid, u128::from(attempt));
        self.degrade(r, holds, uid, now).await
    }

    /// A request whose admission reserved nothing has no record in the store yet: its first
    /// attempt's holds open the reservation, all or nothing.
    async fn open_with_attempt(
        &self,
        r: &mut Reservation,
        holds: Vec<PlannedHold>,
        store_holds: &[Hold],
    ) -> Result<(), AttemptRefused> {
        let req = AdmitRequest {
            uid: r.uid,
            node: &self.cfg.node,
            group: r.group,
            holds: store_holds,
        };
        match self.call(self.store.admit(&req)).await {
            Ok(AdmitDecision::Admitted { views }) => {
                r.holds = holds;
                r.views = views;
                r.remote = true;
                Ok(())
            }
            Ok(AdmitDecision::Replayed(ResvState::Held)) => {
                r.views = self.call(self.store.read(store_holds)).await.unwrap_or_default();
                r.holds = holds;
                r.remote = true;
                Ok(())
            }
            Ok(AdmitDecision::Replayed(_)) => Err(AttemptRefused {
                wait: None,
                error: Box::new(unavailable_error()),
            }),
            Ok(AdmitDecision::Refused { refusals }) => {
                let wait = if refusals.iter().all(|f| f.kind == RefusalKind::Rate) {
                    refusals
                        .iter()
                        .filter_map(|f| f.wait_us)
                        .max()
                        .map(Duration::from_micros)
                } else {
                    None
                };
                let views = self.call(self.store.read(store_holds)).await.unwrap_or_default();
                let error = Box::new(self.refused(&holds, &refusals, &views, r.admitted_at, &r.headers));
                Err(AttemptRefused { wait, error })
            }
            Err(_) => {
                let (uid, now) = (r.uid, r.admitted_at);
                self.degrade(r, holds, uid, now).await
            }
        }
    }

    fn alerts(&self, r: &Reservation, changes: &[crate::store::BudgetChange]) -> Vec<BudgetAlert> {
        let mut out = Vec::new();
        for c in changes {
            let Some(h) = r.holds.get(c.index) else { continue };
            let Some(period) = h.hold.rule.budget_period(r.admitted_at) else {
                continue;
            };
            let amount = &h.info.declared;
            for ratio in &h.info.alert_at {
                let threshold = ratio.0.saturating_mul(*amount);
                let before = c.before.saturating_mul(DEC_ONE);
                let after = c.after.saturating_mul(DEC_ONE);
                if before < threshold && threshold <= after {
                    out.push(BudgetAlert {
                        level: h.info.level.as_str(),
                        owner: h.info.owner.clone(),
                        name: h.info.name.clone(),
                        partition: h.info.partition.clone(),
                        unit: h.info.unit.name(),
                        amount: *amount,
                        spent: c.after,
                        threshold: *ratio,
                        period,
                    });
                }
            }
        }
        out
    }

    /// Settlement (stage `settle`): every hold replaced by its actual amount, concurrency released.
    /// A store that does not answer gets the settlement later from the replay queue.
    pub async fn settle(&self, r: &Reservation, s: &Settlement) -> SettleReport {
        let mut report = SettleReport {
            degraded: r.degraded,
            ..SettleReport::default()
        };
        if r.remote {
            let actual: Vec<u128> = r.holds.iter().map(|h| actual_of(h, s)).collect();
            let holds = r.store_holds();
            let keep_us = self.lease_us();
            let req = SettleRequest {
                uid: r.uid,
                node: &r.node,
                group: r.group,
                holds: &holds,
                actual: &actual,
                keep_us,
            };
            match self.call(self.store.reconcile(&req)).await {
                Ok(SettleOutcome::Settled { budgets }) => {
                    report.alerts = self.alerts(r, &budgets);
                    report.spent = budgets
                        .iter()
                        .filter_map(|c| {
                            let h = r.holds.get(c.index)?;
                            let period = h.hold.rule.budget_period(r.admitted_at)?;
                            Some(BudgetSpent {
                                identity: h.info.identity,
                                period_start: period.start,
                                spent: c.after,
                            })
                        })
                        .collect();
                }
                Ok(SettleOutcome::Noop(_)) => {}
                Err(_) => {
                    for h in &r.holds {
                        report
                            .degraded
                            .mark(h.info.unit.is_budget(), DegradedCause::StoreTimeout);
                    }
                    report.queued = true;
                    self.queue.push(Queued::Reconcile {
                        uid: r.uid,
                        node: r.node.clone(),
                        group: r.group,
                        holds,
                        actual,
                    });
                }
            }
        }
        for part in &r.local {
            let holds: Vec<Hold> = part.holds.iter().map(|(_, l, _)| l.clone()).collect();
            let actual: Vec<u128> = part
                .holds
                .iter()
                .map(|(p, _, scale)| actual_of(p, s).saturating_mul(*scale))
                .collect();
            let req = SettleRequest {
                uid: part.uid,
                node: &r.node,
                group: r.group,
                holds: &holds,
                actual: &actual,
                keep_us: 0,
            };
            let _ = self.local.reconcile(&req).await;
            for (i, (planned, _, _)) in part.holds.iter().enumerate() {
                if matches!(planned.hold.rule, Rule::Concurrency { .. }) {
                    continue;
                }
                let actual = actual_of(planned, s);
                report.queued = true;
                self.queue.push(Queued::Track {
                    uid: derived(part.uid, 0x2_0000 + i as u128),
                    node: r.node.clone(),
                    group: r.group,
                    hold: Hold {
                        mode: HoldMode::Shadow,
                        amount: actual,
                        ..planned.hold.clone()
                    },
                    actual,
                });
            }
        }
        for (i, h) in r.offline.iter().enumerate() {
            report.queued = true;
            self.queue.push(Queued::Track {
                uid: derived(r.uid, 0x1_0000 + i as u128),
                node: r.node.clone(),
                group: r.group,
                hold: h.hold.clone(),
                actual: actual_of(h, s),
            });
        }
        report
    }

    /// Replays queued settlements in order until one fails; the number applied.
    pub async fn flush(&self) -> usize {
        let mut done = 0;
        while let Some(q) = self.queue.pop() {
            let ok = match &q {
                Queued::Reconcile {
                    uid,
                    node,
                    group,
                    holds,
                    actual,
                } => {
                    let keep_us = self.lease_us();
                    let req = SettleRequest {
                        uid: *uid,
                        node,
                        group: *group,
                        holds,
                        actual,
                        keep_us,
                    };
                    self.call(self.store.reconcile(&req)).await.is_ok()
                }
                Queued::Track {
                    uid,
                    node,
                    group,
                    hold,
                    actual,
                } => {
                    let holds = [hold.clone()];
                    let req = AdmitRequest {
                        uid: *uid,
                        node,
                        group: *group,
                        holds: &holds,
                    };
                    let keep_us = self.lease_us();
                    let settle = SettleRequest {
                        uid: *uid,
                        node,
                        group: *group,
                        holds: &holds,
                        actual: &[*actual],
                        keep_us,
                    };
                    self.call(self.store.admit(&req)).await.is_ok()
                        && self.call(self.store.reconcile(&settle)).await.is_ok()
                }
            };
            if !ok {
                self.queue.push_front(q);
                break;
            }
            done += 1;
        }
        done
    }

    fn lease_us(&self) -> u64 {
        self.lease_us.load(Ordering::Relaxed)
    }

    /// Applies `limits.lease_ttl`.
    pub fn set_lease_ttl(&self, ttl: Duration) {
        let us = u64::try_from(ttl.as_micros()).unwrap_or(u64::MAX).max(1);
        self.lease_us.store(us, Ordering::Relaxed);
    }

    /// Renews this node's lease on its reservations; call every `lease_ttl / 3`.
    pub async fn heartbeat(&self) -> Result<(), StoreError> {
        let ttl = self.lease_us();
        self.call(self.store.heartbeat(&self.cfg.node, ttl)).await
    }

    /// Expires reservations of nodes whose lease is gone.
    pub async fn reap(&self, max: usize) -> Result<Vec<ExpiredReservation>, StoreError> {
        self.call(self.store.reap(max)).await
    }

    /// One health probe; true when the store is in use again (after three good probes in a row).
    pub async fn probe(&self) -> bool {
        let ok = matches!(
            tokio::time::timeout(self.cfg.store_timeout, self.store.ping()).await,
            Ok(Ok(()))
        );
        self.health.probed(ok)
    }
}

#[cfg(test)]
mod tests;
