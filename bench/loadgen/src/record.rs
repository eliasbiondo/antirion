//! What a run records: latency histograms at 1 µs resolution and the outcome counts.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use hdrhistogram::Histogram;

/// The highest value a histogram holds: one hour in microseconds.
pub const MAX_US: u64 = 3_600_000_000;

/// A histogram in microseconds, 3 significant digits.
#[must_use]
pub fn histogram() -> Histogram<u64> {
    Histogram::new_with_bounds(1, MAX_US, 3).expect("1 µs to one hour at 3 digits is a valid range")
}

fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(MAX_US).clamp(1, MAX_US)
}

/// The histograms of one run.
#[derive(Debug, Clone)]
pub struct Histograms {
    /// Intended send time to the last byte, for responses with the expected status.
    pub latency: Histogram<u64>,
    /// Intended send time to the first event that carries content.
    pub ttft: Histogram<u64>,
    /// Every gap between consecutive events of a stream.
    pub gap: Histogram<u64>,
    /// The gateway's own overhead header, in microseconds.
    pub overhead_header: Histogram<u64>,
    /// Intended to actual send time.
    pub lag: Histogram<u64>,
}

impl Default for Histograms {
    fn default() -> Self {
        Self {
            latency: histogram(),
            ttft: histogram(),
            gap: histogram(),
            overhead_header: histogram(),
            lag: histogram(),
        }
    }
}

impl Histograms {
    fn add(&mut self, other: &Histograms) {
        let _ = self.latency.add(&other.latency);
        let _ = self.ttft.add(&other.ttft);
        let _ = self.gap.add(&other.gap);
        let _ = self.overhead_header.add(&other.overhead_header);
        let _ = self.lag.add(&other.lag);
    }
}

/// Outcome counts of the measured requests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counts {
    /// Requests whose intended send time fell in the measurement window.
    pub offered: u64,
    /// Responses with the expected status, read to the end.
    pub ok: u64,
    /// Responses with any other status, by status.
    pub non_2xx: BTreeMap<u16, u64>,
    /// Connections that could not be opened, or failed before a response head.
    pub connect_errors: u64,
    /// Requests that did not finish within the timeout.
    pub timeouts: u64,
    /// Bodies that failed mid-way.
    pub body_errors: u64,
    /// Streams the generator left on purpose after their first content event.
    pub disconnects: u64,
    /// Streams still open when a hold-mode run ended.
    pub held: u64,
    /// Requests sent more than 1 ms after their intended time.
    pub late: u64,
}

impl Counts {
    fn add(&mut self, other: &Counts) {
        self.offered += other.offered;
        self.ok += other.ok;
        for (status, n) in &other.non_2xx {
            *self.non_2xx.entry(*status).or_default() += n;
        }
        self.connect_errors += other.connect_errors;
        self.timeouts += other.timeouts;
        self.body_errors += other.body_errors;
        self.disconnects += other.disconnects;
        self.held += other.held;
        self.late += other.late;
    }

    /// Requests that ended in an error of any class.
    #[must_use]
    pub fn errors(&self) -> u64 {
        self.non_2xx.values().sum::<u64>() + self.connect_errors + self.timeouts + self.body_errors
    }
}

/// How one request ended.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub lag: Duration,
    pub kind: OutcomeKind,
    pub latency: Option<Duration>,
    pub ttft: Option<Duration>,
    pub gaps: Vec<Duration>,
    pub overhead_header_us: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OutcomeKind {
    #[default]
    Ok,
    Status(u16),
    Connect,
    Timeout,
    Body,
    Disconnected,
    Held,
}

const LATE: Duration = Duration::from_millis(1);
const SHARDS: usize = 64;

#[derive(Debug, Default)]
struct Shard {
    hist: Histograms,
    counts: Counts,
}

/// A recorder shared by every request task of a run, sharded to keep lock contention low.
#[derive(Debug)]
pub struct Recorder {
    shards: Vec<Mutex<Shard>>,
}

impl Default for Recorder {
    fn default() -> Self {
        Self {
            shards: (0..SHARDS).map(|_| Mutex::new(Shard::default())).collect(),
        }
    }
}

impl Recorder {
    /// Records the outcome of measured request `index`.
    pub fn record(&self, index: u64, o: &Outcome) {
        let shard = usize::try_from(index).unwrap_or(0) % SHARDS;
        let Some(cell) = self.shards.get(shard) else {
            return;
        };
        let Ok(mut s) = cell.lock() else { return };
        s.counts.offered += 1;
        let _ = s.hist.lag.record(micros(o.lag));
        if o.lag > LATE {
            s.counts.late += 1;
        }
        match o.kind {
            OutcomeKind::Ok => s.counts.ok += 1,
            OutcomeKind::Status(code) => *s.counts.non_2xx.entry(code).or_default() += 1,
            OutcomeKind::Connect => s.counts.connect_errors += 1,
            OutcomeKind::Timeout => s.counts.timeouts += 1,
            OutcomeKind::Body => s.counts.body_errors += 1,
            OutcomeKind::Disconnected => s.counts.disconnects += 1,
            OutcomeKind::Held => s.counts.held += 1,
        }
        if o.kind == OutcomeKind::Ok
            && let Some(l) = o.latency
        {
            let _ = s.hist.latency.record(micros(l));
        }
        if let Some(t) = o.ttft {
            let _ = s.hist.ttft.record(micros(t));
        }
        for g in &o.gaps {
            let _ = s.hist.gap.record(micros(*g));
        }
        if let Some(us) = o.overhead_header_us {
            let _ = s.hist.overhead_header.record(us.clamp(1, MAX_US));
        }
    }

    /// Merges every shard.
    #[must_use]
    pub fn finish(&self) -> (Histograms, Counts) {
        let mut hist = Histograms::default();
        let mut counts = Counts::default();
        for cell in &self.shards {
            if let Ok(s) = cell.lock() {
                hist.add(&s.hist);
                counts.add(&s.counts);
            }
        }
        (hist, counts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes_land_in_their_counts() {
        let r = Recorder::default();
        let ok = Outcome {
            latency: Some(Duration::from_micros(250)),
            ..Outcome::default()
        };
        r.record(1, &ok);
        r.record(
            2,
            &Outcome {
                kind: OutcomeKind::Status(503),
                lag: Duration::from_millis(3),
                ..Outcome::default()
            },
        );
        r.record(
            3,
            &Outcome {
                kind: OutcomeKind::Timeout,
                ..Outcome::default()
            },
        );
        let (h, c) = r.finish();
        assert_eq!(c.offered, 3);
        assert_eq!(c.ok, 1);
        assert_eq!(c.non_2xx.get(&503), Some(&1));
        assert_eq!(c.timeouts, 1);
        assert_eq!(c.late, 1);
        assert_eq!(c.errors(), 2);
        assert_eq!(h.latency.len(), 1);
        assert!(h.latency.equivalent(h.latency.value_at_quantile(0.5), 250));
    }
}
