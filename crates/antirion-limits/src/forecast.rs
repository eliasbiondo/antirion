//! A budget's linear forecast and its pacing state. The rate is the spend charged over the trailing
//! `min(elapsed, 7d)` of the current period; the projection extends it to the period's end.

use antirion_core::UtcMicros;

use crate::money::{DEC_ONE, Dec};
use crate::period::PeriodBounds;

const DAY_US: i64 = 86_400_000_000;
const WEEK_US: i64 = 7 * DAY_US;

/// What a forecast is computed from.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Basis {
    /// Settled spend of the period.
    pub spent: u128,
    /// The declared amount.
    pub amount: u128,
    /// The amount admission refuses at (declared plus running grants).
    pub effective: u128,
    pub period: PeriodBounds,
    pub now: UtcMicros,
}

impl Basis {
    /// The window the rate is measured over: the trailing `min(elapsed, 7d)` of the period.
    #[must_use]
    pub fn window(&self) -> PeriodBounds {
        let from = self.period.start.0.max(self.now.0.saturating_sub(WEEK_US));
        PeriodBounds {
            start: UtcMicros(from.min(self.now.0)),
            end: Some(self.now),
        }
    }
}

/// A budget's projection to the end of its period.
#[derive(Clone, Debug, PartialEq)]
pub struct Forecast {
    /// Spend per day at the measured rate, in the budget's unit.
    pub rate_per_day: u128,
    pub projected_spend: u128,
    /// `projected_spend / amount`; `None` for a zero amount.
    pub projected_ratio: Option<f64>,
    /// When the spend would reach the effective amount, if before the period ends; `None` when the
    /// rate is zero or the amount is already reached.
    pub runs_out_at: Option<UtcMicros>,
    pub basis_from: UtcMicros,
    pub basis_to: UtcMicros,
}

/// A forecast of a budget that refuses at its effective amount: once that is reached nothing more is
/// spent, so it projects what it spent at no rate. Before, the projection past the amount says how
/// far the pace would go.
#[must_use]
pub fn blocking(mut f: Forecast, basis: &Basis) -> Forecast {
    if basis.spent >= basis.effective {
        f.rate_per_day = 0;
        f.runs_out_at = None;
        f.projected_spend = basis.spent;
        #[allow(clippy::cast_precision_loss)]
        {
            f.projected_ratio = (basis.amount > 0).then(|| round4(basis.spent as f64 / basis.amount as f64));
        }
    }
    f
}

/// The forecast of `basis`, given the spend `trailing` charged within [`Basis::window`].
#[must_use]
pub fn forecast(basis: &Basis, trailing: i128) -> Forecast {
    let window = basis.window();
    let duration = u128::try_from(basis.now.0.saturating_sub(window.start.0)).unwrap_or(0);
    let trailing = u128::try_from(trailing).unwrap_or(0);
    let over = |span: u128| -> u128 { trailing.saturating_mul(span).checked_div(duration).unwrap_or(0) };
    let remaining = basis.period.end.map_or(0, |end| {
        u128::try_from(end.0.saturating_sub(basis.now.0)).unwrap_or(0)
    });
    let projected_spend = basis.spent.saturating_add(over(remaining));
    #[allow(clippy::cast_precision_loss)]
    let projected_ratio = (basis.amount > 0).then(|| round4(projected_spend as f64 / basis.amount as f64));
    let runs_out_at = if basis.spent >= basis.effective || trailing == 0 || duration == 0 {
        None
    } else {
        let left = basis.effective - basis.spent;
        let t = left.saturating_mul(duration).div_ceil(trailing);
        let at = basis.now.0.saturating_add(i64::try_from(t).unwrap_or(i64::MAX));
        basis
            .period
            .end
            .is_none_or(|end| at <= end.0)
            .then_some(UtcMicros(at))
    };
    Forecast {
        rate_per_day: over(u128::try_from(DAY_US).unwrap_or(0)),
        projected_spend,
        projected_ratio,
        runs_out_at,
        basis_from: window.start,
        basis_to: basis.now,
    }
}

fn round4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

/// How a budget is pacing.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Pace {
    OnPace,
    AtRisk,
    Over,
    Blocked,
}

impl Pace {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Pace::OnPace => "on_pace",
            Pace::AtRisk => "at_risk",
            Pace::Over => "over",
            Pace::Blocked => "blocked",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Pace> {
        Some(match s {
            "on_pace" => Pace::OnPace,
            "at_risk" => Pace::AtRisk,
            "over" => Pace::Over,
            "blocked" => Pace::Blocked,
            _ => return None,
        })
    }
}

/// The state of a budget: `blocked` when it refuses (hard or approval, spent at its effective
/// amount), `over` when spent has reached the declared amount, `at_risk` when the projection
/// reaches the amount or spent has reached the highest alert ratio, else `on_pace`. A budget
/// without a forecast (a rolling one) is at risk only past its alert ratio.
#[must_use]
pub fn pace(basis: &Basis, forecast: Option<&Forecast>, refuses: bool, alert_at: &[Dec]) -> Pace {
    if refuses && basis.spent >= basis.effective {
        return Pace::Blocked;
    }
    if basis.spent >= basis.amount {
        return Pace::Over;
    }
    let past_alert = alert_at
        .iter()
        .max()
        .is_some_and(|r| basis.spent.saturating_mul(DEC_ONE) >= r.0.saturating_mul(basis.amount));
    if forecast.and_then(|f| f.projected_ratio).is_some_and(|r| r >= 1.0) || past_alert {
        return Pace::AtRisk;
    }
    Pace::OnPace
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: i64 = 1_000 * DAY_US;

    fn basis(spent: u128, day: i64) -> Basis {
        Basis {
            spent,
            amount: 3_000,
            effective: 3_000,
            period: PeriodBounds {
                start: UtcMicros(START),
                end: Some(UtcMicros(START + 30 * DAY_US)),
            },
            now: UtcMicros(START + day * DAY_US),
        }
    }

    #[test]
    fn a_blocked_budget_projects_what_it_spent() {
        let b = basis(1_000, 10);
        assert_eq!(
            blocking(forecast(&b, 2_100), &b),
            forecast(&b, 2_100),
            "not blocked yet"
        );
        let blocked = basis(3_100, 10);
        let f = blocking(forecast(&blocked, 2_100), &blocked);
        assert_eq!(
            (f.projected_spend, f.rate_per_day, f.runs_out_at),
            (3_100, 0, None)
        );
    }

    #[test]
    fn a_steady_rate_projects_to_the_period_end() {
        let b = basis(1_000, 10);
        assert_eq!(b.window().start, UtcMicros(START + 3 * DAY_US));
        let f = forecast(&b, 700);
        assert_eq!(f.rate_per_day, 100);
        assert_eq!(f.projected_spend, 3_000);
        assert_eq!(f.projected_ratio, Some(1.0));
        assert_eq!(f.runs_out_at, Some(UtcMicros(START + 30 * DAY_US)));
        assert_eq!(pace(&b, Some(&f), true, &[]), Pace::AtRisk);
    }

    #[test]
    fn a_faster_rate_runs_out_early_and_is_at_risk() {
        let b = basis(2_000, 10);
        let f = forecast(&b, 1_400);
        assert_eq!(f.rate_per_day, 200);
        assert_eq!(f.projected_spend, 6_000);
        assert_eq!(f.runs_out_at, Some(UtcMicros(START + 15 * DAY_US)));
        assert_eq!(pace(&b, Some(&f), true, &[]), Pace::AtRisk);
        let slow = forecast(&basis(100, 10), 70);
        assert_eq!(pace(&basis(100, 10), Some(&slow), true, &[]), Pace::OnPace);
        assert_eq!(slow.runs_out_at, None);
    }

    #[test]
    fn early_in_a_period_the_elapsed_time_is_the_basis() {
        let b = basis(200, 2);
        assert_eq!(b.window().start, UtcMicros(START));
        assert_eq!(forecast(&b, 200).rate_per_day, 100);
        let first = basis(0, 0);
        let f = forecast(&first, 0);
        assert_eq!((f.rate_per_day, f.projected_spend, f.runs_out_at), (0, 0, None));
    }

    #[test]
    fn states_in_order_of_precedence() {
        let mut b = basis(3_000, 20);
        let f = forecast(&b, 700);
        assert_eq!(pace(&b, Some(&f), true, &[]), Pace::Blocked);
        assert_eq!(pace(&b, Some(&f), false, &[]), Pace::Over);
        b.effective = 3_100;
        assert_eq!(pace(&b, Some(&f), true, &[]), Pace::Over);
        let low = basis(2_800, 29);
        let f = forecast(&low, 0);
        assert_eq!(pace(&low, Some(&f), true, &[]), Pace::OnPace);
        assert_eq!(
            pace(&low, Some(&f), true, &[Dec::from_ratio(0.9).unwrap()]),
            Pace::AtRisk
        );
    }

    #[test]
    fn lifetime_budgets_project_their_spend() {
        let b = Basis {
            spent: 10,
            amount: 100,
            effective: 100,
            period: PeriodBounds {
                start: UtcMicros::EPOCH,
                end: None,
            },
            now: UtcMicros(START),
        };
        let f = forecast(&b, 70);
        assert_eq!(f.projected_spend, 10);
        assert_eq!(f.rate_per_day, 10);
        assert_eq!(f.runs_out_at, Some(UtcMicros(START + 9 * DAY_US)));
        assert_eq!(f.projected_ratio, Some(0.1));
    }

    #[test]
    fn without_a_forecast_only_the_alert_ratio_puts_a_budget_at_risk() {
        let b = basis(2_000, 10);
        assert_eq!(pace(&b, Some(&forecast(&b, 1_400)), true, &[]), Pace::AtRisk);
        assert_eq!(pace(&b, None, true, &[]), Pace::OnPace);
        let alert = [Dec::from_ratio(0.5).unwrap()];
        assert_eq!(pace(&b, None, true, &alert), Pace::AtRisk);
        assert_eq!(pace(&basis(3_000, 10), None, true, &[]), Pace::Blocked);
    }
}
