//! The health state of a deployment and its keys as the Admin API shows it.

use antirion_core::UtcMicros;

use super::{BreakerState, DeadReason, HealthRegistry, KeyInfo};
use crate::settings::HealthSettings;

/// One key's state, as the Admin API shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyView {
    pub key: String,
    pub dead_reason: Option<DeadReason>,
    pub scope_dead: bool,
    pub recheck_at: Option<UtcMicros>,
    /// `alive`, `cooling` or `dead`.
    pub pair_state: &'static str,
    pub cooldown_until: Option<UtcMicros>,
    /// When the key last answered an attempt with success on this node; `None` when it never has,
    /// so nothing says it works yet.
    pub last_success_at: Option<UtcMicros>,
}

/// One deployment's state, as the Admin API shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct DeploymentView {
    /// `closed`, `open`, `half_open` or `disabled`.
    pub state: &'static str,
    pub open_until: Option<UtcMicros>,
    pub trips: u32,
    pub consecutive: u32,
    pub window_requests: u32,
    pub window_failures: u32,
    pub cooldown_until: Option<UtcMicros>,
    pub disabled: bool,
    pub disabled_until: Option<UtcMicros>,
    pub keys: Vec<KeyView>,
    /// The window's failure rate when the breaker last tripped.
    pub trip_error_rate: Option<f64>,
}

impl HealthRegistry {
    /// The deployment's state for the Admin API.
    pub fn view(
        &self,
        d: &str,
        provider: &str,
        keys: &[KeyInfo],
        s: &HealthSettings,
        now: UtcMicros,
    ) -> DeploymentView {
        let mut inner = self.inner.lock();
        let dep = inner.dep(d, s, now);
        let (requests, failures) = dep.breaker.totals(&s.breaker, now);
        let mut view = DeploymentView {
            state: if dep.disabled.is_some() {
                "disabled"
            } else {
                dep.breaker.state.as_str()
            },
            open_until: dep
                .breaker
                .open_until
                .filter(|_| dep.breaker.state != BreakerState::Closed),
            trips: dep.breaker.trips,
            consecutive: dep.breaker.consecutive,
            window_requests: requests,
            window_failures: failures,
            cooldown_until: dep.cooldown_until.filter(|u| *u > now),
            disabled: dep.disabled.is_some(),
            disabled_until: dep.disabled.flatten(),
            keys: Vec::new(),
            trip_error_rate: dep.breaker.trip_rate,
        };
        let dep = inner.deployments.get(d);
        for k in keys {
            let scope = inner.keys.get(&(provider.to_owned(), k.name.clone()));
            let pair = dep.and_then(|d| d.pairs.get(&k.name));
            let pair_dead = pair.and_then(|p| p.dead.as_ref());
            let cooling = pair.and_then(|p| p.cooldown_until);
            let pair_state = if pair_dead.is_some() {
                "dead"
            } else if cooling.is_some_and(|u| u > now) {
                "cooling"
            } else {
                "alive"
            };
            view.keys.push(KeyView {
                key: k.name.clone(),
                dead_reason: scope.or(pair_dead).map(|d| d.reason),
                scope_dead: scope.is_some(),
                recheck_at: scope.or(pair_dead).map(|d| d.recheck_at),
                pair_state,
                cooldown_until: cooling,
                last_success_at: inner
                    .answered
                    .get(&(provider.to_owned(), k.name.clone()))
                    .copied(),
            });
        }
        view
    }
}
