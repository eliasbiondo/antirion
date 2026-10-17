//! Per-node statistics of extension hook calls, served by `/runtime/plugins`.

use std::collections::{HashMap, VecDeque};

use serde_json::{Value, json};

/// The latency samples kept per extension for its percentiles.
const SAMPLES: usize = 1024;

#[derive(Debug, Default)]
struct Stat {
    calls: u64,
    failures: u64,
    timeouts: u64,
    samples: VecDeque<f64>,
    last_error: Option<String>,
}

/// How one hook call ended, for the statistics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallEnd {
    Ok,
    Failed,
    TimedOut,
}

/// Hook call counts, failures and latencies per extension name.
#[derive(Debug, Default)]
pub struct ExtensionStats {
    inner: parking_lot::Mutex<HashMap<String, Stat>>,
}

fn percentile(sorted: &[f64], q: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = ((q * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted.get(rank - 1).copied()
}

impl ExtensionStats {
    /// Records one call of `name` that took `ms`.
    pub fn record(&self, name: &str, ms: f64, end: CallEnd, error: Option<&str>) {
        let mut map = self.inner.lock();
        let s = map.entry(name.to_owned()).or_default();
        s.calls += 1;
        match end {
            CallEnd::Ok => {}
            CallEnd::Failed => s.failures += 1,
            CallEnd::TimedOut => {
                s.failures += 1;
                s.timeouts += 1;
            }
        }
        if let Some(e) = error {
            s.last_error = Some(e.to_owned());
        }
        if s.samples.len() == SAMPLES {
            s.samples.pop_front();
        }
        s.samples.push_back(ms);
    }

    /// `{calls, failures, timeouts, p50_ms, p99_ms, last_error}` of one extension (zeros when it
    /// has not been called on this node).
    #[must_use]
    pub fn report(&self, name: &str) -> Value {
        let map = self.inner.lock();
        let Some(s) = map.get(name) else {
            return json!({"name": name, "calls": 0, "failures": 0, "timeouts": 0, "p50_ms": null, "p99_ms": null, "last_error": null});
        };
        let mut sorted: Vec<f64> = s.samples.iter().copied().collect();
        sorted.sort_by(f64::total_cmp);
        json!({
            "name": name,
            "calls": s.calls,
            "failures": s.failures,
            "timeouts": s.timeouts,
            "p50_ms": percentile(&sorted, 0.5),
            "p99_ms": percentile(&sorted, 0.99),
            "last_error": s.last_error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_percentiles() {
        let s = ExtensionStats::default();
        s.record("a", 10.0, CallEnd::Ok, None);
        s.record("a", 30.0, CallEnd::TimedOut, Some("timeout"));
        let r = s.report("a");
        assert_eq!(r["calls"], 2);
        assert_eq!(r["timeouts"], 1);
        assert_eq!(r["failures"], 1);
        assert_eq!(r["p50_ms"], 10.0);
        assert_eq!(r["last_error"], "timeout");
        assert_eq!(s.report("b")["calls"], 0);
    }
}
