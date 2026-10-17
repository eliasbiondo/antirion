//! The provider pressure history: the node's minutes of upstream quota use go to the store with
//! the request log's writes, and reads put every node's minutes into buckets.

use antirion_providers::pressure::{Sample, Series, Used, series, starts};
use antirion_store::{PressureRow, StoreError};

use crate::Runtime;

const MINUTE_US: i64 = 60_000_000;
const HOUR_US: i64 = 60 * MINUTE_US;
/// How long minutes are kept.
pub const KEEP_US: i64 = 90 * 24 * HOUR_US;

fn signed(v: Option<u64>) -> Option<i64> {
    v.map(|v| i64::try_from(v).unwrap_or(i64::MAX))
}

fn unsigned(v: Option<i64>) -> Option<u64> {
    v.and_then(|v| u64::try_from(v).ok())
}

fn row(s: Sample) -> PressureRow {
    PressureRow {
        provider: s.provider,
        credential: s.credential,
        minute_us: s.minute_us,
        req_used: signed(s.requests.used),
        req_limit: signed(s.requests.limit),
        tok_used: signed(s.tokens.used),
        tok_limit: signed(s.tokens.limit),
    }
}

fn sample(r: PressureRow) -> Sample {
    Sample {
        provider: r.provider,
        credential: r.credential,
        minute_us: r.minute_us,
        requests: Used {
            used: unsigned(r.req_used),
            limit: unsigned(r.req_limit),
        },
        tokens: Used {
            used: unsigned(r.tok_used),
            limit: unsigned(r.tok_limit),
        },
    }
}

impl Runtime {
    /// Writes the minutes this node collected, and trims the history at most once an hour.
    pub async fn flush_pressure(&self) -> Result<(), StoreError> {
        let pressure = &self.state.headroom.pressure;
        let rows: Vec<PressureRow> = pressure.take().into_iter().map(row).collect();
        if !rows.is_empty() {
            self.store.add_pressure(&self.node.id, &rows).await?;
        }
        let now = self.now().0;
        if pressure.trim_due(now, HOUR_US) {
            self.store.expire_pressure(now.saturating_sub(KEEP_US)).await?;
        }
        Ok(())
    }

    /// Every provider's pressure over `[from_us, to_us)`, in buckets of `interval_us` (one bucket
    /// without it), from every node's minutes.
    pub async fn provider_pressure(
        &self,
        from_us: i64,
        to_us: i64,
        interval_us: Option<i64>,
    ) -> Result<Vec<Series>, StoreError> {
        self.flush_pressure().await?;
        let bounds = starts(from_us, to_us, interval_us);
        let first = bounds.first().copied().unwrap_or(from_us);
        let rows = self
            .store
            .pressure_between(first - first.rem_euclid(MINUTE_US), to_us)
            .await?;
        let samples: Vec<Sample> = rows.into_iter().map(sample).collect();
        Ok(series(&samples, &bounds, to_us))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_and_samples_round_trip() {
        let s = Sample {
            provider: "p".into(),
            credential: "c".into(),
            minute_us: MINUTE_US,
            requests: Used {
                used: Some(3),
                limit: Some(10),
            },
            tokens: Used::default(),
        };
        assert_eq!(sample(row(s.clone())), s);
    }
}
