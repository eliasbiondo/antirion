//! Verdicts reaching the health state: an attempt's, a failure after a stream's commit, and an
//! active check's.

use std::time::Duration;

use antirion_core::UtcMicros;

use super::breaker::{BreakerState, SuccessMark, after, doubled};
use super::{Dead, DeadReason, HealthRegistry, Inner, Ticket, Verdict};
use crate::class::{BreakerEffect, Class};
use crate::settings::HealthSettings;

/// A success recorded at a stream's commit; a failure of the stream after its commit takes its
/// place ([`HealthRegistry::failed_after_commit`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Committed {
    deployment: String,
    mark: Option<SuccessMark>,
}

impl Committed {
    #[must_use]
    pub fn deployment(&self) -> &str {
        &self.deployment
    }
}

impl HealthRegistry {
    /// Records an attempt's verdict and releases its probe slots. A success returns what a later
    /// failure of the same stream needs.
    pub fn finish(&self, t: &Ticket, v: &Verdict, s: &HealthSettings, now: UtcMicros) -> Option<Committed> {
        let mut inner = self.inner.lock();
        let effect = if v.counts && v.class != Class::ClientCancelled {
            v.class.breaker()
        } else {
            BreakerEffect::Neutral
        };
        let dep = inner.dep(&t.deployment, s, now);
        let before = dep.state();
        let mark = (effect == BreakerEffect::Ok && dep.breaker.state == BreakerState::Closed)
            .then(|| dep.breaker.mark(&s.breaker, now));
        inner
            .deployments
            .entry(t.deployment.clone())
            .or_default()
            .breaker
            .verdict(&s.breaker, now, effect, t.probe);
        let reason = if before == "half_open" {
            "probe"
        } else {
            "failures"
        };
        inner.changed(&t.deployment, before, reason, now);
        if v.counts {
            inner.apply(t, v, s, now);
        }
        if v.class == Class::Success
            && let Some(key) = &t.key
        {
            inner.answered.insert((t.provider.clone(), key.clone()), now);
        }
        release(&mut inner, t);
        drop(inner);
        self.give_back(t);
        (v.class == Class::Success).then(|| Committed {
            deployment: t.deployment.clone(),
            mark,
        })
    }

    /// Gives back the probe slots of an attempt that was never sent.
    pub fn abandon(&self, t: &Ticket) {
        let mut inner = self.inner.lock();
        if t.probe
            && let Some(dep) = inner.deployments.get_mut(&t.deployment)
        {
            dep.breaker.probes = dep.breaker.probes.saturating_sub(1);
        }
        release(&mut inner, t);
        drop(inner);
        self.give_back(t);
    }

    /// A committed stream failed (a reset, an idle timeout, a malformed event): a failure for its
    /// deployment's breaker, in place of the success recorded at the commit.
    pub fn failed_after_commit(&self, c: &Committed, s: &HealthSettings, now: UtcMicros) {
        let mut inner = self.inner.lock();
        let dep = inner.dep(&c.deployment, s, now);
        let before = dep.state();
        match c.mark {
            Some(m) => dep.breaker.failed_after_success(&s.breaker, now, m),
            None => dep.breaker.verdict(&s.breaker, now, BreakerEffect::Fail, false),
        }
        inner.changed(&c.deployment, before, "failures", now);
    }

    /// The verdict of an active health check of deployment `d`.
    pub fn checked(&self, d: &str, ok: bool, s: &HealthSettings, now: UtcMicros) {
        let mut inner = self.inner.lock();
        let dep = inner.dep(d, s, now);
        let before = dep.state();
        dep.breaker.checked(&s.breaker, now, ok);
        inner.changed(d, before, "health_check", now);
    }
}

impl Inner {
    /// The key, pair and cooldown effects of a verdict that counts.
    fn apply(&mut self, t: &Ticket, v: &Verdict, s: &HealthSettings, now: UtcMicros) {
        let clamp = |h: Duration| h.min(s.max_cooldown);
        let hinted = v.hint.map(|h| h.since(now));
        let d = t.deployment.as_str();
        match (v.class, &t.key) {
            (Class::Overloaded, _) => {
                if let Some(h) = hinted {
                    let until = after(after(now, clamp(h)), s.cooldown_buffer);
                    let dep = self.deployments.entry(d.to_owned()).or_default();
                    dep.cooldown_until = Some(dep.cooldown_until.map_or(until, |c| c.max(until)));
                    self.share_cooldown(d, now);
                }
            }
            (Class::RateLimited, Some(k)) => {
                let h = hinted.unwrap_or(s.rate_limit_cooldown);
                let until = after(after(now, clamp(h)), s.cooldown_buffer);
                let dep = self.deployments.entry(d.to_owned()).or_default();
                let pair = dep.pairs.entry(k.clone()).or_default();
                pair.cooldown_until = Some(pair.cooldown_until.map_or(until, |c| c.max(until)));
                self.share_pair(d, k, now);
            }
            (Class::CredentialRejected, Some(k)) if v.status == Some(403) => {
                let pair = self
                    .deployments
                    .entry(d.to_owned())
                    .or_default()
                    .pairs
                    .entry(k.clone())
                    .or_default();
                pair.dead = Some(dead(pair.dead.as_ref(), DeadReason::Permission, s, now));
                self.share_pair(d, k, now);
            }
            (Class::QuotaExhausted | Class::CredentialRejected, Some(k)) => {
                let reason = if v.class == Class::QuotaExhausted {
                    DeadReason::Quota
                } else {
                    DeadReason::Credential
                };
                let id = (t.provider.clone(), k.clone());
                let next = dead(self.keys.get(&id), reason, s, now);
                self.keys.insert(id, next);
                self.share_key(&t.provider, k, now);
            }
            (Class::Success, Some(k)) => {
                if self.keys.remove(&(t.provider.clone(), k.clone())).is_some() {
                    self.share_key(&t.provider, k, now);
                }
                let revived = self
                    .deployments
                    .get_mut(d)
                    .and_then(|dep| dep.pairs.get_mut(k))
                    .and_then(|p| p.dead.take())
                    .is_some();
                if revived {
                    self.share_pair(d, k, now);
                }
            }
            _ => {}
        }
    }
}

/// The dead state after one more failure of a key or pair: the re-check interval doubles with
/// each failed re-check.
fn dead(prev: Option<&Dead>, reason: DeadReason, s: &HealthSettings, now: UtcMicros) -> Dead {
    let k = prev.map_or(1, |d| d.k.saturating_add(1));
    Dead {
        reason,
        recheck_at: after(now, doubled(s.dead_key_recheck, k, s.max_dead_key_recheck)),
        k,
        probing: false,
    }
}

fn release(inner: &mut Inner, t: &Ticket) {
    if !t.key_probe {
        return;
    }
    let Some(k) = &t.key else { return };
    if let Some(d) = inner.keys.get_mut(&(t.provider.clone(), k.clone())) {
        d.probing = false;
    }
    if let Some(d) = inner
        .deployments
        .get_mut(&t.deployment)
        .and_then(|dep| dep.pairs.get_mut(k))
        .and_then(|p| p.dead.as_mut())
    {
        d.probing = false;
    }
}
