//! Guardrail statistics over the last 24 hours, from every node's hourly counts, and the
//! false-positive rate of each guardrail's labelled events over 30 days.

use std::collections::HashMap;

use antirion_obs::sketch::Sketch;
use antirion_store::StoreError;

use crate::Runtime;

const HOUR_US: i64 = 3_600_000_000;
/// The hours a statistic covers: the current one and the 23 before it.
pub const HOURS: usize = 24;
const FP_WINDOW_US: i64 = 30 * 24 * HOUR_US;

/// One guardrail's figures.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GuardrailStats {
    pub evaluations: u64,
    pub hits: u64,
    /// Hits per hour, oldest first; the last is the current hour.
    pub hits_by_hour: Vec<u64>,
    pub blocks: u64,
    pub redactions: u64,
    pub flags: u64,
    pub errors: u64,
    pub p95_latency_ms: Option<f64>,
    pub false_positive_rate: Option<f64>,
}

fn n(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

impl Runtime {
    /// Every guardrail's figures, cluster-wide, by name.
    pub async fn guardrail_stats(&self) -> Result<HashMap<String, GuardrailStats>, StoreError> {
        self.read_safety().await;
        let now = self.now().0;
        let hour = now - now.rem_euclid(HOUR_US);
        let first = hour - (HOURS as i64 - 1) * HOUR_US;
        let mut sketches: HashMap<String, Sketch> = HashMap::new();
        let mut out: HashMap<String, GuardrailStats> = HashMap::new();
        for h in self.store.guardrail_hours(first).await? {
            let Ok(slot) = usize::try_from((h.hour_us - first) / HOUR_US) else {
                continue;
            };
            if slot >= HOURS {
                continue;
            }
            let s = out.entry(h.guardrail.clone()).or_insert_with(|| GuardrailStats {
                hits_by_hour: vec![0; HOURS],
                ..GuardrailStats::default()
            });
            s.evaluations += n(h.evaluations);
            s.hits += n(h.hits);
            s.blocks += n(h.blocks);
            s.redactions += n(h.redactions);
            s.flags += n(h.flags);
            s.errors += n(h.errors);
            if let Some(b) = s.hits_by_hour.get_mut(slot) {
                *b += n(h.hits);
            }
            sketches
                .entry(h.guardrail)
                .or_default()
                .merge(&Sketch::decode(&h.latency));
        }
        for (name, sk) in sketches {
            if let Some(s) = out.get_mut(&name) {
                s.p95_latency_ms = sk.quantile(0.95).map(|v| (v * 1000.0).round() / 1000.0);
            }
        }
        for (name, fp, labelled) in self.store.review_counts(now - FP_WINDOW_US).await? {
            if labelled <= 0 {
                continue;
            }
            #[allow(clippy::cast_precision_loss)]
            let rate = fp as f64 / labelled as f64;
            out.entry(name)
                .or_insert_with(|| GuardrailStats {
                    hits_by_hour: vec![0; HOURS],
                    ..GuardrailStats::default()
                })
                .false_positive_rate = Some(rate);
        }
        Ok(out)
    }
}
