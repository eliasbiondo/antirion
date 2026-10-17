//! What the console adds to budgets beyond their declarations, kept in node memory so the request
//! path never waits on a store for it: overage grants that raise a budget's effective amount until
//! they expire, the last settled spend seen for each budget (downgrades read it), and the refused
//! and downgraded requests counted since the runtime last collected them.

use std::collections::HashMap;

use antirion_core::UtcMicros;
use parking_lot::{Mutex, RwLock};

use crate::counter::{CounterId, Rule};
use crate::level::Level;
use crate::period::PeriodBounds;
use crate::plan::{HoldInfo, PlannedHold};

/// A grant as admission applies it: `amount` more until `until`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveGrant {
    pub id: String,
    pub amount: u128,
    pub until: UtcMicros,
}

/// The budget of a refused or downgraded request, as the runtime needs it to record the period's
/// events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Touched {
    pub identity: CounterId,
    pub level: Level,
    pub owner: String,
    pub name: String,
    pub partition: Vec<(String, String)>,
    pub period: PeriodBounds,
}

/// Requests counted for one budget and period since the last collection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tally {
    pub budget: Touched,
    pub refused: u64,
    pub downgraded: u64,
}

#[derive(Debug, Default)]
pub struct Overlay {
    grants: RwLock<HashMap<CounterId, Vec<ActiveGrant>>>,
    /// Per budget identity: the start of the period and the settled spend last seen in it.
    seen: RwLock<HashMap<CounterId, (UtcMicros, u128)>>,
    pending: Mutex<HashMap<(CounterId, i64), Tally>>,
}

impl Overlay {
    /// Replaces every grant.
    pub fn set_grants(&self, grants: HashMap<CounterId, Vec<ActiveGrant>>) {
        *self.grants.write() = grants;
    }

    /// What the grants of `identity` running at `now` add to its amount.
    #[must_use]
    pub fn extra(&self, identity: CounterId, now: UtcMicros) -> u128 {
        self.grants.read().get(&identity).map_or(0, |gs| {
            gs.iter()
                .filter(|g| g.until > now)
                .fold(0u128, |acc, g| acc.saturating_add(g.amount))
        })
    }

    /// Raises the amount of every budget hold by its running grants.
    pub fn raise(&self, holds: &mut [PlannedHold], now: UtcMicros) {
        let grants = self.grants.read();
        if grants.is_empty() {
            return;
        }
        for h in holds.iter_mut() {
            let extra = grants.get(&h.info.identity).map_or(0, |gs| {
                gs.iter()
                    .filter(|g| g.until > now)
                    .fold(0u128, |acc, g| acc.saturating_add(g.amount))
            });
            match &mut h.hold.rule {
                Rule::Budget { amount, .. } => *amount = amount.saturating_add(extra),
                Rule::Rolling(w) => {
                    w.limit = w.limit.saturating_add(u64::try_from(extra).unwrap_or(u64::MAX));
                }
                _ => {}
            }
        }
    }

    /// Records a budget's settled spend in the period starting at `period_start`; an older period's
    /// figure never replaces a newer one's.
    pub fn saw(&self, identity: CounterId, period_start: UtcMicros, spent: u128) {
        let mut seen = self.seen.write();
        let entry = seen.entry(identity).or_insert((period_start, spent));
        if entry.0 <= period_start {
            *entry = (period_start, spent);
        }
    }

    /// The settled spend last seen for a budget in the period starting at `period_start`; zero when
    /// only an earlier period was seen.
    #[must_use]
    pub fn spent(&self, identity: CounterId, period_start: UtcMicros) -> Option<u128> {
        match self.seen.read().get(&identity) {
            Some((start, spent)) if *start == period_start => Some(*spent),
            Some((start, _)) if *start < period_start => Some(0),
            _ => None,
        }
    }

    fn count(&self, budget: Touched, refused: u64, downgraded: u64) {
        let mut pending = self.pending.lock();
        let t = pending
            .entry((budget.identity, budget.period.start.0))
            .or_insert_with(|| Tally {
                budget,
                refused: 0,
                downgraded: 0,
            });
        t.refused = t.refused.saturating_add(refused);
        t.downgraded = t.downgraded.saturating_add(downgraded);
    }

    /// Counts a request the budget of `info` refused in `period`.
    pub fn refused(&self, info: &HoldInfo, period: PeriodBounds) {
        self.count(touched(info, period), 1, 0);
    }

    /// Counts a request the budget sent to its downgrade target.
    pub fn downgraded(&self, budget: Touched) {
        self.count(budget, 0, 1);
    }

    /// The counts gathered since the last call.
    #[must_use]
    pub fn take(&self) -> Vec<Tally> {
        self.pending.lock().drain().map(|(_, t)| t).collect()
    }

    /// Forgets spend and counts (grants are reloaded by their owner).
    pub fn clear(&self) {
        self.seen.write().clear();
        self.pending.lock().clear();
        self.grants.write().clear();
    }
}

fn touched(info: &HoldInfo, period: PeriodBounds) -> Touched {
    Touched {
        identity: info.identity,
        level: info.level,
        owner: info.owner.clone(),
        name: info.name.clone(),
        partition: info.partition.clone(),
        period,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::counter::{Hold, HoldMode};
    use crate::decl::OnUnavailable;
    use crate::plan::Unit;

    fn budget_hold(identity: CounterId, amount: u128) -> PlannedHold {
        PlannedHold {
            hold: Hold {
                counter: identity,
                group: 0,
                rule: Rule::Budget {
                    amount,
                    period: PeriodBounds {
                        start: UtcMicros(0),
                        end: None,
                    },
                },
                amount: 1,
                mode: HoldMode::Enforce,
            },
            info: HoldInfo {
                level: Level::Key,
                owner: "k".into(),
                name: "b".into(),
                unit: Unit::Usd,
                on_unavailable: OnUnavailable::FailClosed,
                per_us: 0,
                alert_at: Vec::new(),
                partition: Vec::new(),
                identity,
                declared: amount,
                bypass_on_hit: false,

                class: None,
            },
        }
    }

    #[test]
    fn running_grants_raise_the_amount_until_they_expire() {
        let o = Overlay::default();
        let id = CounterId([7; 16]);
        o.set_grants(HashMap::from([(
            id,
            vec![
                ActiveGrant {
                    id: "a".into(),
                    amount: 100,
                    until: UtcMicros(50),
                },
                ActiveGrant {
                    id: "b".into(),
                    amount: 10,
                    until: UtcMicros(20),
                },
            ],
        )]));
        assert_eq!(o.extra(id, UtcMicros(10)), 110);
        assert_eq!(o.extra(id, UtcMicros(20)), 100);
        assert_eq!(o.extra(id, UtcMicros(50)), 0);
        let mut holds = vec![budget_hold(id, 1000), budget_hold(CounterId([1; 16]), 5)];
        o.raise(&mut holds, UtcMicros(30));
        let amounts: Vec<u128> = holds
            .iter()
            .filter_map(|h| match h.hold.rule {
                Rule::Budget { amount, .. } => Some(amount),
                _ => None,
            })
            .collect();
        assert_eq!(amounts, vec![1100, 5]);
        assert_eq!(holds[0].info.declared, 1000);
    }

    #[test]
    fn spend_is_kept_per_period_and_counts_are_collected_once() {
        let o = Overlay::default();
        let id = CounterId([2; 16]);
        o.saw(id, UtcMicros(100), 40);
        o.saw(id, UtcMicros(0), 99);
        assert_eq!(o.spent(id, UtcMicros(100)), Some(40));
        assert_eq!(o.spent(id, UtcMicros(200)), Some(0));
        assert_eq!(o.spent(id, UtcMicros(50)), None);
        let h = budget_hold(id, 10);
        let p = PeriodBounds {
            start: UtcMicros(100),
            end: Some(UtcMicros(200)),
        };
        o.refused(&h.info, p);
        o.refused(&h.info, p);
        o.downgraded(touched(&h.info, p));
        let t = o.take();
        assert_eq!(t.len(), 1);
        assert_eq!((t[0].refused, t[0].downgraded), (2, 1));
        assert!(o.take().is_empty());
    }
}
