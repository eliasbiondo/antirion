//! Health state that lasts across requests: each deployment's circuit breaker, deployment and pair
//! cooldowns, dead keys and pairs, manual disables, and key selection over the eligible keys. All
//! times are the gateway clock.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use antirion_core::UtcMicros;
use parking_lot::Mutex;

use self::breaker::Breaker;
pub use self::breaker::BreakerState;
pub use self::shared::SharedRecord;
pub use self::verdict::Committed;
pub use self::view::{DeploymentView, KeyView};
use crate::budget::RetryBudget;
use crate::class::Class;
use crate::settings::HealthSettings;

mod breaker;
mod lease;
mod reconcile;
mod shared;
mod verdict;
mod view;

pub use self::reconcile::ConfigChange;

/// Why a key or pair is dead.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DeadReason {
    Credential,
    Quota,
    Permission,
}

impl DeadReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            DeadReason::Credential => "credential",
            DeadReason::Quota => "quota",
            DeadReason::Permission => "permission",
        }
    }
}

#[derive(Clone, Debug)]
struct Dead {
    reason: DeadReason,
    recheck_at: UtcMicros,
    /// 1 at the first mark, +1 per failed re-check.
    k: u32,
    probing: bool,
}

#[derive(Clone, Debug, Default)]
struct Pair {
    cooldown_until: Option<UtcMicros>,
    dead: Option<Dead>,
    /// When the pair's state last changed, here or on another node.
    stamp: i64,
}

/// When each part of a deployment's state last changed, here or on another node.
#[derive(Clone, Copy, Debug, Default)]
struct Stamps {
    breaker: i64,
    cooldown: i64,
    manual: i64,
}

#[derive(Clone, Debug, Default)]
struct Deployment {
    breaker: Breaker,
    cooldown_until: Option<UtcMicros>,
    /// A manual disable, with its end when it has one.
    disabled: Option<Option<UtcMicros>>,
    pairs: HashMap<String, Pair>,
    /// Smooth weighted round-robin state of the deployment's keys.
    key_rr: HashMap<String, i64>,
    stamps: Stamps,
}

#[derive(Debug, Default)]
struct Inner {
    deployments: HashMap<String, Deployment>,
    /// Provider-scope key state, by (provider, key).
    keys: HashMap<(String, String), Dead>,
    /// When each provider-scope key state last changed, by (provider, key).
    key_stamps: HashMap<(String, String), i64>,
    /// When each key last had an attempt the upstream answered with success, by (provider, key):
    /// the evidence that it works.
    answered: HashMap<(String, String), UtcMicros>,
    /// State changes not yet taken, oldest first.
    changes: Vec<StateChange>,
    /// Changes for the other nodes of a cluster, by record field; `None` outside a cluster.
    outbox: Option<BTreeMap<String, SharedRecord>>,
}

/// The most state changes kept until they are taken; older ones are dropped.
const MAX_CHANGES: usize = 1024;

/// A deployment moved between `closed`, `open`, `half_open` and `disabled`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateChange {
    pub deployment: String,
    pub from: &'static str,
    pub to: &'static str,
    pub reason: &'static str,
}

impl Inner {
    /// Notes a change of `d`'s state from `before`, if there was one; a trip, and a close of an
    /// open or half-open breaker, are shared with the cluster.
    fn changed(&mut self, d: &str, before: &'static str, reason: &'static str, now: UtcMicros) {
        let after = self.deployments.get(d).map_or("closed", Deployment::state);
        if after == before {
            return;
        }
        if after == "open" || (after == "closed" && matches!(before, "open" | "half_open")) {
            self.share_breaker(d, now);
        }
        if self.changes.len() >= MAX_CHANGES {
            self.changes.remove(0);
        }
        self.changes.push(StateChange {
            deployment: d.to_owned(),
            from: before,
            to: after,
            reason,
        });
    }

    fn state_of(&self, d: &str) -> &'static str {
        self.deployments.get(d).map_or("closed", Deployment::state)
    }
}

/// One credential of a provider, for selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyInfo {
    pub name: String,
    pub weight: u32,
}

/// Why a deployment is not eligible now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unhealthy {
    /// The earliest known recovery; `None` when unknown (a manual disable without an end).
    pub recover_at: Option<UtcMicros>,
    /// The only obstacle is rate-limit cooldowns of its pairs.
    pub cooling_only: bool,
    /// Manually disabled.
    pub disabled: bool,
    /// The only obstacle is the breaker or a deployment cooldown: it has a usable key.
    pub breaker_only: bool,
}

/// How to choose a key.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum KeyMode {
    /// Any eligible key not yet tried on this deployment.
    Untried,
    /// An untried eligible key, else `last` (the retry of the same deployment).
    PreferUntried,
}

/// A held attempt: the probe slots it took, released by [`HealthRegistry::finish`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ticket {
    pub deployment: String,
    pub provider: String,
    pub key: Option<String>,
    probe: bool,
    key_probe: bool,
    /// The cluster-wide lease the probe holds.
    lease: Option<u64>,
}

/// The verdict of one attempt, for the health state.
#[derive(Copy, Clone, Debug)]
pub struct Verdict {
    pub class: Class,
    pub status: Option<u16>,
    /// The upstream hint, on the gateway clock.
    pub hint: Option<UtcMicros>,
    /// False for a timeout shortened by the client or the deadline: it counts for nothing.
    pub counts: bool,
}

/// The health state of every deployment and key of this node.
#[derive(Debug, Default)]
pub struct HealthRegistry {
    inner: Mutex<Inner>,
    /// The cross-request retry budgets.
    pub budget: RetryBudget,
    /// The cluster's probe leases, in cluster mode.
    leases: std::sync::OnceLock<std::sync::Arc<dyn crate::lease::ProbeLeases>>,
}

fn key_alive(dead: Option<&Dead>, now: UtcMicros) -> bool {
    dead.is_none_or(|d| now >= d.recheck_at && !d.probing)
}

impl Deployment {
    /// The state the Admin API and state-change events name.
    fn state(&self) -> &'static str {
        if self.disabled.is_some() {
            "disabled"
        } else {
            self.breaker.state.as_str()
        }
    }

    fn refresh(&mut self, s: &HealthSettings, now: UtcMicros) {
        self.breaker.refresh(&s.breaker, now);
        if let Some(Some(until)) = self.disabled
            && now >= until
        {
            self.disabled = None;
        }
    }
}

impl Inner {
    fn dep(&mut self, d: &str, s: &HealthSettings, now: UtcMicros) -> &mut Deployment {
        let before = self.state_of(d);
        self.deployments.entry(d.to_owned()).or_default().refresh(s, now);
        let reason = if before == "disabled" {
            "disable_expired"
        } else {
            "open_duration_elapsed"
        };
        self.changed(d, before, reason, now);
        self.deployments.entry(d.to_owned()).or_default()
    }

    /// Whether a key may be used on deployment `d` now, and when it recovers if not.
    fn key_status(&self, provider: &str, d: Option<&Deployment>, key: &str, now: UtcMicros) -> KeyStatus {
        let scope = self.keys.get(&(provider.to_owned(), key.to_owned()));
        let pair = d.and_then(|d| d.pairs.get(key));
        let pair_dead = pair.and_then(|p| p.dead.as_ref());
        let cooling = pair.and_then(|p| p.cooldown_until).filter(|u| *u > now);
        if key_alive(scope, now) && key_alive(pair_dead, now) && cooling.is_none() {
            return KeyStatus::Usable;
        }
        let dead_until = [scope, pair_dead]
            .into_iter()
            .flatten()
            .map(|d| d.recheck_at)
            .max();
        let recover = match (dead_until, cooling) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        KeyStatus::Blocked {
            recover,
            cooling_only: cooling.is_some() && key_alive(scope, now) && key_alive(pair_dead, now),
        }
    }
}

impl Inner {
    /// Whether some key of `d` may be used now; else its earliest recovery and whether only pair
    /// rate-limit cooldowns block it. A keyless deployment always may.
    fn keys_state(
        &self,
        d: &str,
        provider: &str,
        keys: &[KeyInfo],
        now: UtcMicros,
    ) -> Result<(), (Option<UtcMicros>, bool)> {
        if keys.is_empty() {
            return Ok(());
        }
        let dep = self.deployments.get(d);
        let mut recover: Option<UtcMicros> = None;
        let mut cooling_only = true;
        for k in keys.iter().filter(|k| k.weight > 0) {
            match self.key_status(provider, dep, &k.name, now) {
                KeyStatus::Usable => return Ok(()),
                KeyStatus::Blocked {
                    recover: r,
                    cooling_only: c,
                } => {
                    recover = match (recover, r) {
                        (Some(a), Some(b)) => Some(a.min(b)),
                        (a, b) => a.or(b),
                    };
                    cooling_only &= c;
                }
            }
        }
        Err((recover, cooling_only))
    }
}

enum KeyStatus {
    Usable,
    Blocked {
        recover: Option<UtcMicros>,
        cooling_only: bool,
    },
}

impl HealthRegistry {
    /// Whether deployment `d` (of `provider`, with `keys`; empty for a keyless provider) is eligible.
    pub fn status(
        &self,
        d: &str,
        provider: &str,
        keys: &[KeyInfo],
        s: &HealthSettings,
        now: UtcMicros,
    ) -> Result<(), Unhealthy> {
        let mut inner = self.inner.lock();
        let dep = inner.dep(d, s, now);
        if let Some(until) = dep.disabled {
            return Err(Unhealthy {
                recover_at: until,
                cooling_only: false,
                disabled: true,
                breaker_only: false,
            });
        }
        let breaker = match dep.breaker.state {
            _ if !s.breaker.enabled => None,
            BreakerState::Open => Some(dep.breaker.open_until),
            BreakerState::HalfOpen if dep.breaker.probes >= s.breaker.half_open_probes => {
                Some(Some(now.saturating_add(Duration::from_secs(1))))
            }
            _ => None,
        };
        let cooldown = dep.cooldown_until.filter(|u| *u > now);
        let keys = inner.keys_state(d, provider, keys, now);
        if breaker.is_none() && cooldown.is_none() {
            return keys.map_err(|(recover_at, cooling_only)| Unhealthy {
                recover_at,
                cooling_only,
                disabled: false,
                breaker_only: false,
            });
        }
        Err(Unhealthy {
            recover_at: breaker.unwrap_or(cooldown),
            cooling_only: false,
            disabled: false,
            breaker_only: keys.is_ok(),
        })
    }

    /// Chooses a key of deployment `d` by smooth weighted round-robin over its eligible keys.
    /// `tried` are the keys already used on `d` in this request; `last` is the key of the attempt
    /// that just failed. `None` when no key qualifies.
    #[allow(clippy::too_many_arguments)]
    pub fn select_key(
        &self,
        d: &str,
        provider: &str,
        keys: &[KeyInfo],
        tried: &[String],
        last: Option<&str>,
        mode: KeyMode,
        s: &HealthSettings,
        now: UtcMicros,
    ) -> Option<String> {
        let mut inner = self.inner.lock();
        inner.dep(d, s, now);
        let dep = inner.deployments.get(d);
        let usable = |k: &&KeyInfo| {
            !tried.contains(&k.name)
                && matches!(inner.key_status(provider, dep, &k.name, now), KeyStatus::Usable)
        };
        let eligible: Vec<&KeyInfo> = keys.iter().filter(|k| k.weight > 0).filter(usable).collect();
        if eligible.is_empty() {
            // A key of weight 0 is a standby: used, in order, only when no weighted key can be.
            if let Some(standby) = keys.iter().filter(|k| k.weight == 0).find(usable) {
                return Some(standby.name.clone());
            }
            return match mode {
                KeyMode::PreferUntried => last.map(str::to_owned),
                KeyMode::Untried => None,
            };
        }
        let total: i64 = eligible.iter().map(|k| i64::from(k.weight)).sum();
        let rr = &mut inner.deployments.entry(d.to_owned()).or_default().key_rr;
        let mut best: Option<(&str, i64)> = None;
        for k in &eligible {
            let slot = rr.entry(k.name.clone()).or_insert(0);
            *slot += i64::from(k.weight);
            if best.is_none_or(|(_, b)| *slot > b) {
                best = Some((&k.name, *slot));
            }
        }
        let (chosen, _) = best?;
        if let Some(slot) = rr.get_mut(chosen) {
            *slot -= total;
        }
        Some(chosen.to_owned())
    }

    /// Takes the probe slots an attempt needs: a half-open breaker's, and a dead key's re-check.
    /// `None` when a slot is taken by another request, so the candidate must be skipped.
    pub fn begin(
        &self,
        d: &str,
        provider: &str,
        key: Option<&str>,
        s: &HealthSettings,
        now: UtcMicros,
    ) -> Option<Ticket> {
        self.take(d, provider, key, s, now, false)
    }

    /// [`HealthRegistry::begin`] for a request that runs as if every breaker were closed (every
    /// candidate was unhealthy and the configuration says to ignore health): no probe slot is taken.
    pub fn begin_ignoring_breaker(
        &self,
        d: &str,
        provider: &str,
        key: Option<&str>,
        s: &HealthSettings,
        now: UtcMicros,
    ) -> Option<Ticket> {
        self.take(d, provider, key, s, now, true)
    }

    fn take(
        &self,
        d: &str,
        provider: &str,
        key: Option<&str>,
        s: &HealthSettings,
        now: UtcMicros,
        ignore_breaker: bool,
    ) -> Option<Ticket> {
        let mut inner = self.inner.lock();
        let mut ticket = Ticket {
            deployment: d.to_owned(),
            provider: provider.to_owned(),
            key: key.map(str::to_owned),
            probe: false,
            key_probe: false,
            lease: None,
        };
        let dep = inner.dep(d, s, now);
        if !ignore_breaker && s.breaker.enabled && dep.breaker.state == BreakerState::HalfOpen {
            if dep.breaker.probes >= s.breaker.half_open_probes {
                return None;
            }
            ticket.probe = true;
        }
        if let Some(k) = key {
            let pair_dead = dep.pairs.get_mut(k).and_then(|p| p.dead.as_mut());
            let pair_probe = match pair_dead {
                Some(dead) if dead.probing || now < dead.recheck_at => return None,
                Some(dead) => {
                    dead.probing = true;
                    true
                }
                None => false,
            };
            match inner.keys.get_mut(&(provider.to_owned(), k.to_owned())) {
                Some(dead) if dead.probing || now < dead.recheck_at => {
                    if pair_probe
                        && let Some(dead) = inner
                            .deployments
                            .get_mut(d)
                            .and_then(|dep| dep.pairs.get_mut(k))
                            .and_then(|p| p.dead.as_mut())
                    {
                        dead.probing = false;
                    }
                    return None;
                }
                Some(dead) => {
                    dead.probing = true;
                    ticket.key_probe = true;
                }
                None => ticket.key_probe = pair_probe,
            }
        }
        if ticket.probe
            && let Some(dep) = inner.deployments.get_mut(d)
        {
            dep.breaker.probes += 1;
        }
        Some(ticket)
    }

    /// When the deployment's breaker or cooldown lets traffic through again, if it is blocked.
    pub fn recovery(&self, d: &str, now: UtcMicros) -> Option<UtcMicros> {
        let inner = self.inner.lock();
        let dep = inner.deployments.get(d)?;
        let open = dep
            .breaker
            .open_until
            .filter(|u| dep.breaker.state == BreakerState::Open && *u > now);
        [open, dep.cooldown_until.filter(|u| *u > now)]
            .into_iter()
            .flatten()
            .max()
    }

    /// Whether the deployment is open or manually disabled now (a retry of it is not permitted);
    /// with `ignore_breaker`, only a manual disable blocks.
    pub fn blocked(&self, d: &str, s: &HealthSettings, now: UtcMicros, ignore_breaker: bool) -> bool {
        let mut inner = self.inner.lock();
        let dep = inner.dep(d, s, now);
        dep.disabled.is_some()
            || (!ignore_breaker && s.breaker.enabled && dep.breaker.state == BreakerState::Open)
    }

    /// The state of `d` now: `closed`, `open`, `half_open` or `disabled`.
    pub fn state(&self, d: &str, s: &HealthSettings, now: UtcMicros) -> &'static str {
        self.inner.lock().dep(d, s, now).state()
    }

    /// The first of `keys`, in their order, that may be used on `d` now.
    pub fn usable_key(&self, d: &str, provider: &str, keys: &[KeyInfo], now: UtcMicros) -> Option<String> {
        let inner = self.inner.lock();
        let dep = inner.deployments.get(d);
        keys.iter()
            .find(|k| matches!(inner.key_status(provider, dep, &k.name, now), KeyStatus::Usable))
            .map(|k| k.name.clone())
    }

    /// The latest cooldown that applies to `d` and its pair with `key`.
    pub fn cooldown(&self, d: &str, key: Option<&str>, now: UtcMicros) -> Option<UtcMicros> {
        let inner = self.inner.lock();
        let dep = inner.deployments.get(d)?;
        let pair = key.and_then(|k| dep.pairs.get(k)).and_then(|p| p.cooldown_until);
        [dep.cooldown_until, pair]
            .into_iter()
            .flatten()
            .filter(|u| *u > now)
            .max()
    }

    /// A manual disable, until `until` or indefinitely; never cleared by anything but `enable`
    /// or its end.
    pub fn disable(&self, d: &str, until: Option<UtcMicros>, now: UtcMicros) {
        let mut inner = self.inner.lock();
        let before = inner.state_of(d);
        inner.deployments.entry(d.to_owned()).or_default().disabled = Some(until);
        inner.share_manual(d, now);
        inner.changed(d, before, "manual", now);
    }

    /// A manual disable read back from the store at startup: older than anything shared since.
    pub fn restore_disable(&self, d: &str, until: Option<UtcMicros>) {
        let mut inner = self.inner.lock();
        inner.deployments.entry(d.to_owned()).or_default().disabled = Some(until);
    }

    pub fn enable(&self, d: &str, now: UtcMicros) {
        let mut inner = self.inner.lock();
        let before = inner.state_of(d);
        if let Some(dep) = inner.deployments.get_mut(d) {
            dep.disabled = None;
        }
        inner.share_manual(d, now);
        inner.changed(d, before, "manual", now);
    }

    /// The state changes since the last call, oldest first.
    pub fn take_changes(&self) -> Vec<StateChange> {
        std::mem::take(&mut self.inner.lock().changes)
    }

    /// Closes the breaker, clears its counters and cooldowns, and revives its pairs and keys.
    pub fn reset(&self, d: &str, provider: &str, now: UtcMicros) {
        let mut inner = self.inner.lock();
        let before = inner.state_of(d);
        let pairs = reset_deployment(&mut inner, d, provider, now);
        inner.share_breaker(d, now);
        inner.share_cooldown(d, now);
        for k in &pairs {
            inner.share_pair(d, k, now);
            inner.share_key(provider, k, now);
        }
        inner.changed(d, before, "manual", now);
    }

    /// Revives a key at provider scope and every pair it has.
    pub fn reset_key(&self, provider: &str, key: &str, deployments: &[&str], now: UtcMicros) {
        let mut inner = self.inner.lock();
        inner.keys.remove(&(provider.to_owned(), key.to_owned()));
        inner.share_key(provider, key, now);
        for d in deployments {
            if let Some(p) = inner
                .deployments
                .get_mut(*d)
                .and_then(|dep| dep.pairs.get_mut(key))
            {
                *p = Pair::default();
                inner.share_pair(d, key, now);
            }
        }
    }

    /// The manual disables, for persistence: (deployment, end).
    pub fn disables(&self) -> Vec<(String, Option<UtcMicros>)> {
        let inner = self.inner.lock();
        let mut out: Vec<(String, Option<UtcMicros>)> = inner
            .deployments
            .iter()
            .filter_map(|(n, d)| d.disabled.map(|u| (n.clone(), u)))
            .collect();
        out.sort();
        out
    }

    /// Forgets every state, manual disables included (test-mode reset).
    pub fn clear(&self) {
        let mut inner = self.inner.lock();
        inner.keys.clear();
        inner.key_stamps.clear();
        inner.deployments.clear();
        self.budget.clear();
    }
}

/// Closes a deployment's breaker and clears its counters, cooldowns, pairs and keys; a manual disable
/// stays. Returns the keys whose pairs were cleared.
fn reset_deployment(inner: &mut Inner, d: &str, provider: &str, now: UtcMicros) -> Vec<String> {
    let dep = inner.deployments.entry(d.to_owned()).or_default();
    let disabled = dep.disabled;
    let manual = dep.stamps.manual;
    let names: Vec<String> = dep.pairs.keys().cloned().collect();
    *dep = Deployment {
        disabled,
        ..Deployment::default()
    };
    dep.stamps.manual = manual;
    dep.breaker.close(now);
    dep.breaker.trips = 0;
    for k in &names {
        inner.keys.remove(&(provider.to_owned(), k.clone()));
    }
    names
}

#[cfg(test)]
mod tests;
