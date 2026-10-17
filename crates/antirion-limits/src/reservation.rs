//! A request's reservation from admission to settlement, and what settlement reports.

use std::time::Duration;

use antirion_core::UtcMicros;
use antirion_error::GatewayError;

use crate::counter::Hold;
use crate::degraded::Degraded;
use crate::estimate::Estimate;
use crate::headers::{self, PolicyState};
use crate::money::{Dec, Nusd};
use crate::period::PeriodBounds;
use crate::plan::{AdmitPlan, PlannedHold};
use crate::settings::HeaderSettings;
use crate::store::{CounterView, ReservationId};
use crate::usage::Usage;

/// Holds taken on the node's local store while the shared store was unavailable.
#[derive(Clone, Debug)]
pub(crate) struct LocalPart {
    pub(crate) uid: ReservationId,
    /// Each planned hold, its local form, and the factor its amounts were multiplied by.
    pub(crate) holds: Vec<(PlannedHold, Hold, u128)>,
}

/// An admitted request's reservation, kept until settlement.
#[derive(Clone, Debug)]
pub struct Reservation {
    pub uid: ReservationId,
    pub group: u128,
    pub node: String,
    /// Holds in the shared store: admission holds, then attempt holds, in store order.
    pub holds: Vec<PlannedHold>,
    /// The state of each of `holds` right after it was taken (headers).
    pub views: Vec<CounterView>,
    /// Whether the shared store has a record of this reservation.
    pub remote: bool,
    pub(crate) local: Vec<LocalPart>,
    /// Soft budgets of a request admitted while the store was down; their spend is replayed.
    pub(crate) offline: Vec<PlannedHold>,
    pub estimate: Estimate,
    pub hedge: u32,
    pub usd: Option<Nusd>,
    pub admitted_at: UtcMicros,
    pub headers: HeaderSettings,
    pub degraded: Degraded,
}

impl Reservation {
    pub(crate) fn new(plan: &AdmitPlan, node: &str) -> Reservation {
        Reservation {
            uid: plan.uid,
            group: plan.group,
            node: node.to_owned(),
            holds: Vec::new(),
            views: Vec::new(),
            remote: false,
            local: Vec::new(),
            offline: Vec::new(),
            estimate: plan.estimate,
            hedge: plan.hedge,
            usd: plan.usd,
            admitted_at: plan.admitted_at,
            headers: plan.headers.clone(),
            degraded: Degraded::default(),
        }
    }

    /// The rate limits of this request after its reservation, most constrained first.
    #[must_use]
    pub fn rate_limit_states(&self) -> Vec<PolicyState> {
        headers::policy_states(&self.holds, &self.views)
    }

    /// `RateLimit-Policy` and `RateLimit`, when IETF headers are enabled and a limit applies.
    #[must_use]
    pub fn ietf_headers(&self) -> Option<(String, String)> {
        if !self.headers.ietf {
            return None;
        }
        headers::ietf(&self.rate_limit_states(), self.headers.max_policies)
    }

    /// `x-antirion-reservation`, when enabled.
    #[must_use]
    pub fn reservation_header(&self) -> Option<String> {
        self.headers
            .expose_reservation
            .then(|| headers::reservation(&self.estimate, self.hedge, self.usd))
    }

    /// `x-antirion-budget-remaining` and `x-antirion-budget-reset`.
    #[must_use]
    pub fn budget_headers(&self) -> Option<(String, Option<String>)> {
        headers::budget(&self.holds, &self.views)
    }

    pub(crate) fn store_holds(&self) -> Vec<Hold> {
        self.holds.iter().map(|h| h.hold.clone()).collect()
    }
}

/// What a request consumed, summed over every upstream call made for it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settlement {
    pub usage: Usage,
    /// The billed cost of the request's charged lines (0 when unpriced).
    pub billed: Nusd,
    /// A 2xx final response (a complete stream for streams): `count: successful` keeps its hold.
    pub success: bool,
}

/// A budget crossed one of its `alert_at` ratios with this settlement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetAlert {
    pub level: &'static str,
    pub owner: String,
    pub name: String,
    pub partition: Vec<(String, String)>,
    pub unit: &'static str,
    pub amount: u128,
    pub spent: u128,
    pub threshold: Dec,
    pub period: PeriodBounds,
}

/// A budget's settled spend in its period right after a settlement.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BudgetSpent {
    pub identity: crate::counter::CounterId,
    pub period_start: UtcMicros,
    pub spent: u128,
}

/// The outcome of a settlement.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettleReport {
    pub alerts: Vec<BudgetAlert>,
    /// The settled spend of each budget the settlement charged in the shared store.
    pub spent: Vec<BudgetSpent>,
    pub degraded: Degraded,
    /// The settlement waits in the replay queue.
    pub queued: bool,
}

/// Per-attempt counters refused: the position is ineligible for this attempt.
#[derive(Debug)]
pub struct AttemptRefused {
    /// When the position could fit again; absent when waiting cannot help.
    pub wait: Option<Duration>,
    pub error: Box<GatewayError>,
}
