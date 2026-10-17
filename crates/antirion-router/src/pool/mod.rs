//! Self-hosted pools: replicas of one model server, scheduled from the metrics they report (queue
//! and KV cache), the load the replica sent them since their last scrape, and how much of the
//! request's prompt prefix each has already seen. Metrics come from a scraper the runtime runs;
//! this module keeps what it read, the assumed load and the prefix indexes of this replica, and
//! scores the endpoints of a request.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::Value;

use crate::lru::Lru;

pub mod metrics;
pub mod prefix;

pub use metrics::{Lora, LoraFit, MetricNames, Reading};

/// How a pool scrapes its endpoints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scrape {
    pub path: String,
    pub interval: Duration,
    pub timeout: Duration,
    pub names: MetricNames,
}

/// A pool node.
#[derive(Clone, Debug, PartialEq)]
pub struct PoolSpec {
    pub scrape: Scrape,
    pub queue_slack: f64,
    pub kv_limit: f64,
    pub w_prefix: f64,
    pub w_queue: f64,
    pub w_kv: f64,
    pub chunk_chars: usize,
    pub max_chunks: usize,
    pub entries: usize,
    /// `scheduler.lora`: requests for a LoRA adapter prefer endpoints that run it.
    pub lora: bool,
    /// `picker: external`: an endpoint picker chooses the endpoints.
    pub picker: Option<Picker>,
}

/// An external endpoint picker (`picker: external`, `picker_service`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picker {
    pub url: String,
    pub timeout: Duration,
    /// `picker_on_error: builtin`: a picker that fails leaves the choice to the built-in scheduler.
    pub fail_open: bool,
}

impl PoolSpec {
    /// Reads a `pool` node's `metrics` and `scheduler` members.
    #[must_use]
    pub fn from_value(
        node: &serde_json::Map<String, Value>,
        parse: impl Fn(&str) -> Option<Duration>,
    ) -> PoolSpec {
        let m = node.get("metrics").unwrap_or(&Value::Null);
        let s = node.get("scheduler").unwrap_or(&Value::Null);
        let dur = |v: &Value, k: &str, d: u64| {
            v.get(k)
                .and_then(Value::as_str)
                .and_then(&parse)
                .unwrap_or(Duration::from_millis(d))
        };
        let num = |p: &str, d: f64| s.pointer(p).and_then(Value::as_f64).unwrap_or(d);
        let int = |p: &str, d: usize| {
            s.pointer(p)
                .and_then(Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(d)
                .max(1)
        };
        PoolSpec {
            scrape: Scrape {
                path: m
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or("/metrics")
                    .to_owned(),
                interval: dur(m, "interval", 250),
                timeout: dur(m, "timeout", 200),
                names: MetricNames::from_value(m.get("names")),
            },
            queue_slack: num("/queue_slack", 8.0),
            kv_limit: num("/kv_cache_limit", 0.9),
            w_prefix: num("/weights/prefix", 2.0),
            w_queue: num("/weights/queue", 1.0),
            w_kv: num("/weights/kv", 1.0),
            chunk_chars: int("/prefix/chunk_chars", 256),
            max_chunks: int("/prefix/max_chunks", 128),
            entries: int("/prefix/entries", 100_000),
            lora: s.get("lora").and_then(Value::as_bool).unwrap_or(false),
            picker: (node.get("picker").and_then(Value::as_str) == Some("external")).then(|| {
                let svc = node.get("picker_service").unwrap_or(&Value::Null);
                Picker {
                    url: svc
                        .get("url")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    timeout: dur(svc, "timeout", 300),
                    fail_open: node.get("picker_on_error").and_then(Value::as_str) == Some("builtin"),
                }
            }),
        }
    }

    /// Metrics older than this are unknown.
    #[must_use]
    pub fn stale_after(&self) -> Duration {
        self.scrape.interval * 3
    }
}

#[derive(Debug)]
struct Endpoint {
    reading: Option<(Reading, Instant)>,
    /// Attempts in flight that were dispatched to the endpoint, by serial, with when.
    assumed: HashMap<u64, Instant>,
    prefixes: Lru<u64, ()>,
}

/// One endpoint as the scheduler sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct EndpointView {
    /// The latest reading and the attempts dispatched after it that are still in flight; `None`
    /// when the reading is missing or stale.
    pub fresh: Option<(Reading, u32)>,
    /// The share of the request's chunk hashes, from the first, the endpoint has seen.
    pub prefix: f64,
}

/// The endpoints of one pool, by deployment.
type Endpoints = HashMap<String, Endpoint>;

/// The pool state of this replica, by pool and endpoint.
#[derive(Debug, Default)]
pub struct PoolState {
    pools: Mutex<HashMap<String, Endpoints>>,
    serial: Mutex<u64>,
}

fn endpoint_view(e: Option<&Endpoint>, hashes: &[u64], stale_after: Duration, now: Instant) -> EndpointView {
    let Some(e) = e else {
        return EndpointView {
            fresh: None,
            prefix: 0.0,
        };
    };
    let fresh = e
        .reading
        .as_ref()
        .filter(|(_, at)| now.saturating_duration_since(*at) <= stale_after)
        .map(|(r, at)| {
            let assumed = e.assumed.values().filter(|d| **d >= *at).count();
            (r.clone(), u32::try_from(assumed).unwrap_or(u32::MAX))
        });
    let seen = hashes.iter().take_while(|h| e.prefixes.contains(h)).count();
    let prefix = if hashes.is_empty() {
        0.0
    } else {
        seen as f64 / hashes.len() as f64
    };
    EndpointView { fresh, prefix }
}

/// The entry of an endpoint, created on first use (scrapes and dispatches only, never per candidate).
fn endpoint<'a>(
    all: &'a mut HashMap<String, Endpoints>,
    pool: &str,
    deployment: &str,
    entries: usize,
) -> &'a mut Endpoint {
    all.entry(pool.to_owned())
        .or_default()
        .entry(deployment.to_owned())
        .or_insert_with(|| Endpoint {
            reading: None,
            assumed: HashMap::new(),
            prefixes: Lru::new(entries),
        })
}

impl PoolState {
    /// Records a scrape that started at `started`: the reading on success; a failure keeps the old
    /// reading, which ages until it is stale.
    pub fn scraped(
        &self,
        pool: &str,
        deployment: &str,
        reading: Option<Reading>,
        started: Instant,
        entries: usize,
    ) {
        let mut all = self.pools.lock();
        let e = endpoint(&mut all, pool, deployment, entries);
        if let Some(r) = reading {
            e.reading = Some((r, started));
        }
    }

    /// Counts an attempt dispatched to an endpoint and learns its prefix; returns the serial that
    /// [`PoolState::finished`] takes.
    pub fn dispatched(&self, pool: &str, deployment: &str, hashes: &[u64], entries: usize) -> u64 {
        let serial = {
            let mut s = self.serial.lock();
            *s += 1;
            *s
        };
        let mut all = self.pools.lock();
        let e = endpoint(&mut all, pool, deployment, entries);
        e.assumed.insert(serial, Instant::now());
        for h in hashes {
            e.prefixes.put(*h, ());
        }
        serial
    }

    /// The attempt `serial` ended.
    pub fn finished(&self, pool: &str, deployment: &str, serial: u64) {
        if let Some(e) = self
            .pools
            .lock()
            .get_mut(pool)
            .and_then(|p| p.get_mut(deployment))
        {
            e.assumed.remove(&serial);
        }
    }

    /// An endpoint for the scheduler, with the prefix share of `hashes`.
    #[must_use]
    pub fn view(
        &self,
        pool: &str,
        deployment: &str,
        hashes: &[u64],
        stale_after: Duration,
        now: Instant,
    ) -> EndpointView {
        self.views(pool, &[deployment], hashes, stale_after, now)
            .pop()
            .unwrap_or(EndpointView {
                fresh: None,
                prefix: 0.0,
            })
    }

    /// The views of several endpoints of one pool, in order, under one lookup of the pool.
    #[must_use]
    pub fn views(
        &self,
        pool: &str,
        deployments: &[&str],
        hashes: &[u64],
        stale_after: Duration,
        now: Instant,
    ) -> Vec<EndpointView> {
        let all = self.pools.lock();
        let endpoints = all.get(pool);
        deployments
            .iter()
            .map(|d| endpoint_view(endpoints.and_then(|p| p.get(*d)), hashes, stale_after, now))
            .collect()
    }

    /// Forgets every reading, assumption and prefix.
    pub fn reset(&self) {
        self.pools.lock().clear();
    }
}

/// An endpoint the scheduler ranks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Candidate {
    pub queue: f64,
    pub kv: f64,
    /// Unknown metrics: never filtered by the KV cache.
    pub stale: bool,
    pub prefix: f64,
    /// How the endpoint suits a request for a LoRA adapter (`Unknown` without `scheduler.lora`).
    pub lora: LoraFit,
}

/// The LoRA filter over the endpoints still kept: those running the adapter, else those with a
/// free adapter slot, else all.
fn lora_filter(cands: &[Candidate], keep: &mut [bool]) {
    for wanted in [LoraFit::Running, LoraFit::Free] {
        if cands.iter().zip(keep.iter()).any(|(c, k)| *k && c.lora == wanted) {
            for (c, k) in cands.iter().zip(keep.iter_mut()) {
                *k = *k && c.lora == wanted;
            }
            return;
        }
    }
}

/// Ranks endpoints: the queue filter, then the KV filter (skipped when it would remove every
/// endpoint), then the LoRA filter, then the score. Returns, per candidate, whether it passed the
/// filters and its score.
#[must_use]
pub fn rank(spec: &PoolSpec, cands: &[Candidate]) -> Vec<(bool, f64)> {
    let min_q = cands.iter().map(|c| c.queue).fold(f64::INFINITY, f64::min);
    let mut keep: Vec<bool> = cands
        .iter()
        .map(|c| c.queue <= min_q + spec.queue_slack)
        .collect();
    let kv_ok = |c: &Candidate| c.stale || c.kv < spec.kv_limit;
    if cands.iter().zip(&keep).any(|(c, k)| *k && kv_ok(c)) {
        for (c, k) in cands.iter().zip(keep.iter_mut()) {
            *k = *k && kv_ok(c);
        }
    }
    if spec.lora {
        lora_filter(cands, &mut keep);
    }
    let q_max = cands
        .iter()
        .zip(&keep)
        .filter(|(_, k)| **k)
        .map(|(c, _)| c.queue)
        .fold(0.0, f64::max);
    cands
        .iter()
        .zip(keep)
        .map(|(c, k)| {
            let score = spec.w_prefix * c.prefix
                + spec.w_queue * (1.0 - c.queue / (q_max + 1.0))
                + spec.w_kv * (1.0 - c.kv);
            (k, score)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> PoolSpec {
        PoolSpec::from_value(&serde_json::Map::new(), |_| None)
    }

    fn c(queue: f64, kv: f64) -> Candidate {
        Candidate {
            queue,
            kv,
            stale: false,
            prefix: 0.0,
            lora: LoraFit::Unknown,
        }
    }

    #[test]
    fn lora_prefers_the_running_adapter_then_free_slots() {
        let mut s = spec();
        s.lora = true;
        let with = |fit| Candidate {
            lora: fit,
            ..c(0.0, 0.1)
        };
        let r = rank(
            &s,
            &[with(LoraFit::Free), with(LoraFit::Running), with(LoraFit::Full)],
        );
        assert_eq!(
            r.iter().map(|x| x.0).collect::<Vec<_>>(),
            vec![false, true, false]
        );
        let r = rank(&s, &[with(LoraFit::Full), with(LoraFit::Free)]);
        assert_eq!(r.iter().map(|x| x.0).collect::<Vec<_>>(), vec![false, true]);
        let r = rank(&s, &[with(LoraFit::Full), with(LoraFit::Unknown)]);
        assert!(r.iter().all(|x| x.0), "nothing better: every endpoint stays");
        let busy_running = Candidate {
            lora: LoraFit::Running,
            ..c(50.0, 0.1)
        };
        let r = rank(&s, &[busy_running, with(LoraFit::Free)]);
        assert_eq!(
            r.iter().map(|x| x.0).collect::<Vec<_>>(),
            vec![false, true],
            "the queue filter comes first"
        );
        s.lora = false;
        let r = rank(&s, &[with(LoraFit::Free), with(LoraFit::Running)]);
        assert!(r.iter().all(|x| x.0));
    }

    #[test]
    fn queued_and_full_endpoints_are_filtered() {
        let s = spec();
        let r = rank(&s, &[c(10.0, 0.1), c(0.0, 0.1)]);
        assert!(!r[0].0 && r[1].0);
        let r = rank(&s, &[c(0.0, 0.95), c(0.0, 0.2)]);
        assert!(!r[0].0 && r[1].0);
        let r = rank(&s, &[c(0.0, 0.95), c(0.0, 0.95)]);
        assert!(r[0].0 && r[1].0, "the KV filter never removes every endpoint");
    }

    #[test]
    fn prefix_wins_until_the_queue_filter_removes_it() {
        let s = spec();
        let warm = Candidate {
            prefix: 1.0,
            ..c(3.0, 0.1)
        };
        let r = rank(&s, &[warm, c(0.0, 0.1)]);
        assert!(r[0].1 > r[1].1);
        let busy = Candidate {
            prefix: 1.0,
            ..c(50.0, 0.1)
        };
        let r = rank(&s, &[busy, c(0.0, 0.1)]);
        assert!(!r[0].0 && r[1].0);
    }

    #[test]
    fn assumed_load_counts_attempts_in_flight_since_the_reading() {
        let p = PoolState::default();
        let t0 = Instant::now();
        let reading = Reading {
            waiting: 1.0,
            running: 0.0,
            kv: Some(0.1),
            base_model: None,
            lora: None,
            served: None,
        };
        p.scraped("p", "a", Some(reading), t0, 10);
        let s1 = p.dispatched("p", "a", &[1, 2], 10);
        let _s2 = p.dispatched("p", "a", &[1], 10);
        let v = p.view("p", "a", &[1, 2, 3], Duration::from_secs(1), Instant::now());
        assert_eq!(v.fresh.as_ref().map(|(_, a)| *a), Some(2));
        assert!((v.prefix - 2.0 / 3.0).abs() < 1e-9);
        p.finished("p", "a", s1);
        let v = p.view("p", "a", &[], Duration::from_secs(1), Instant::now());
        assert_eq!(v.fresh.as_ref().map(|(_, a)| *a), Some(1));
        let later = Instant::now() + Duration::from_secs(5);
        assert!(
            p.view("p", "a", &[], Duration::from_secs(1), later)
                .fresh
                .is_none()
        );
    }
}
