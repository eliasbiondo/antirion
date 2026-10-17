//! The public probe bodies, shared by both listeners. They expose no dependency details.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use antirion_obs::Fam;
use http::StatusCode;
use serde_json::{Value, json};

use crate::Runtime;

/// Detects scheduling stalls: a task ticks it, and liveness fails when the last tick is older than
/// `server.probes.liveness_stall`.
#[derive(Debug)]
pub struct Watchdog {
    origin: Instant,
    last_ms: AtomicU64,
    stall: Duration,
}

impl Watchdog {
    /// How often the runtime ticks the watchdog.
    pub const TICK: Duration = Duration::from_millis(500);

    #[must_use]
    pub fn new(stall: Duration) -> Watchdog {
        Watchdog {
            origin: Instant::now(),
            last_ms: AtomicU64::new(0),
            stall,
        }
    }

    pub fn tick(&self) {
        let ms = u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.last_ms.store(ms, Ordering::Relaxed);
    }

    #[must_use]
    pub fn stalled(&self) -> bool {
        let last = Duration::from_millis(self.last_ms.load(Ordering::Relaxed));
        self.origin.elapsed().saturating_sub(last) > self.stall
    }
}

#[must_use]
pub fn livez(rt: &Runtime) -> (StatusCode, Value) {
    if rt.liveness.stalled() {
        return (StatusCode::SERVICE_UNAVAILABLE, json!({"status": "stalled"}));
    }
    (StatusCode::OK, json!({"status": "alive"}))
}

/// Whether the node takes traffic: not draining, and no dependency named in
/// `server.probes.readiness_requires` is degraded.
#[must_use]
pub fn ready(rt: &Runtime) -> bool {
    let ready = !rt.drain.is_draining() && required_down(rt, &rt.degraded()).is_none();
    let mut since = rt.state.ready_since.lock();
    match (ready, *since) {
        (true, None) => *since = Some(rt.now()),
        (false, Some(_)) => *since = None,
        _ => {}
    }
    ready
}

/// This node's load and life, as `GET /status` and the heartbeat report them.
#[must_use]
pub fn load(rt: &Runtime) -> antirion_store::pg::NodeLoad {
    let adm = rt.admission.state();
    let to_i64 = |n: u64| i64::try_from(n).unwrap_or(i64::MAX);
    let _ = ready(rt);
    antirion_store::pg::NodeLoad {
        in_flight: to_i64(rt.drain.active()),
        max_in_flight: (adm.limits.max_in_flight > 0).then(|| to_i64(adm.limits.max_in_flight)),
        queue_depth: to_i64(adm.queued),
        started_us: rt.node.started_at.0,
        ready_since_us: rt.state.ready_since.lock().map(|t| t.0),
    }
}

fn required_down<'a>(rt: &Runtime, degraded: &[&'a str]) -> Option<&'a str> {
    degraded
        .iter()
        .find(|d| rt.readiness_requires.iter().any(|r| r == *d))
        .copied()
}

#[must_use]
pub fn readyz(rt: &Runtime) -> (StatusCode, Value) {
    if rt.drain.is_draining() {
        let body = json!({"status": "draining", "node_id": rt.node.id, "in_flight": rt.drain.active()});
        return (StatusCode::SERVICE_UNAVAILABLE, body);
    }
    let degraded = rt.degraded();
    let down = required_down(rt, &degraded);
    let mut body = json!({
        "status": if down.is_some() { "not_ready" } else { "ready" },
        "node_id": rt.node.id,
        "mode": rt.mode(),
        "version": rt.node.version,
        "config_version": rt.gens.load().generation,
        "degraded": degraded,
    });
    if rt.test.is_some()
        && let Some(obj) = body.as_object_mut()
    {
        obj.insert("test_mode".into(), Value::Bool(true));
    }
    let status = if down.is_some() {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    (status, body)
}

/// A cluster node that is still waiting for its store at startup, with no configuration to serve:
/// it is alive, and not ready because of the store.
#[derive(Clone, Debug)]
pub struct Waiting {
    pub node_id: String,
    pub version: String,
}

impl Waiting {
    /// The status and body of `/-/livez` (`live`) or `/-/readyz`.
    #[must_use]
    pub fn probe(&self, live: bool) -> (StatusCode, Value) {
        if live {
            return (StatusCode::OK, json!({"status": "alive"}));
        }
        let body = json!({
            "status": "not_ready",
            "node_id": self.node_id,
            "mode": "cluster",
            "version": self.version,
            "degraded": ["store"],
        });
        (StatusCode::SERVICE_UNAVAILABLE, body)
    }
}

impl Runtime {
    /// Brings the dependency, spool and clock-skew metrics up to date, before an exposition.
    pub fn observe_dependencies(&self) {
        let up = |down: bool| if down { 0.0 } else { 1.0 };
        let m = &self.metrics;
        m.set(Fam::DependencyUp, &["store"], up(self.store.is_down()));
        let engine = self.accounts.engine();
        if engine.store().valkey().is_some() {
            m.set(Fam::DependencyUp, &["counters"], up(engine.is_store_down()));
        }
        let spool = self.ledger.state();
        m.set(Fam::SpoolBytes, &[], spool.bytes as f64);
        m.set(Fam::LedgerRowsDropped, &[], spool.dropped as f64);
        if let Some(skew_ms) = self.cluster.as_ref().and_then(|c| c.view().clock_skew_ms) {
            m.set(Fam::ClockSkew, &[], skew_ms as f64 / 1000.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_watchdog_stalls_only_without_ticks() {
        let w = Watchdog::new(Duration::from_millis(20));
        w.tick();
        assert!(!w.stalled());
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(w.stalled());
        w.tick();
        assert!(!w.stalled());
    }

    #[test]
    fn a_node_waiting_for_its_store_is_alive_and_not_ready() {
        let w = Waiting {
            node_id: "n1".into(),
            version: "1.0.0".into(),
        };
        assert_eq!(w.probe(true), (StatusCode::OK, json!({"status": "alive"})));
        let (status, body) = w.probe(false);
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["status"], "not_ready");
        assert_eq!(body["node_id"], "n1");
        assert_eq!(body["degraded"], json!(["store"]));
    }
}
