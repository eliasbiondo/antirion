//! What the attempt loop keeps for one request: the deployments and keys tried, how often each
//! deployment was retried, and the steps that open an attempt (a position, a key, its probe slots
//! and the retry budget).

use std::collections::HashMap;
use std::time::Duration;

use antirion_core::UtcMicros;
use tokio::time::Instant;

use super::{AttemptTimers, KeyPick, LoopCx, Planner, Selected, capped};
use crate::health::{KeyMode, Ticket};

/// The time spent between a timer's expiry and the loop's next decision, which the
/// minimum-attempt-time rule does not hold against the next attempt.
pub(super) const MEASUREMENT_SLACK: Duration = Duration::from_millis(50);

/// How long a probe's lease lives when no timer bounds the attempt.
const LEASE_TTL: Duration = Duration::from_secs(600);

/// The skip reason of a retry the deployment's retry budget refused.
pub(super) const RETRY_BUDGET: &str = "retry_budget";

/// The attempt about to be sent.
pub(super) struct Current {
    pub(super) sel: Selected,
    pub(super) key: Option<String>,
    pub(super) ticket: Ticket,
    pub(super) via: Option<String>,
    pub(super) wait: Duration,
    pub(super) moved: bool,
}

impl Current {
    /// The next attempt on the same position: another key, or the same pair again.
    pub(super) fn again(sel: Selected, key: Option<String>, ticket: Ticket, wait: Duration) -> Current {
        Current {
            sel,
            key,
            ticket,
            via: None,
            wait,
            moved: false,
        }
    }
}

pub(super) struct Loop<'c, 'a> {
    pub(super) cx: &'c LoopCx<'a>,
    pub(super) tried: Vec<String>,
    tried_keys: HashMap<String, Vec<String>>,
    pub(super) retries_same: HashMap<String, u32>,
}

impl<'c, 'a> Loop<'c, 'a> {
    pub(super) fn new(cx: &'c LoopCx<'a>) -> Loop<'c, 'a> {
        Loop {
            cx,
            tried: Vec::new(),
            tried_keys: HashMap::new(),
            retries_same: HashMap::new(),
        }
    }

    pub(super) fn now(&self) -> UtcMicros {
        self.cx.clock.now()
    }

    pub(super) fn keys_of(&self, d: &str) -> &[String] {
        self.tried_keys.get(d).map_or(&[], Vec::as_slice)
    }

    /// The request runs as if `d`'s breaker were closed.
    pub(super) fn ignores(&self, d: &str) -> bool {
        self.cx.ignore_health.iter().any(|x| x == d)
    }

    /// Records an attempt that was sent: a deployment's first in this request counts toward its
    /// first attempts, which the retry budget is relative to.
    pub(super) fn sent(&mut self, d: &str, key: Option<&str>) {
        if !self.tried.iter().any(|t| t == d) {
            self.cx.health.budget.first(d, self.now());
            self.tried.push(d.to_owned());
        }
        if let Some(k) = key {
            self.tried_keys
                .entry(d.to_owned())
                .or_default()
                .push(k.to_owned());
        }
    }

    /// Takes a retry onto `d` from its budget; false when the budget is spent.
    pub(super) fn retry_allowed(&self, d: &str) -> bool {
        self.cx
            .health
            .budget
            .retry(d, &self.cx.health_cfg.budget, self.now())
    }

    pub(super) fn begin(&self, sel: &Selected, key: &KeyPick) -> Option<(Option<String>, Ticket)> {
        let key = match key {
            KeyPick::Key(k) => Some(k.clone()),
            KeyPick::Keyless => None,
            KeyPick::None => return None,
        };
        let s = self.cx.health_cfg.for_deployment(&sel.deployment);
        let health = self.cx.health;
        let ticket = if self.ignores(&sel.deployment) {
            health.begin_ignoring_breaker(&sel.deployment, &sel.provider, key.as_deref(), s, self.now())
        } else {
            health.begin(&sel.deployment, &sel.provider, key.as_deref(), s, self.now())
        }?;
        Some((key, ticket))
    }

    /// A new position of the current tier: selected, gated, with a key and its probe slots.
    pub(super) async fn fresh<P: Planner>(&self, plan: &mut P) -> Option<Current> {
        loop {
            let sel = plan.select(&self.tried)?;
            if let Err(reason) = plan.gate(&sel).await {
                plan.skip(&sel, reason);
                continue;
            }
            let pick = plan.select_key(&sel, self.keys_of(&sel.deployment), None, KeyMode::Untried);
            if pick == KeyPick::None {
                plan.skip(&sel, "no_eligible_key");
                continue;
            }
            let Some((key, mut ticket)) = self.begin(&sel, &pick) else {
                plan.skip(&sel, "ineligible");
                continue;
            };
            let settings = self.cx.health_cfg.for_deployment(&sel.deployment);
            if !self
                .cx
                .health
                .lease(&mut ticket, settings, self.lease_ttl(&sel))
                .await
            {
                self.cx.health.abandon(&ticket);
                plan.skip(&sel, "ineligible");
                continue;
            }
            return Some(Current {
                via: sel.via.clone(),
                sel,
                key,
                ticket,
                wait: Duration::ZERO,
                moved: true,
            });
        }
    }

    /// How long a probe's lease may outlive its verdict: the attempt's timer, within the deadline.
    fn lease_ttl(&self, sel: &Selected) -> Duration {
        let t = &sel.policy.timeouts;
        [t.attempt, self.cx.deadline]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(LEASE_TTL)
    }

    pub(super) fn left(&self, deadline_at: Option<Instant>) -> Option<Duration> {
        deadline_at.map(|d| d.saturating_duration_since(Instant::now()))
    }

    /// The timers of an attempt on `sel`.
    pub(super) fn timers(&self, sel: &Selected, deadline_at: Option<Instant>) -> AttemptTimers {
        let configured = sel.policy.timeouts.clone();
        AttemptTimers {
            connect: configured.connect,
            timeouts: capped(&configured, self.cx.read_timeout),
            configured,
            deadline_at,
            deadline: self.cx.deadline,
        }
    }
}
