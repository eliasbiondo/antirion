//! Budgets that let cache hits through (`cache_hits_bypass_budgets`): such a budget's refusal at
//! admission is held back until the cache lookup. The request is admitted without those budgets; a
//! hit is served without consulting them, and a miss gets the held refusal with no upstream call.

use antirion_error::GatewayError;

use crate::engine::Reservation;
use crate::plan::{AdmitPlan, PlannedHold};
use crate::spend::Accounts;

/// A budget refusal waiting for the cache lookup.
#[derive(Debug)]
pub struct DeferredRefusal {
    error: GatewayError,
    budgets: Vec<PlannedHold>,
}

/// Whether `e` names one of `holds` (by level and declaration name) that lets cache hits through.
fn refused_by_bypassing(e: &GatewayError, holds: &[PlannedHold]) -> bool {
    if e.code != antirion_error::Code::BudgetExceeded {
        return false;
    }
    let Some(limit) = e.limit.as_deref() else {
        return false;
    };
    let field = |k: &str| limit.get(k).and_then(serde_json::Value::as_str);
    holds.iter().any(|h| {
        h.info.bypass_on_hit
            && Some(h.info.level.as_str()) == field("level")
            && Some(h.info.name.as_str()) == field("name")
    })
}

impl Accounts {
    /// Admission in which a refusal by a budget with `cache_hits_bypass_budgets` is deferred: the
    /// plan is admitted again without those budgets, and the refusal is returned for a cache miss.
    pub async fn admit_deferring(
        &self,
        plan: AdmitPlan,
    ) -> Result<(Reservation, Option<DeferredRefusal>), GatewayError> {
        if !plan.holds.iter().any(|h| h.info.bypass_on_hit) {
            return self.admit(plan).await.map(|r| (r, None));
        }
        let mut without = plan.clone();
        without.holds.retain(|h| !h.info.bypass_on_hit);
        let (admitted, budgets) = self.admit_unnoted(plan).await;
        match admitted {
            Ok(r) => Ok((r, None)),
            Err(e) if refused_by_bypassing(&e, &budgets) => {
                let r = self.admit(without).await?;
                Ok((r, Some(DeferredRefusal { error: e, budgets })))
            }
            Err(e) => {
                self.note_refusal(&e, &budgets);
                Err(e)
            }
        }
    }

    /// A cache miss applies the deferred refusal: it is counted against its budget like any
    /// refusal at admission, and is the request's error.
    pub fn refuse_deferred(&self, d: DeferredRefusal) -> GatewayError {
        self.note_refusal(&d.error, &d.budgets);
        d.error
    }
}
