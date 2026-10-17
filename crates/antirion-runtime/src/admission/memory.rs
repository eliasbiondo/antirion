//! Memory-pressure shedding: a sampler reads the process's resident set every `SAMPLE_EVERY` and
//! turns shedding on above the high watermark and off below the low one. The request path reads
//! one flag.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use antirion_config::{ConfigError, DetailCode, DocPath};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::Admission;

const SAMPLE_EVERY: Duration = Duration::from_millis(100);

/// One watermark: a size in bytes, or a ratio of the cgroup's memory limit in millionths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Watermark {
    Bytes(u64),
    Ratio(u32),
}

impl Watermark {
    /// Reads a watermark value: a size string or a JSON number in [0, 1].
    #[must_use]
    pub fn from_value(v: &Value) -> Option<Watermark> {
        match v {
            Value::String(s) => antirion_config::scalar::parse_size(s).map(Watermark::Bytes),
            Value::Number(n) => {
                let r = n.as_f64()?;
                (0.0..=1.0)
                    .contains(&r)
                    .then(|| Watermark::Ratio((r * 1_000_000.0).round() as u32))
            }
            _ => None,
        }
    }

    /// The watermark in bytes; a ratio needs the cgroup limit.
    fn bytes(self, cgroup_limit: Option<u64>) -> Option<u64> {
        match self {
            Watermark::Bytes(b) => Some(b),
            Watermark::Ratio(ppm) => cgroup_limit
                .map(|l| u64::try_from(u128::from(l) * u128::from(ppm) / 1_000_000).unwrap_or(u64::MAX)),
        }
    }
}

/// `memory_high_watermark` and `memory_low_watermark`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Watermarks {
    pub high: Watermark,
    pub low: Watermark,
}

impl Watermarks {
    /// The watermarks of a `server.admission` block; `None` when shedding is off.
    #[must_use]
    pub fn from_block(block: Option<&Value>) -> Option<Watermarks> {
        let b = block?;
        let high = Watermark::from_value(b.get("memory_high_watermark")?)?;
        let low = b
            .get("memory_low_watermark")
            .and_then(Watermark::from_value)
            .unwrap_or(high);
        Some(Watermarks { high, low })
    }

    /// The next shedding state for a resident set of `rss` bytes.
    fn next(self, shedding: bool, rss: u64, cgroup_limit: Option<u64>) -> bool {
        let (Some(high), Some(low)) = (self.high.bytes(cgroup_limit), self.low.bytes(cgroup_limit)) else {
            return false;
        };
        if rss > high {
            true
        } else if rss < low {
            false
        } else {
            shedding
        }
    }
}

/// The rule the schema cannot state: the low watermark is of the high one's kind and below it.
#[must_use]
pub fn check_document(doc: &Value) -> Vec<ConfigError> {
    let Some(block) = doc.pointer("/server/admission") else {
        return Vec::new();
    };
    let (Some(high), Some(low)) = (
        block.get("memory_high_watermark").and_then(Watermark::from_value),
        block.get("memory_low_watermark").and_then(Watermark::from_value),
    ) else {
        return Vec::new();
    };
    let below = match (high, low) {
        (Watermark::Bytes(h), Watermark::Bytes(l)) => l < h,
        (Watermark::Ratio(h), Watermark::Ratio(l)) => l < h,
        _ => false,
    };
    if below {
        return Vec::new();
    }
    vec![ConfigError::new(
        &DocPath::root()
            .key("server")
            .key("admission")
            .key("memory_low_watermark"),
        DetailCode::Conflict,
        "memory_low_watermark must be of the same kind as memory_high_watermark (both sizes or both \
         ratios) and below it",
    )]
}

/// The cgroup memory limit of the process (v2, then v1); `None` without one.
async fn cgroup_limit() -> Option<u64> {
    for path in [
        "/sys/fs/cgroup/memory.max",
        "/sys/fs/cgroup/memory/memory.limit_in_bytes",
    ] {
        if let Ok(text) = tokio::fs::read_to_string(path).await
            && let Ok(n) = text.trim().parse::<u64>()
            && n < u64::MAX / 2
        {
            return Some(n);
        }
    }
    None
}

async fn rss() -> Option<u64> {
    tokio::task::spawn_blocking(memory_stats::memory_stats)
        .await
        .ok()
        .flatten()
        .and_then(|m| u64::try_from(m.physical_mem).ok())
}

impl Admission {
    /// Whether new inference requests are shed for memory pressure.
    #[must_use]
    pub fn shedding(&self) -> bool {
        self.shedding.load(Ordering::Relaxed)
    }

    /// Samples the resident set until `stop`, while watermarks are configured.
    pub async fn watch_memory(self: Arc<Self>, stop: CancellationToken) {
        let limit = cgroup_limit().await;
        let mut warned = false;
        loop {
            tokio::select! {
                () = stop.cancelled() => return,
                () = tokio::time::sleep(SAMPLE_EVERY) => {}
            }
            let Some(marks) = self.inner.lock().limits.memory else {
                self.shedding.store(false, Ordering::Relaxed);
                continue;
            };
            if limit.is_none() && matches!(marks.high, Watermark::Ratio(_)) && !warned {
                tracing::warn!(
                    "memory_high_watermark is a ratio of the cgroup memory limit, and the process has none: \
                     no memory-pressure shedding"
                );
                warned = true;
            }
            let Some(rss) = rss().await else { continue };
            let was = self.shedding();
            let now = marks.next(was, rss, limit);
            if now != was {
                tracing::warn!(rss, shedding = now, "memory-pressure shedding changed");
                self.shedding.store(now, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn watermarks_have_hysteresis() {
        let m = Watermarks::from_block(Some(&json!({
            "memory_high_watermark": "100B", "memory_low_watermark": "50B"
        })))
        .unwrap();
        assert!(!m.next(false, 80, None));
        assert!(m.next(false, 101, None));
        assert!(m.next(true, 80, None));
        assert!(!m.next(true, 49, None));
        let r = Watermarks::from_block(Some(&json!({
            "memory_high_watermark": 0.8, "memory_low_watermark": 0.5
        })))
        .unwrap();
        assert!(r.next(false, 900, Some(1000)));
        assert!(!r.next(false, 900, None));
        assert!(Watermarks::from_block(Some(&json!({"memory_high_watermark": null}))).is_none());
    }

    #[test]
    fn the_low_watermark_is_below_the_high_one() {
        let doc = |h: Value, l: Value| json!({"server": {"admission": {"memory_high_watermark": h, "memory_low_watermark": l}}});
        assert!(check_document(&doc(json!("2GiB"), json!("1GiB"))).is_empty());
        assert!(check_document(&doc(json!(0.9), json!(0.8))).is_empty());
        assert_eq!(check_document(&doc(json!("1GiB"), json!("2GiB"))).len(), 1);
        assert_eq!(check_document(&doc(json!(0.9), json!("1GiB"))).len(), 1);
        assert!(check_document(&json!({})).is_empty());
    }
}
