//! Selection state of this replica that outlives requests and config generations: smooth weighted
//! round-robin counters by node, and per-deployment latency (PeakEWMA per slot), success EWMA and
//! in-flight attempts, shared by every route that uses the deployment.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use antirion_core::UtcMicros;
use parking_lot::Mutex;

use crate::content::{CostQualitySpec, EmbeddingCache, Knob, Window};
use crate::lru::Lru;
use crate::pool::PoolState;

/// Replayed classifier decisions kept per replica.
const REPLAYS: usize = 100_000;

/// The latency slot of a request.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Slot {
    /// Streaming generation: time to the first content event.
    Ttft,
    /// Non-streaming generation: time per output token.
    PerToken,
    /// Non-generative endpoints: time to the complete response.
    E2e,
}

impl Slot {
    const ALL: [Slot; 3] = [Slot::Ttft, Slot::PerToken, Slot::E2e];

    fn index(self) -> usize {
        match self {
            Slot::Ttft => 0,
            Slot::PerToken => 1,
            Slot::E2e => 2,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Slot::Ttft => "ttft",
            Slot::PerToken => "per_token",
            Slot::E2e => "e2e",
        }
    }

    /// The default estimate of a slot with no samples, in milliseconds.
    #[must_use]
    pub fn default_ms(self) -> f64 {
        match self {
            Slot::Ttft | Slot::E2e => 500.0,
            Slot::PerToken => 50.0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Ewma {
    value: f64,
    at: Instant,
}

impl Ewma {
    fn decayed(&self, now: Instant, tau: Duration) -> f64 {
        let dt = now.saturating_duration_since(self.at).as_secs_f64();
        let w = (-dt / tau.as_secs_f64().max(1e-3)).exp();
        self.value * w
    }
}

#[derive(Clone, Debug, Default)]
struct DeploymentStats {
    latency: [Option<Ewma>; 3],
    success: Option<Ewma>,
    inflight: u32,
}

const DEFAULT_TAU: Duration = Duration::from_secs(10);

/// One deployment's statistics, as the routing state view shows them.
#[derive(Clone, Debug, PartialEq)]
pub struct StatsView {
    /// Estimates in milliseconds by slot, `None` without samples.
    pub latency_ms: [(Slot, Option<f64>); 3],
    pub success: f64,
    pub inflight: u32,
}

/// The selection state of this replica.
#[derive(Debug, Default)]
pub struct RoutingState {
    /// SWRR counters by node state key, by child position (`None` until the child is counted).
    swrr: Mutex<HashMap<String, Vec<Option<i64>>>>,
    stats: Mutex<HashMap<String, DeploymentStats>>,
    /// Mirrored copies in flight, by the mirroring node's state key.
    mirrors: Mutex<HashMap<String, u32>>,
    /// `least_latency` nodes whose children's estimates were started, by state key and slot.
    primed: Mutex<HashMap<String, [bool; 3]>>,
    /// Pool metrics, assumed load and prefix indexes.
    pools: PoolState,
    /// The score windows of `strong_fraction` nodes, by state key.
    windows: Mutex<HashMap<String, Window>>,
    /// A classifier's last class per session, for continuation turns, with its expiry.
    replays: Mutex<Option<Lru<String, (usize, UtcMicros)>>>,
    /// Query and example embeddings of embedding classifiers.
    embeddings: EmbeddingCache,
    /// Canary ramps and error budgets.
    pub canaries: crate::canary::Canaries,
    /// Shadow comparisons by mirroring node.
    pub shadows: crate::shadow::Shadows,
    /// What the external endpoint pickers answered, by pool node.
    pub pickers: crate::pickers::Pickers,
    /// Attempts in flight on the other nodes of the cluster, by deployment, and when they were read.
    remote: Mutex<Option<(HashMap<String, u32>, Instant)>>,
    /// What bandit experiments learnt, and their stops.
    pub bandits: crate::bandit::BanditState,
}

/// How long the other nodes' counts stand for cluster-scope `least_busy`.
const REMOTE_FRESH: Duration = Duration::from_secs(2);

/// A mirrored copy in flight; dropping it ends the count.
#[derive(Debug)]
pub struct MirrorSlot {
    state: Arc<RoutingState>,
    node: String,
}

impl Drop for MirrorSlot {
    fn drop(&mut self) {
        let mut m = self.state.mirrors.lock();
        if let Some(n) = m.get_mut(&self.node) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                m.remove(&self.node);
            }
        }
    }
}

/// An attempt in flight on a deployment; dropping it ends the count.
#[derive(Debug)]
pub struct InFlight {
    state: Arc<RoutingState>,
    deployment: String,
    /// The pool it was dispatched through, and its serial there.
    pool: Option<(String, u64)>,
}

impl Drop for InFlight {
    fn drop(&mut self) {
        {
            let mut stats = self.state.stats.lock();
            if let Some(s) = stats.get_mut(&self.deployment) {
                s.inflight = s.inflight.saturating_sub(1);
            }
        }
        if let Some((pool, serial)) = &self.pool {
            self.state.pools.finished(pool, &self.deployment, *serial);
        }
    }
}

impl RoutingState {
    /// One smooth weighted round-robin pick among the `(child position, weight)` candidates of the
    /// node `key` with `children` children; returns the index of the chosen candidate. Children
    /// not listed keep their counters. The key names the node's children and weights, so positions
    /// are stable under it.
    pub fn swrr(&self, key: &str, children: usize, candidates: &[(usize, u32)]) -> Option<usize> {
        let mut all = self.swrr.lock();
        let state = match all.get_mut(key) {
            Some(s) => s,
            None => all.entry(key.to_owned()).or_default(),
        };
        if state.len() < children {
            state.resize(children, None);
        }
        let total: i64 = candidates.iter().map(|(_, w)| i64::from(*w)).sum();
        let mut best: Option<(usize, i64)> = None;
        for (i, (pos, w)) in candidates.iter().enumerate() {
            let Some(slot) = state.get_mut(*pos) else { continue };
            let c = slot.get_or_insert(0);
            *c += i64::from(*w);
            if best.is_none_or(|(_, b)| *c > b) {
                best = Some((i, *c));
            }
        }
        let (chosen, _) = best?;
        if let Some((pos, _)) = candidates.get(chosen)
            && let Some(Some(c)) = state.get_mut(*pos)
        {
            *c -= total;
        }
        Some(chosen)
    }

    /// The child [`swrr`](Self::swrr) would choose now, without changing the counters.
    #[must_use]
    pub fn swrr_peek(&self, key: &str, candidates: &[(usize, u32)]) -> Option<usize> {
        let all = self.swrr.lock();
        let state = all.get(key);
        let mut best: Option<(usize, i64)> = None;
        for (i, (pos, w)) in candidates.iter().enumerate() {
            let current = state.and_then(|s| s.get(*pos)).copied().flatten().unwrap_or(0);
            let c = current + i64::from(*w);
            if best.is_none_or(|(_, b)| c > b) {
                best = Some((i, c));
            }
        }
        best.map(|(i, _)| i)
    }

    /// This node's attempts in flight, by deployment, for the other nodes to read.
    #[must_use]
    pub fn local_inflight(&self) -> HashMap<String, u32> {
        self.stats
            .lock()
            .iter()
            .filter(|(_, s)| s.inflight > 0)
            .map(|(d, s)| (d.clone(), s.inflight))
            .collect()
    }

    /// Keeps the other nodes' attempts in flight, summed by deployment.
    pub fn set_remote_inflight(&self, counts: HashMap<String, u32>, now: Instant) {
        *self.remote.lock() = Some((counts, now));
    }

    /// The other nodes' attempts in flight on a deployment; `None` when their counts are not
    /// fresh (the counter store is unreachable).
    #[must_use]
    pub fn remote_inflight(&self, deployment: &str, now: Instant) -> Option<u32> {
        let remote = self.remote.lock();
        let (counts, at) = remote.as_ref()?;
        (now.saturating_duration_since(*at) <= REMOTE_FRESH)
            .then(|| counts.get(deployment).copied().unwrap_or(0))
    }

    /// The SWRR counters of a node, by child position (`None` for a child never counted).
    #[must_use]
    pub fn swrr_state(&self, key: &str) -> Vec<Option<i64>> {
        self.swrr.lock().get(key).cloned().unwrap_or_default()
    }

    /// Clears every counter and statistic.
    pub fn reset(&self) {
        self.swrr.lock().clear();
        self.primed.lock().clear();
        self.pools.reset();
        self.windows.lock().clear();
        *self.replays.lock() = None;
        self.embeddings.clear();
        self.canaries.reset();
        self.shadows.reset();
        self.pickers.reset();
        self.bandits.clear();
        self.stats.lock().retain(|_, s| {
            s.latency = [None; 3];
            s.success = None;
            s.inflight > 0
        });
    }

    /// Counts a mirrored copy of `node` in flight until the slot is dropped; `None` when `max` are
    /// already in flight.
    #[must_use]
    pub fn begin_mirror(self: &Arc<Self>, node: &str, max: u32) -> Option<MirrorSlot> {
        let mut m = self.mirrors.lock();
        let n = m.entry(node.to_owned()).or_default();
        if *n >= max {
            return None;
        }
        *n += 1;
        Some(MirrorSlot {
            state: Arc::clone(self),
            node: node.to_owned(),
        })
    }

    /// Counts an attempt in flight until the guard is dropped.
    #[must_use]
    pub fn begin(self: &Arc<Self>, deployment: &str) -> InFlight {
        self.stats
            .lock()
            .entry(deployment.to_owned())
            .or_default()
            .inflight += 1;
        InFlight {
            state: Arc::clone(self),
            deployment: deployment.to_owned(),
            pool: None,
        }
    }

    /// Counts an attempt dispatched to an endpoint of a pool: in flight, and assumed queued on the
    /// endpoint until its next reading, until the guard is dropped.
    #[must_use]
    pub fn begin_in_pool(self: &Arc<Self>, deployment: &str, d: &crate::plan::PoolDispatch) -> InFlight {
        let mut g = self.begin(deployment);
        let serial = self.pools.dispatched(&d.pool, deployment, &d.hashes, d.entries);
        g.pool = Some((d.pool.clone(), serial));
        g
    }

    /// Pool metrics, assumed load and prefix indexes of this replica.
    #[must_use]
    pub fn pools(&self) -> &PoolState {
        &self.pools
    }

    /// The threshold of cost-quality node `key` for this request; a valid score `w` then joins the
    /// node's window.
    pub fn threshold(&self, key: &str, spec: &CostQualitySpec, w: Option<f64>) -> f64 {
        match spec.knob {
            Knob::Threshold(t) => t,
            Knob::Fraction(p) => {
                let mut all = self.windows.lock();
                let win = match all.get_mut(key) {
                    Some(w) => w,
                    None => all.entry(key.to_owned()).or_default(),
                };
                let tau = win.threshold(p, &spec.calibration);
                if let Some(w) = w {
                    win.observe(w, spec.calibration.window);
                }
                tau
            }
        }
    }

    /// Query and example embeddings of embedding classifiers.
    #[must_use]
    pub fn embeddings(&self) -> &EmbeddingCache {
        &self.embeddings
    }

    /// The class a classifier chose for a session's earlier request, while it lives.
    #[must_use]
    pub fn replayed(&self, key: &str, now: UtcMicros) -> Option<usize> {
        let mut r = self.replays.lock();
        let lru = r.as_mut()?;
        lru.get(&key.to_owned())
            .filter(|(_, until)| *until > now)
            .map(|(c, _)| *c)
    }

    /// Remembers the class a classifier chose for a session, until `until`.
    pub fn remember(&self, key: String, class: usize, until: UtcMicros) {
        self.replays
            .lock()
            .get_or_insert_with(|| Lru::new(REPLAYS))
            .put(key, (class, until));
    }

    #[must_use]
    pub fn inflight(&self, deployment: &str) -> u32 {
        self.stats.lock().get(deployment).map_or(0, |s| s.inflight)
    }

    /// Runs `f` with a lookup of in-flight attempts by deployment, under one lock.
    pub fn with_inflight<T>(&self, f: impl FnOnce(&dyn Fn(&str) -> u32) -> T) -> T {
        let stats = self.stats.lock();
        f(&|d: &str| stats.get(d).map_or(0, |s| s.inflight))
    }

    /// The decayed latency estimate of a slot, in milliseconds. A deployment without samples
    /// starts from `default_ms`, which decays like any estimate, so a cold or long-unused
    /// deployment is eventually tried again.
    #[must_use]
    pub fn estimate(
        &self,
        deployment: &str,
        slot: Slot,
        tau: Duration,
        default_ms: f64,
        now: Instant,
    ) -> f64 {
        let mut stats = self.stats.lock();
        let s = stats.entry(deployment.to_owned()).or_default();
        match s.latency.get_mut(slot.index()) {
            Some(Some(e)) => e.decayed(now, tau),
            Some(empty) => {
                *empty = Some(Ewma {
                    value: default_ms,
                    at: now,
                });
                default_ms
            }
            None => default_ms,
        }
    }

    /// Starts the estimate of every deployment in `deployments` without one at `default_ms`, once per
    /// node `key` and slot: the cold children of a node then decay from the same instant, so none
    /// stays behind the others only because it was looked at later.
    pub fn prime<'a>(
        &self,
        key: &str,
        slot: Slot,
        default_ms: f64,
        now: Instant,
        deployments: impl Iterator<Item = &'a str>,
    ) {
        {
            let mut stats = self.stats.lock();
            for d in deployments {
                let s = stats.entry(d.to_owned()).or_default();
                if let Some(empty @ None) = s.latency.get_mut(slot.index()) {
                    *empty = Some(Ewma {
                        value: default_ms,
                        at: now,
                    });
                }
            }
        }
        let mut primed = self.primed.lock();
        let p = match primed.get_mut(key) {
            Some(p) => p,
            None => primed.entry(key.to_owned()).or_default(),
        };
        if let Some(flag) = p.get_mut(slot.index()) {
            *flag = true;
        }
    }

    /// Whether [`RoutingState::prime`] ran for node `key` and `slot`.
    #[must_use]
    pub fn primed(&self, key: &str, slot: Slot) -> bool {
        self.primed
            .lock()
            .get(key)
            .is_some_and(|p| p.get(slot.index()) == Some(&true))
    }

    /// The decayed success rate, 1 before any sample.
    #[must_use]
    pub fn success(&self, deployment: &str) -> f64 {
        let stats = self.stats.lock();
        stats
            .get(deployment)
            .and_then(|s| s.success)
            .map_or(1.0, |e| e.value)
            .clamp(0.0, 1.0)
    }

    /// Records a latency sample in milliseconds (PeakEWMA): a higher value replaces the estimate,
    /// a lower one is averaged in. Samples that are not finite and non-negative are dropped.
    pub fn observe(&self, deployment: &str, slot: Slot, ms: f64, tau: Duration, now: Instant) {
        if !ms.is_finite() || ms < 0.0 {
            return;
        }
        let mut stats = self.stats.lock();
        let s = stats.entry(deployment.to_owned()).or_default();
        let next = match s.latency.get(slot.index()).copied().flatten() {
            None => ms,
            Some(e) if ms > e.value => ms,
            Some(e) => {
                let dt = now.saturating_duration_since(e.at).as_secs_f64();
                let w = (-dt / tau.as_secs_f64().max(1e-3)).exp();
                e.value * w + ms * (1.0 - w)
            }
        };
        if let Some(slot) = s.latency.get_mut(slot.index()) {
            *slot = Some(Ewma { value: next, at: now });
        }
    }

    /// Records a success (1) or a deployment-attributable failure (0).
    pub fn outcome(&self, deployment: &str, ok: bool, now: Instant) {
        let mut stats = self.stats.lock();
        let s = stats.entry(deployment.to_owned()).or_default();
        let x = if ok { 1.0 } else { 0.0 };
        let (prev, dt) = match s.success {
            None => (1.0, 0.0),
            Some(e) => (e.value, now.saturating_duration_since(e.at).as_secs_f64()),
        };
        let w = (-dt / DEFAULT_TAU.as_secs_f64()).exp().min(0.9);
        let next = prev * w + x * (1.0 - w);
        s.success = Some(Ewma { value: next, at: now });
    }

    /// A deployment's statistics for the state view.
    #[must_use]
    pub fn view(&self, deployment: &str, tau: Duration, now: Instant) -> StatsView {
        let stats = self.stats.lock();
        let s = stats.get(deployment);
        let lat = |slot: Slot| {
            s.and_then(|s| s.latency.get(slot.index()).copied().flatten())
                .map(|e| e.decayed(now, tau))
        };
        StatsView {
            latency_ms: Slot::ALL.map(|slot| (slot, lat(slot))),
            success: s.and_then(|s| s.success).map_or(1.0, |e| e.value).clamp(0.0, 1.0),
            inflight: s.map_or(0, |s| s.inflight),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirrors_in_flight_are_bounded_per_node() {
        let s = Arc::new(RoutingState::default());
        let a = s.begin_mirror("n", 2);
        let b = s.begin_mirror("n", 2);
        assert!(a.is_some() && b.is_some());
        assert!(s.begin_mirror("n", 2).is_none());
        assert!(s.begin_mirror("other", 2).is_some());
        drop(a);
        assert!(s.begin_mirror("n", 2).is_some());
    }

    #[test]
    fn swrr_is_exact_and_smooth() {
        let s = RoutingState::default();
        let c = vec![(0, 5), (1, 1), (2, 1)];
        let seq: String = (0..7)
            .filter_map(|_| s.swrr("n", 3, &c))
            .map(|i| ["a", "b", "c"][i])
            .collect();
        assert_eq!(seq, "aabacaa");
        assert_eq!(s.swrr_state("n"), vec![Some(0), Some(0), Some(0)]);
        assert_eq!(s.swrr("n", 3, &[(2, 1)]), Some(0), "a lone candidate");
        assert_eq!(s.swrr_state("n"), vec![Some(0), Some(0), Some(0)]);
    }

    #[test]
    fn peak_ewma_and_inflight() {
        let s = Arc::new(RoutingState::default());
        let t0 = Instant::now();
        s.observe("d", Slot::E2e, 200.0, DEFAULT_TAU, t0);
        s.observe("d", Slot::E2e, f64::INFINITY, DEFAULT_TAU, t0);
        s.observe("d", Slot::E2e, -1.0, DEFAULT_TAU, t0);
        assert!((s.estimate("d", Slot::E2e, DEFAULT_TAU, 500.0, t0) - 200.0).abs() < 1e-6);
        assert!(s.estimate("d", Slot::E2e, DEFAULT_TAU, 500.0, t0 + Duration::from_secs(60)) < 1.0);
        assert_eq!(s.estimate("x", Slot::Ttft, DEFAULT_TAU, 500.0, t0), 500.0);
        let g = s.begin("d");
        assert_eq!(s.inflight("d"), 1);
        drop(g);
        assert_eq!(s.inflight("d"), 0);
    }
}
