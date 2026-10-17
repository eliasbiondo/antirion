//! The counter store: where counters and reservations live, behind one atomic protocol.

use std::future::Future;

use crate::counter::{CounterId, Hold};

pub mod memory;
#[cfg(test)]
mod scenarios;
pub mod valkey;

/// A reservation's identity: the gateway-generated request id (a UUIDv7), never the client's.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReservationId(pub u128);

impl ReservationId {
    #[must_use]
    pub fn hex(self) -> String {
        format!("{:032x}", self.0)
    }
}

/// The state of a reservation record.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ResvState {
    Held,
    Settled,
    Expired,
    /// A settlement arrived before its admission; the late admission must not hold anything.
    Cancelled,
}

impl ResvState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ResvState::Held => "held",
            ResvState::Settled => "settled",
            ResvState::Expired => "expired",
            ResvState::Cancelled => "cancelled",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<ResvState> {
        Some(match s {
            "held" => ResvState::Held,
            "settled" => ResvState::Settled,
            "expired" => ResvState::Expired,
            "cancelled" => ResvState::Cancelled,
            _ => return None,
        })
    }
}

/// Why a counter refused.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RefusalKind {
    /// It may fit later.
    Rate,
    /// It can never fit.
    Exceeds,
    /// A budget is exhausted for its period.
    Budget,
}

/// One counter's refusal, by its index in the holds of the call.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct StoreRefusal {
    pub index: usize,
    pub kind: RefusalKind,
    /// Microseconds until it would fit, for rate refusals.
    pub wait_us: Option<u64>,
}

/// A counter's state as headers and the Admin API show it.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct CounterView {
    /// Whole units still available (0 in debt).
    pub remaining: u128,
    /// Microseconds until the counter is back to its initial state (rate limits).
    pub full_in_us: u64,
    /// Budgets: settled spend and outstanding holds of the period.
    pub spent: u128,
    pub reserved: u128,
}

/// The outcome of an admission call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdmitDecision {
    /// Every hold was taken; a view per hold, after it.
    Admitted { views: Vec<CounterView> },
    /// Nothing was taken.
    Refused { refusals: Vec<StoreRefusal> },
    /// The reservation (or this attempt) already exists in this state: a retried call.
    Replayed(ResvState),
}

/// A budget's settled spend before and after a settlement, for threshold alerts.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BudgetChange {
    pub index: usize,
    pub before: u128,
    pub after: u128,
}

/// The outcome of a settlement call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettleOutcome {
    Settled {
        budgets: Vec<BudgetChange>,
    },
    /// Already settled, expired or cancelled; `None` when the admission had not landed (a
    /// tombstone was written).
    Noop(Option<ResvState>),
}

/// A reservation expired because its node stopped renewing its lease.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpiredReservation {
    pub uid: ReservationId,
    pub node: String,
    /// The holds, charged at their reservation (concurrency released).
    pub holds: Vec<Hold>,
}

/// The store did not answer, or answered something unreadable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("counter store unavailable: {0}")]
    Unavailable(String),
    #[error("counter store protocol error: {0}")]
    Protocol(String),
}

/// An adjustment that would make a budget's spend negative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the adjustment would make spend negative")]
pub struct NegativeSpend;

/// The admission of a reservation.
#[derive(Clone, Copy, Debug)]
pub struct AdmitRequest<'a> {
    pub uid: ReservationId,
    pub node: &'a str,
    /// The requesting organization's incarnation (groups the reservation record).
    pub group: u128,
    /// In canonical order, with distinct counters.
    pub holds: &'a [Hold],
}

/// Per-attempt holds added to a reservation.
#[derive(Clone, Copy, Debug)]
pub struct AttemptRequest<'a> {
    pub uid: ReservationId,
    pub group: u128,
    pub attempt: u16,
    pub holds: &'a [Hold],
}

/// The settlement of a reservation.
#[derive(Clone, Copy, Debug)]
pub struct SettleRequest<'a> {
    pub uid: ReservationId,
    /// The node that admitted the reservation.
    pub node: &'a str,
    pub group: u128,
    /// Every hold of the reservation (admission holds, then attempt holds), in order.
    pub holds: &'a [Hold],
    /// The actual amount of each hold.
    pub actual: &'a [u128],
    /// How long the closed record stays, for idempotency.
    pub keep_us: u64,
}

/// Counters and reservations, atomically. Every method is idempotent where the protocol needs it.
pub trait CounterStore: Send + Sync {
    /// Takes every hold, or none. Idempotent by `uid` for at least the idempotency window.
    fn admit(&self, req: &AdmitRequest<'_>)
    -> impl Future<Output = Result<AdmitDecision, StoreError>> + Send;

    /// Adds per-attempt holds to a held reservation, all or nothing. Idempotent by
    /// (`uid`, `attempt`).
    fn admit_attempt(
        &self,
        req: &AttemptRequest<'_>,
    ) -> impl Future<Output = Result<AdmitDecision, StoreError>> + Send;

    /// Replaces every hold (admission holds, then attempt holds, in order) by its actual amount,
    /// releases concurrency and closes the reservation, keeping its record for idempotency.
    fn reconcile(
        &self,
        req: &SettleRequest<'_>,
    ) -> impl Future<Output = Result<SettleOutcome, StoreError>> + Send;

    /// Renews a node's lease on all its reservations.
    fn heartbeat(&self, node: &str, ttl_us: u64) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Expires up to `max` reservations of nodes whose lease is gone.
    fn reap(&self, max: usize) -> impl Future<Output = Result<Vec<ExpiredReservation>, StoreError>> + Send;

    /// The current views of counters (amounts in `holds` are ignored).
    fn read(&self, holds: &[Hold]) -> impl Future<Output = Result<Vec<CounterView>, StoreError>> + Send;

    /// Raises a budget's settled spend for the period of its rule to at least `spent` (the rebuild
    /// from the ledger); a counter other nodes already charged further is kept.
    fn seed_budget(&self, hold: &Hold, spent: u128) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Raises each sub-window of a rolling budget to at least its charge in `slots` (the rebuild
    /// from the ledger, by sub-window index).
    fn seed_window(
        &self,
        hold: &Hold,
        slots: &[(i64, u128)],
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Adds `delta` (negative: a credit) to a budget's settled spend; the spend after it.
    fn adjust_budget(
        &self,
        hold: &Hold,
        delta: i128,
    ) -> impl Future<Output = Result<Result<u128, NegativeSpend>, StoreError>> + Send;

    /// A cheap liveness probe.
    fn ping(&self) -> impl Future<Output = Result<(), StoreError>> + Send;
}

/// The counter key of a hold's state: budgets are keyed by period start too.
pub(crate) fn state_key(h: &Hold) -> (CounterId, i64) {
    (h.counter, h.rule.period_start())
}

/// The node's counter store: process memory (standalone) or the shared Valkey (cluster).
#[derive(Debug)]
pub enum Counters {
    Memory(Box<memory::MemoryCounters>),
    Valkey(valkey::ValkeyCounters),
}

impl Counters {
    /// Drops closed reservations past their idempotency window and idle counters (memory only;
    /// the shared store expires its own keys).
    pub fn sweep(&self) {
        if let Counters::Memory(m) = self {
            m.sweep();
        }
    }

    /// Forgets every counter and reservation (memory only: a test-mode reset).
    pub fn clear(&self) {
        if let Counters::Memory(m) = self {
            m.clear();
        }
    }

    /// The shared store, in cluster mode.
    #[must_use]
    pub fn valkey(&self) -> Option<&valkey::ValkeyCounters> {
        match self {
            Counters::Valkey(v) => Some(v),
            Counters::Memory(_) => None,
        }
    }
}

impl CounterStore for Counters {
    async fn admit(&self, req: &AdmitRequest<'_>) -> Result<AdmitDecision, StoreError> {
        match self {
            Counters::Memory(m) => m.admit(req).await,
            Counters::Valkey(v) => v.admit(req).await,
        }
    }

    async fn admit_attempt(&self, req: &AttemptRequest<'_>) -> Result<AdmitDecision, StoreError> {
        match self {
            Counters::Memory(m) => m.admit_attempt(req).await,
            Counters::Valkey(v) => v.admit_attempt(req).await,
        }
    }

    async fn reconcile(&self, req: &SettleRequest<'_>) -> Result<SettleOutcome, StoreError> {
        match self {
            Counters::Memory(m) => m.reconcile(req).await,
            Counters::Valkey(v) => v.reconcile(req).await,
        }
    }

    async fn heartbeat(&self, node: &str, ttl_us: u64) -> Result<(), StoreError> {
        match self {
            Counters::Memory(m) => m.heartbeat(node, ttl_us).await,
            Counters::Valkey(v) => v.heartbeat(node, ttl_us).await,
        }
    }

    async fn reap(&self, max: usize) -> Result<Vec<ExpiredReservation>, StoreError> {
        match self {
            Counters::Memory(m) => m.reap(max).await,
            Counters::Valkey(v) => v.reap(max).await,
        }
    }

    async fn read(&self, holds: &[Hold]) -> Result<Vec<CounterView>, StoreError> {
        match self {
            Counters::Memory(m) => m.read(holds).await,
            Counters::Valkey(v) => v.read(holds).await,
        }
    }

    async fn seed_budget(&self, hold: &Hold, spent: u128) -> Result<(), StoreError> {
        match self {
            Counters::Memory(m) => m.seed_budget(hold, spent).await,
            Counters::Valkey(v) => v.seed_budget(hold, spent).await,
        }
    }

    async fn seed_window(&self, hold: &Hold, slots: &[(i64, u128)]) -> Result<(), StoreError> {
        match self {
            Counters::Memory(m) => m.seed_window(hold, slots).await,
            Counters::Valkey(v) => v.seed_window(hold, slots).await,
        }
    }

    async fn adjust_budget(
        &self,
        hold: &Hold,
        delta: i128,
    ) -> Result<Result<u128, NegativeSpend>, StoreError> {
        match self {
            Counters::Memory(m) => m.adjust_budget(hold, delta).await,
            Counters::Valkey(v) => v.adjust_budget(hold, delta).await,
        }
    }

    async fn ping(&self) -> Result<(), StoreError> {
        match self {
            Counters::Memory(m) => m.ping().await,
            Counters::Valkey(v) => v.ping().await,
        }
    }
}
