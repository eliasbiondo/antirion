//! The two load shapes. Open loop: requests leave on a fixed timeline (constant arrival rate) and are
//! timed from their intended send time, which corrects coordinated omission. Closed loop: a fixed
//! number of connections each send their next request when the previous one ends.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use cpu_time::ProcessTime;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::conn::{ConnectError, H1Pool, H2Pool, Pool};
use crate::exec::{Source, exchange};
use crate::record::{Counts, Histograms, Recorder};

/// How the generator talks to the nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// HTTP/1.1 keep-alive, connections opened on demand up to the cap.
    Http1 { max_connections: usize },
    /// h2 with prior knowledge: a fixed set of connections with a stream cap each.
    H2c { connections: usize, streams: usize },
}

/// The connections to every node of the target; request `node` indexes into it.
#[derive(Debug)]
pub struct Nodes {
    pools: Vec<Pool>,
    drivers: TaskTracker,
    stop: CancellationToken,
}

impl Nodes {
    pub async fn connect(addrs: &[SocketAddr], protocol: Protocol) -> Result<Self, ConnectError> {
        let drivers = TaskTracker::new();
        let stop = CancellationToken::new();
        let mut pools = Vec::with_capacity(addrs.len());
        for &addr in addrs {
            pools.push(match protocol {
                Protocol::Http1 { max_connections } => {
                    Pool::H1(H1Pool::new(addr, max_connections, drivers.clone(), stop.clone()))
                }
                Protocol::H2c { connections, streams } => {
                    Pool::H2(H2Pool::connect(addr, connections, streams, &drivers, &stop).await?)
                }
            });
        }
        Ok(Self { pools, drivers, stop })
    }

    fn pool(&self, node: usize) -> Option<&Pool> {
        self.pools.get(node % self.pools.len().max(1))
    }

    /// Connections opened over the whole life of these nodes.
    #[must_use]
    pub fn connections(&self) -> u64 {
        self.pools.iter().map(Pool::opened).sum()
    }

    /// Closes every connection and waits for their drivers.
    pub async fn close(self) {
        self.stop.cancel();
        self.drivers.close();
        self.drivers.wait().await;
    }
}

/// The measurement window of a run.
#[derive(Debug, Clone, Copy)]
pub struct Window {
    pub start: Instant,
    pub end: Instant,
}

impl Window {
    fn contains(&self, t: Instant) -> bool {
        t >= self.start && t < self.end
    }
}

/// What a run measured.
#[derive(Debug, Clone)]
pub struct RunReport {
    pub hist: Histograms,
    pub counts: Counts,
    pub window: Window,
    /// Generator CPU time over the window, in seconds (every thread of this process).
    pub generator_cpu_s: f64,
    /// Completed requests with the expected status per second of the window.
    pub achieved_rps: f64,
    pub connections: u64,
}

/// Constant arrival rate.
#[derive(Debug, Clone, Copy)]
pub struct OpenLoop {
    pub rate: f64,
    pub warmup: Duration,
    pub measure: Duration,
    pub timeout: Duration,
    /// End every request still open when the window ends (idle streams), counting it as held.
    pub hold: bool,
}

/// Fixed concurrency.
#[derive(Debug, Clone, Copy)]
pub struct ClosedLoop {
    pub concurrency: usize,
    pub warmup: Duration,
    pub measure: Duration,
    pub timeout: Duration,
}

/// Waits on this OS thread until `t`. Sleeps end late by up to a share of their length and by
/// milliseconds on a loaded or power-managed machine (timer coalescing), so a sleep stops well
/// short of the deadline and the last few milliseconds are yielded, then spun: send times stay
/// within microseconds of the schedule.
#[allow(clippy::disallowed_methods)]
fn wait_until(t: Instant) {
    loop {
        let now = Instant::now();
        if now >= t {
            return;
        }
        let left = t - now;
        if left > Duration::from_millis(4) {
            std::thread::sleep(left - (left / 4).max(Duration::from_millis(3)));
        } else if left > Duration::from_micros(50) {
            std::thread::yield_now();
        } else {
            std::hint::spin_loop();
        }
    }
}

async fn cpu_at(t: Instant) -> Duration {
    tokio::time::sleep_until(tokio::time::Instant::from_std(t)).await;
    ProcessTime::try_now()
        .map(|p| p.as_duration())
        .unwrap_or_default()
}

fn report(rec: &Recorder, window: Window, cpu: (Duration, Duration), connections: u64) -> RunReport {
    let (hist, counts) = rec.finish();
    let secs = window.end.saturating_duration_since(window.start).as_secs_f64();
    #[allow(clippy::cast_precision_loss)]
    let achieved = if secs > 0.0 { counts.ok as f64 / secs } else { 0.0 };
    RunReport {
        hist,
        counts,
        window,
        generator_cpu_s: cpu.1.saturating_sub(cpu.0).as_secs_f64(),
        achieved_rps: achieved,
        connections,
    }
}

/// At low rates the runtime's workers are parked between requests, and waking one can take longer
/// than the 1 ms the schedule allows: each request is then handed over this much early and
/// waits for its instant on the worker.
const LEAD: Duration = Duration::from_millis(2);

/// Runs `cfg` from `start` (which should be a little in the future, so the caller can arm its
/// samplers on the returned window). The pacer thread spawns each request onto the runtime itself,
/// so no hop through another task adds to the send lag.
pub async fn open_loop(
    cfg: OpenLoop,
    source: Arc<dyn Source>,
    nodes: Arc<Nodes>,
    start: Instant,
) -> RunReport {
    let window = Window {
        start: start + cfg.warmup,
        end: start + cfg.warmup + cfg.measure,
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let total = ((cfg.warmup + cfg.measure).as_secs_f64() * cfg.rate).floor() as u64;
    let rec = Arc::new(Recorder::default());
    let hold = CancellationToken::new();
    let tasks = TaskTracker::new();
    let period = 1.0 / cfg.rate.max(f64::MIN_POSITIVE);
    let lead = if period >= LEAD.as_secs_f64() * 2.0 {
        LEAD
    } else {
        Duration::ZERO
    };
    let pacer = {
        let (rec, hold, tasks) = (Arc::clone(&rec), hold.clone(), tasks.clone());
        let (source, nodes) = (Arc::clone(&source), Arc::clone(&nodes));
        let handle = tokio::runtime::Handle::current();
        std::thread::spawn(move || {
            for i in 0..total {
                #[allow(clippy::cast_precision_loss)]
                let intended = start + Duration::from_secs_f64(i as f64 * period);
                wait_until(intended.checked_sub(lead).unwrap_or(intended));
                let (source, nodes, rec, hold) = (
                    Arc::clone(&source),
                    Arc::clone(&nodes),
                    Arc::clone(&rec),
                    hold.clone(),
                );
                tasks.spawn_on(
                    async move {
                        while Instant::now() < intended {
                            tokio::task::yield_now().await;
                        }
                        let p = source.prepare(i);
                        let Some(pool) = nodes.pool(p.node) else { return };
                        let out = exchange(pool, p, intended, cfg.timeout, &hold).await;
                        if window.contains(intended) {
                            rec.record(i, &out);
                        }
                    },
                    &handle,
                );
            }
        })
    };
    let cpu = tokio::join!(cpu_at(window.start), cpu_at(window.end));
    if cfg.hold {
        hold.cancel();
    }
    let _ = tokio::task::spawn_blocking(move || pacer.join()).await;
    tasks.close();
    tasks.wait().await;
    report(&rec, window, cpu, nodes.connections())
}

/// Runs `cfg` from `start` with `cfg.concurrency` workers, each timing from its actual send.
pub async fn closed_loop(
    cfg: ClosedLoop,
    source: Arc<dyn Source>,
    nodes: Arc<Nodes>,
    start: Instant,
) -> RunReport {
    let window = Window {
        start: start + cfg.warmup,
        end: start + cfg.warmup + cfg.measure,
    };
    let rec = Arc::new(Recorder::default());
    let next = Arc::new(AtomicU64::new(0));
    let hold = CancellationToken::new();
    let workers = TaskTracker::new();
    tokio::time::sleep_until(tokio::time::Instant::from_std(start)).await;
    for _ in 0..cfg.concurrency.max(1) {
        let (source, nodes, rec, next, hold) = (
            Arc::clone(&source),
            Arc::clone(&nodes),
            Arc::clone(&rec),
            Arc::clone(&next),
            hold.clone(),
        );
        workers.spawn(async move {
            while Instant::now() < window.end {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let p = source.prepare(i);
                let Some(pool) = nodes.pool(p.node) else { return };
                let sent = Instant::now();
                let out = exchange(pool, p, sent, cfg.timeout, &hold).await;
                if window.contains(sent) {
                    rec.record(i, &out);
                }
            }
        });
    }
    workers.close();
    let cpu = tokio::join!(cpu_at(window.start), cpu_at(window.end));
    workers.wait().await;
    report(&rec, window, cpu, nodes.connections())
}
