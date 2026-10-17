//! The rollups of the records settled since the last write: per grain, period start and dimension
//! values, the summed measures and the latency, first-token and throughput sketches.

use std::collections::HashMap;

use antirion_obs::sketch::Sketch;
use antirion_store::logs::RollupRow;

use super::facts::{Facts, N};

/// The periods rollups are kept at, in seconds.
pub const GRAINS: [i64; 3] = [60, 3_600, 86_400];

/// Measures and sketches of one group of records.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Agg {
    pub counts: [i64; N],
    pub latency: Sketch,
    pub ttft: Sketch,
    pub otps: Sketch,
}

impl Agg {
    pub fn add(&mut self, f: &Facts) {
        for (a, b) in self.counts.iter_mut().zip(f.counts) {
            *a = a.saturating_add(b);
        }
        self.latency.add(f.latency_ms);
        if let Some(t) = f.ttft_ms {
            self.ttft.add(t);
        }
        if let Some(o) = f.otps {
            self.otps.add(o);
        }
    }

    pub fn merge(&mut self, other: &Agg) {
        for (a, b) in self.counts.iter_mut().zip(other.counts) {
            *a = a.saturating_add(b);
        }
        self.latency.merge(&other.latency);
        self.ttft.merge(&other.ttft);
        self.otps.merge(&other.otps);
    }

    /// The measures and sketches of a stored row.
    #[must_use]
    pub fn of_row(row: &RollupRow) -> Agg {
        let mut counts = [0i64; N];
        for (a, b) in counts.iter_mut().zip(&row.counts) {
            *a = *b;
        }
        let sketch = |i: usize| row.sketches.get(i).map(|b| Sketch::decode(b)).unwrap_or_default();
        Agg {
            counts,
            latency: sketch(0),
            ttft: sketch(1),
            otps: sketch(2),
        }
    }
}

/// Merges two encoded sketches (the store's merge function for rollup rows).
#[must_use]
pub fn merge_encoded(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut s = Sketch::decode(a);
    s.merge(&Sketch::decode(b));
    s.encode()
}

type Key = (i64, i64, Vec<Option<String>>);

/// Rollups not yet written.
#[derive(Debug, Default)]
pub(crate) struct Pending {
    groups: HashMap<Key, Agg>,
}

/// The start of the `grain_s` period that contains `t_us`.
#[must_use]
pub fn period_start(t_us: i64, grain_s: i64) -> i64 {
    let g = grain_s.saturating_mul(1_000_000).max(1);
    t_us.div_euclid(g) * g
}

impl Pending {
    pub(crate) fn add(&mut self, f: &Facts) {
        for grain in GRAINS {
            let key = (grain, period_start(f.started_us, grain), f.dims.clone());
            self.groups.entry(key).or_default().add(f);
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    pub(crate) fn clear(&mut self) {
        self.groups.clear();
    }

    /// Takes the pending rollups as store rows of `node_id`.
    pub(crate) fn drain(&mut self, node_id: &str) -> Vec<RollupRow> {
        self.groups
            .drain()
            .map(|((grain_s, start_us, dims), a)| RollupRow {
                grain_s,
                start_us,
                node_id: node_id.to_owned(),
                dims,
                counts: a.counts.to_vec(),
                sketches: vec![a.latency.encode(), a.ttft.encode(), a.otps.encode()],
            })
            .collect()
    }

    /// Puts back rows a failed write took.
    pub(crate) fn restore(&mut self, rows: Vec<RollupRow>) {
        for r in rows {
            let a = Agg::of_row(&r);
            self.groups
                .entry((r.grain_s, r.start_us, r.dims))
                .or_default()
                .merge(&a);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_roll_up_into_every_grain() {
        let mut p = Pending::default();
        let mut f = Facts {
            started_us: 3_600_000_000 + 61_000_000,
            dims: vec![Some("m".into())],
            latency_ms: 120.0,
            ..Facts::default()
        };
        f.counts[0] = 1;
        p.add(&f);
        p.add(&f);
        let rows = p.drain("n");
        assert_eq!(rows.len(), 3);
        let minute = rows.iter().find(|r| r.grain_s == 60).unwrap();
        assert_eq!(minute.start_us, 3_660_000_000);
        assert_eq!(minute.counts[0], 2);
        assert_eq!(Sketch::decode(&minute.sketches[0]).count(), 2);
        assert_eq!(rows.iter().find(|r| r.grain_s == 86_400).unwrap().start_us, 0);
        assert!(p.is_empty());
        p.restore(rows);
        assert_eq!(p.drain("n").len(), 3);
        assert_eq!(period_start(-1, 60), -60_000_000);
    }
}
