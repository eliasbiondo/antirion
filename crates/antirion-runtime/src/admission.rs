//! Gateway-level admission control: at most `max_in_flight` inference requests hold a slot, at most
//! `max_queue` more wait for one (in FIFO order, or by project in turn), each for at most
//! `queue_timeout`; above the memory high watermark new requests are shed. The settings change at
//! run time without losing the queue's order.

mod lanes;
pub mod memory;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use antirion_config::scalar::parse_duration;
use antirion_config::views::AdmissionView;
use antirion_obs::{Fam, Metrics};
use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// Reasons of the admission rules.
pub mod reasons {
    antirion_error::reasons! {
        QUEUE_FULL = "queue_full" => Overloaded,
        QUEUE_TIMEOUT = "queue_timeout" => Overloaded,
        MEMORY_PRESSURE = "memory_pressure" => Overloaded,
    }
}

/// The admission bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_in_flight: u64,
    pub max_queue: u64,
    pub queue_timeout: Duration,
    /// `fairness: project`: queued requests are served by project in turn.
    pub fair: bool,
    /// The memory watermarks, when shedding is on.
    pub memory: Option<memory::Watermarks>,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_in_flight: 16_384,
            max_queue: 4_096,
            queue_timeout: Duration::from_secs(2),
            fair: false,
            memory: None,
        }
    }
}

impl Limits {
    /// The limits a `server.admission` block sets, defaults for what it leaves out.
    #[must_use]
    pub fn from_view(v: &AdmissionView) -> Limits {
        let d = Limits::default();
        Limits {
            max_in_flight: v.max_in_flight.unwrap_or(d.max_in_flight).max(1),
            max_queue: v.max_queue.unwrap_or(d.max_queue),
            queue_timeout: v
                .queue_timeout
                .as_deref()
                .and_then(parse_duration)
                .filter(|t| !t.is_zero())
                .unwrap_or(d.queue_timeout),
            fair: v.fairness.as_deref() == Some("project"),
            memory: None,
        }
    }

    /// The limits of the document's `server.admission`, else of the startup file's, else defaults.
    #[must_use]
    pub fn effective(doc: &Value, statics: &Value) -> Limits {
        let block = doc
            .pointer("/server/admission")
            .or_else(|| statics.pointer("/server/admission"));
        let view = block
            .and_then(|b| serde_json::from_value::<AdmissionView>(b.clone()).ok())
            .unwrap_or_default();
        Limits {
            memory: memory::Watermarks::from_block(block),
            ..Limits::from_view(&view)
        }
    }
}

/// Why a request was not admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// Every slot and queue position is taken, or the queue shrank below the request's place.
    QueueFull,
    /// The request waited `queue_timeout` without a slot.
    QueueTimeout,
    /// The request was abandoned while it waited.
    Gone,
}

/// What `GET /health` reports.
#[derive(Clone, Copy, Debug)]
pub struct State {
    pub in_flight: u64,
    pub queued: u64,
    pub limits: Limits,
}

struct Inner {
    limits: Limits,
    in_flight: u64,
    queue: lanes::Lanes,
    next_id: u64,
}

/// The node's admission state.
pub struct Admission {
    inner: Mutex<Inner>,
    metrics: Metrics,
    shedding: AtomicBool,
}

impl std::fmt::Debug for Admission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Admission").finish_non_exhaustive()
    }
}

/// A held slot; dropping it frees the slot for the oldest queued request.
#[derive(Debug)]
pub struct Permit {
    adm: Arc<Admission>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.adm.release();
    }
}

/// A queued request; dropping it leaves the queue, or gives back a slot granted but never taken.
struct Queued {
    adm: Arc<Admission>,
    id: u64,
    lane: u64,
    rx: Option<oneshot::Receiver<Result<(), Refused>>>,
    settled: bool,
}

impl Drop for Queued {
    fn drop(&mut self) {
        if self.settled || self.adm.withdraw(self.id, self.lane) {
            return;
        }
        if let Some(Ok(Ok(()))) = self.rx.take().map(|mut rx| rx.try_recv()) {
            self.adm.release();
        }
    }
}

impl Admission {
    #[must_use]
    pub fn new(metrics: Metrics) -> Arc<Admission> {
        Arc::new(Admission {
            inner: Mutex::new(Inner {
                limits: Limits::default(),
                in_flight: 0,
                queue: lanes::Lanes::default(),
                next_id: 0,
            }),
            metrics,
            shedding: AtomicBool::new(false),
        })
    }

    #[must_use]
    pub fn state(&self) -> State {
        let g = self.inner.lock();
        State {
            in_flight: g.in_flight,
            queued: g.queue.len() as u64,
            limits: g.limits,
        }
    }

    /// Applies new limits: queued requests are admitted up to a larger bound, and the newest beyond a
    /// smaller queue are refused at once.
    pub fn configure(&self, limits: Limits) {
        let mut g = self.inner.lock();
        if g.limits == limits {
            return;
        }
        g.limits = limits;
        g.queue.set_fair(limits.fair);
        Self::pump(&mut g);
        let mut shed = 0;
        while g.queue.len() as u64 > limits.max_queue {
            let Some(w) = g.queue.pop_newest() else { break };
            let _ = w.tx.send(Err(Refused::QueueFull));
            shed += 1;
        }
        self.gauges(&g);
        drop(g);
        if shed > 0 {
            self.metrics
                .add(Fam::AdmissionRejected, &["queue_full"], f64::from(shed));
        }
    }

    /// Takes a slot for a request of `project`, waiting in the queue when every slot is taken. `gone`
    /// ends the wait without one.
    pub async fn acquire(
        self: &Arc<Self>,
        gone: &CancellationToken,
        project: &str,
    ) -> Result<Permit, Refused> {
        let (queued, timeout) = {
            let mut g = self.inner.lock();
            if g.in_flight < g.limits.max_in_flight {
                g.in_flight += 1;
                self.gauges(&g);
                return Ok(Permit {
                    adm: Arc::clone(self),
                });
            }
            if g.queue.len() as u64 >= g.limits.max_queue {
                drop(g);
                self.metrics.add(Fam::AdmissionRejected, &["queue_full"], 1.0);
                return Err(Refused::QueueFull);
            }
            let (tx, rx) = oneshot::channel();
            let id = g.next_id;
            g.next_id += 1;
            let lane = lane_of(project);
            g.queue.push(lanes::Waiter { id, lane, tx });
            self.gauges(&g);
            let queued = Queued {
                adm: Arc::clone(self),
                id,
                lane,
                rx: Some(rx),
                settled: false,
            };
            (queued, g.limits.queue_timeout)
        };
        let mut queued = queued;
        let Some(rx) = queued.rx.as_mut() else {
            return Err(Refused::Gone);
        };
        let decided = tokio::select! {
            r = rx => r.ok(),
            () = tokio::time::sleep(timeout) => None,
            () = gone.cancelled() => None,
        };
        let outcome = match decided {
            Some(r) => r,
            None if self.withdraw(queued.id, queued.lane) => {
                if gone.is_cancelled() {
                    Err(Refused::Gone)
                } else {
                    self.metrics.add(Fam::AdmissionRejected, &["queue_timeout"], 1.0);
                    Err(Refused::QueueTimeout)
                }
            }
            None => queued
                .rx
                .as_mut()
                .and_then(|rx| rx.try_recv().ok())
                .unwrap_or(Err(Refused::Gone)),
        };
        queued.settled = true;
        match outcome {
            Ok(()) => {
                let permit = Permit {
                    adm: Arc::clone(self),
                };
                self.hand_on();
                if gone.is_cancelled() {
                    drop(permit);
                    return Err(Refused::Gone);
                }
                Ok(permit)
            }
            Err(e) => Err(e),
        }
    }

    /// Removes a waiter from the queue; false when it already left it (admitted or shed).
    fn withdraw(&self, id: u64, lane: u64) -> bool {
        let mut g = self.inner.lock();
        if !g.queue.remove(id, lane) {
            return false;
        }
        self.gauges(&g);
        true
    }

    fn release(&self) {
        let mut g = self.inner.lock();
        g.in_flight = g.in_flight.saturating_sub(1);
        Self::pump(&mut g);
        self.gauges(&g);
    }

    /// Hands a free slot to the next waiter. Each admitted waiter hands on the next free slot when
    /// it resumes, so requests admitted together start in queue order.
    fn pump(g: &mut Inner) {
        while g.in_flight < g.limits.max_in_flight {
            let Some(w) = g.queue.pop() else {
                break;
            };
            if w.tx.send(Ok(())).is_ok() {
                g.in_flight += 1;
                break;
            }
        }
    }

    /// Passes the turn on after a waiter took its slot.
    fn hand_on(&self) {
        let mut g = self.inner.lock();
        Self::pump(&mut g);
        self.gauges(&g);
    }

    fn gauges(&self, g: &Inner) {
        self.metrics.set(Fam::AdmissionInFlight, &[], g.in_flight as f64);
        self.metrics
            .set(Fam::AdmissionQueueDepth, &[], g.queue.len() as f64);
    }
}

/// A project's lane: a hash of its name, so queueing takes no copy of it.
fn lane_of(project: &str) -> u64 {
    use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
    BuildHasherDefault::<DefaultHasher>::default().hash_one(project)
}

#[cfg(test)]
mod tests {
    use antirion_obs::BuildInfo;
    use tokio_util::task::TaskTracker;

    use super::*;

    fn admission(max_in_flight: u64, max_queue: u64, timeout_ms: u64) -> Arc<Admission> {
        let a = Admission::new(Metrics::new(&BuildInfo {
            version: String::new(),
            commit: String::new(),
            rustc: String::new(),
            semconv: String::new(),
            test_mode: false,
        }));
        a.configure(Limits {
            max_in_flight,
            max_queue,
            queue_timeout: Duration::from_millis(timeout_ms),
            ..Limits::default()
        });
        a
    }

    #[tokio::test]
    async fn a_full_node_queues_then_refuses() {
        let tasks = TaskTracker::new();
        let a = admission(1, 1, 50);
        let never = CancellationToken::new();
        let held = a.acquire(&never, "p").await.unwrap();
        let waiting = {
            let a = Arc::clone(&a);
            tasks.spawn(async move { a.acquire(&CancellationToken::new(), "p").await.map(drop) })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(a.acquire(&never, "p").await.unwrap_err(), Refused::QueueFull);
        assert_eq!(waiting.await.unwrap().unwrap_err(), Refused::QueueTimeout);
        drop(held);
        assert_eq!(a.state().in_flight, 0);
    }

    #[tokio::test]
    async fn a_freed_slot_goes_to_the_oldest_waiter() {
        let tasks = TaskTracker::new();
        let a = admission(1, 5, 5_000);
        let held = a.acquire(&CancellationToken::new(), "p").await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        for i in 0..3 {
            let a = Arc::clone(&a);
            let tx = tx.clone();
            tasks.spawn(async move {
                let p = a.acquire(&CancellationToken::new(), "p").await;
                let _ = tx.send(i);
                drop(p);
            });
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(a.state().queued, 3);
        drop(held);
        let order = [rx.recv().await, rx.recv().await, rx.recv().await];
        assert_eq!(order, [Some(0), Some(1), Some(2)]);
    }

    #[tokio::test]
    async fn shrinking_sheds_the_newest_and_growing_admits_at_once() {
        let tasks = TaskTracker::new();
        let a = admission(1, 5, 5_000);
        let _held = a.acquire(&CancellationToken::new(), "p").await.unwrap();
        let mut waiters = Vec::new();
        for _ in 0..4 {
            let a = Arc::clone(&a);
            waiters.push(tasks.spawn(async move { a.acquire(&CancellationToken::new(), "p").await }));
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        a.configure(Limits {
            max_in_flight: 1,
            max_queue: 2,
            queue_timeout: Duration::from_secs(5),
            ..Limits::default()
        });
        let newest = waiters.split_off(2);
        for w in newest {
            assert_eq!(w.await.unwrap().unwrap_err(), Refused::QueueFull);
        }
        a.configure(Limits {
            max_in_flight: 3,
            max_queue: 2,
            queue_timeout: Duration::from_secs(5),
            ..Limits::default()
        });
        for w in waiters {
            assert!(w.await.unwrap().is_ok());
        }
        assert_eq!(a.state().queued, 0);
    }

    #[tokio::test]
    async fn with_fairness_a_lone_project_is_served_next() {
        let tasks = TaskTracker::new();
        let a = admission(1, 20, 5_000);
        a.configure(Limits {
            max_in_flight: 1,
            max_queue: 20,
            queue_timeout: Duration::from_secs(5),
            fair: true,
            memory: None,
        });
        let held = a.acquire(&CancellationToken::new(), "p").await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        for (i, project) in ["busy", "busy", "busy", "busy", "lone"].into_iter().enumerate() {
            let a = Arc::clone(&a);
            let tx = tx.clone();
            tasks.spawn(async move {
                let p = a.acquire(&CancellationToken::new(), project).await;
                let _ = tx.send(i);
                drop(p);
            });
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        drop(held);
        let first_two = [rx.recv().await, rx.recv().await];
        assert!(first_two.contains(&Some(4)), "{first_two:?}");
    }

    #[tokio::test]
    async fn a_waiter_that_leaves_frees_its_place() {
        let tasks = TaskTracker::new();
        let a = admission(1, 1, 5_000);
        let held = a.acquire(&CancellationToken::new(), "p").await.unwrap();
        let gone = CancellationToken::new();
        let waiter = {
            let a = Arc::clone(&a);
            let gone = gone.clone();
            tasks.spawn(async move { a.acquire(&gone, "p").await.map(drop) })
        };
        tokio::time::sleep(Duration::from_millis(5)).await;
        gone.cancel();
        assert_eq!(waiter.await.unwrap().unwrap_err(), Refused::Gone);
        assert_eq!(a.state().queued, 0);
        drop(held);
        assert_eq!(a.state().in_flight, 0);
    }

    #[test]
    fn the_document_wins_over_the_startup_file() {
        let doc = serde_json::json!({"server": {"admission": {"max_in_flight": 3}}});
        let statics = serde_json::json!({"server": {"admission": {"max_in_flight": 9, "max_queue": 0}}});
        assert_eq!(Limits::effective(&doc, &statics).max_in_flight, 3);
        assert_eq!(Limits::effective(&doc, &statics).max_queue, 4_096);
        assert_eq!(Limits::effective(&Value::Null, &statics).max_queue, 0);
    }
}
