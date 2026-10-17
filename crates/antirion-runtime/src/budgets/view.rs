//! A budget counter as the console reads it: its state, forecast and pace, its counts of refused
//! and downgraded requests in the period, and its running grants.

use antirion_core::UtcMicros;
use antirion_limits::applicable::NamedBudget;
use antirion_limits::forecast::{Basis, Forecast, Pace, blocking, forecast, pace};
use antirion_limits::spend::BudgetState;
use antirion_store::{GrantRow, StoreError};

use crate::Runtime;

/// What the Admin API shows of a budget counter beyond its declaration.
#[derive(Clone, Debug)]
pub struct BudgetView {
    pub state: BudgetState,
    /// `None` for a budget that makes no forecast (a rolling one).
    pub forecast: Option<Forecast>,
    pub pace: Pace,
    pub refused: u64,
    pub downgraded: u64,
    pub grants: Vec<GrantRow>,
}

impl Runtime {
    /// A budget counter's state with its forecast and pace, read from the counters and the ledger.
    /// A rolling budget has no end to project to, so it has no forecast.
    pub async fn budget_forecast(
        &self,
        b: &NamedBudget<'_>,
        now: UtcMicros,
    ) -> Result<(BudgetState, Option<Forecast>, Pace), StoreError> {
        let state = self
            .accounts
            .budget_state(b, now)
            .await
            .map_err(|_| StoreError::Unavailable)?;
        let basis = Basis {
            spent: state.view.spent,
            amount: b.quota,
            effective: state.effective,
            period: state.period,
            now,
        };
        let f = if b.budget.forecasts() {
            let trailing = self
                .accounts
                .charged(b.identity, basis.window())
                .await
                .map_err(|_| StoreError::Unavailable)?;
            let f = forecast(&basis, trailing);
            Some(if b.budget.hard { blocking(f, &basis) } else { f })
        } else {
            None
        };
        let p = pace(&basis, f.as_ref(), b.budget.hard, &b.budget.alert_at);
        Ok((state, f, p))
    }

    /// Everything the Admin API shows of a budget counter. Counts noticed on this node are
    /// recorded first, so they are included.
    pub async fn budget_view(&self, b: &NamedBudget<'_>) -> Result<BudgetView, StoreError> {
        let now = self.now();
        let (state, forecast, pace) = self.budget_forecast(b, now).await?;
        let identity = b.identity.hex();
        let (refused, downgraded) = self.store.tally(&identity, state.period.start.0).await?;
        let grants = self.store.running_grants(Some(&identity), now.0).await?;
        Ok(BudgetView {
            state,
            forecast,
            pace,
            refused: u64::try_from(refused).unwrap_or(0),
            downgraded: u64::try_from(downgraded).unwrap_or(0),
            grants,
        })
    }
}
