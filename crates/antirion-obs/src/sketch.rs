//! A mergeable quantile sketch with a relative-error guarantee (the logarithmic bucketing of
//! DDSketch): every value lands in the bucket `ceil(log_γ v)`, and a quantile is answered with the
//! bucket's midpoint, which is within [`RELATIVE_ACCURACY`] of the true value at that rank. Merging
//! adds bucket counts, so the sketch of a wide interval is exactly the merge of its parts.

use std::collections::BTreeMap;

/// The relative error bound of every quantile.
pub const RELATIVE_ACCURACY: f64 = 0.01;

/// Values at or below this are counted as zero (a latency below a microsecond).
const MIN_VALUE: f64 = 1e-3;

/// The current encoding's leading byte.
const VERSION: u8 = 1;

fn gamma() -> f64 {
    (1.0 + RELATIVE_ACCURACY) / (1.0 - RELATIVE_ACCURACY)
}

/// A quantile sketch over non-negative values.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sketch {
    zeros: u64,
    bins: BTreeMap<i32, u64>,
    count: u64,
    min: f64,
    max: f64,
}

impl Sketch {
    #[must_use]
    pub fn new() -> Sketch {
        Sketch::default()
    }

    #[must_use]
    pub fn count(&self) -> u64 {
        self.count
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The smallest value added, when any was.
    #[must_use]
    pub fn min(&self) -> Option<f64> {
        (self.count > 0).then_some(self.min)
    }

    /// The largest value added, when any was.
    #[must_use]
    pub fn max(&self) -> Option<f64> {
        (self.count > 0).then_some(self.max)
    }

    fn index(v: f64) -> i32 {
        let i = (v.ln() / gamma().ln()).ceil();
        if i >= f64::from(i32::MAX) {
            i32::MAX
        } else if i <= f64::from(i32::MIN) {
            i32::MIN
        } else {
            i as i32
        }
    }

    fn value_of(index: i32) -> f64 {
        let g = gamma();
        2.0 * g.powi(index) / (g + 1.0)
    }

    /// Adds one value; negative and non-finite values are ignored.
    pub fn add(&mut self, v: f64) {
        if !v.is_finite() || v < 0.0 {
            return;
        }
        if v <= MIN_VALUE {
            self.zeros += 1;
        } else {
            *self.bins.entry(Self::index(v)).or_insert(0) += 1;
        }
        self.extend_range(v, v);
        self.count += 1;
    }

    fn extend_range(&mut self, lo: f64, hi: f64) {
        if self.count == 0 {
            self.min = lo;
            self.max = hi;
        } else {
            self.min = self.min.min(lo);
            self.max = self.max.max(hi);
        }
    }

    pub fn merge(&mut self, other: &Sketch) {
        if other.count == 0 {
            return;
        }
        self.extend_range(other.min, other.max);
        self.zeros += other.zeros;
        for (i, n) in &other.bins {
            *self.bins.entry(*i).or_insert(0) += n;
        }
        self.count += other.count;
    }

    /// The value at rank `q · (count − 1)` (lower nearest rank), within the relative accuracy;
    /// `None` for an empty sketch.
    #[must_use]
    pub fn quantile(&self, q: f64) -> Option<f64> {
        if self.count == 0 {
            return None;
        }
        let q = q.clamp(0.0, 1.0);
        let rank = (q * (self.count - 1) as f64).floor() as u64;
        if rank < self.zeros {
            return Some(self.min.max(0.0));
        }
        let mut seen = self.zeros;
        for (i, n) in &self.bins {
            seen += n;
            if seen > rank {
                return Some(Self::value_of(*i).clamp(self.min, self.max));
            }
        }
        Some(self.max)
    }

    /// The buckets as `(representative value, count)`, in increasing order, zeros first.
    pub fn buckets(&self) -> impl Iterator<Item = (f64, u64)> + '_ {
        let zero = (self.zeros > 0).then_some((self.min.max(0.0), self.zeros));
        zero.into_iter().chain(
            self.bins
                .iter()
                .map(|(i, n)| (Self::value_of(*i).clamp(self.min, self.max), *n)),
        )
    }

    /// The compact binary form the store keeps.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(16 + self.bins.len() * 3);
        out.push(VERSION);
        out.extend_from_slice(&self.min.to_le_bytes());
        out.extend_from_slice(&self.max.to_le_bytes());
        put_varint(&mut out, self.zeros);
        put_varint(&mut out, self.bins.len() as u64);
        let mut prev = 0i64;
        for (i, n) in &self.bins {
            let i = i64::from(*i);
            put_varint(&mut out, zigzag(i - prev));
            put_varint(&mut out, *n);
            prev = i;
        }
        out
    }

    /// Reads [`encode`](Self::encode)'s form; an empty or unreadable input is an empty sketch.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Sketch {
        Self::try_decode(bytes).unwrap_or_default()
    }

    fn try_decode(bytes: &[u8]) -> Option<Sketch> {
        let (&version, rest) = bytes.split_first()?;
        if version != VERSION {
            return None;
        }
        let (min, rest) = rest.split_at_checked(8)?;
        let (max, mut rest) = rest.split_at_checked(8)?;
        let mut s = Sketch {
            min: f64::from_le_bytes(min.try_into().ok()?),
            max: f64::from_le_bytes(max.try_into().ok()?),
            zeros: take_varint(&mut rest)?,
            ..Sketch::default()
        };
        let n = take_varint(&mut rest)?;
        let mut prev = 0i64;
        for _ in 0..n {
            let i = prev + unzigzag(take_varint(&mut rest)?);
            let c = take_varint(&mut rest)?;
            s.bins.insert(i32::try_from(i).ok()?, c);
            prev = i;
        }
        s.count = s.zeros + s.bins.values().sum::<u64>();
        Some(s)
    }
}

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn take_varint(rest: &mut &[u8]) -> Option<u64> {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let (&b, tail) = rest.split_first()?;
        *rest = tail;
        if shift > 63 {
            return None;
        }
        v |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Some(v);
        }
        shift += 7;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exact(sorted: &[f64], q: f64) -> f64 {
        sorted[(q * (sorted.len() - 1) as f64).floor() as usize]
    }

    #[test]
    fn quantiles_are_within_the_relative_accuracy() {
        let mut values: Vec<f64> = (1..=5000).map(|i| f64::from(i).powf(1.7) / 3.0).collect();
        let mut s = Sketch::new();
        for v in &values {
            s.add(*v);
        }
        values.sort_by(f64::total_cmp);
        for q in [0.0, 0.5, 0.9, 0.95, 0.99, 1.0] {
            let got = s.quantile(q).unwrap();
            let want = exact(&values, q);
            assert!(
                (got - want).abs() <= want * RELATIVE_ACCURACY,
                "q={q}: {got} vs {want}"
            );
        }
        assert_eq!(s.count(), 5000);
    }

    #[test]
    fn merging_parts_equals_the_whole() {
        let mut whole = Sketch::new();
        let mut a = Sketch::new();
        let mut b = Sketch::new();
        for i in 0..1000 {
            let v = f64::from(i % 97) * 13.5;
            whole.add(v);
            if i % 3 == 0 { a.add(v) } else { b.add(v) }
        }
        a.merge(&b);
        assert_eq!(a, whole);
        assert_eq!(a.quantile(0.95), whole.quantile(0.95));
    }

    #[test]
    fn encoding_round_trips() {
        let mut s = Sketch::new();
        for v in [0.0, 0.5, 1.0, 250.0, 250.1, 9_000.0, 1e7] {
            s.add(v);
        }
        let back = Sketch::decode(&s.encode());
        assert_eq!(back, s);
        assert_eq!(Sketch::decode(&[]), Sketch::new());
        assert_eq!(Sketch::decode(&[9, 1, 2]), Sketch::new());
        assert_eq!(Sketch::new().quantile(0.5), None);
    }

    #[test]
    fn extremes_are_exact_and_buckets_cover_the_count() {
        let mut s = Sketch::new();
        for v in [3.0, 4.0, 1000.0] {
            s.add(v);
        }
        assert_eq!(s.quantile(0.0), Some(3.0));
        assert_eq!(s.quantile(1.0), Some(1000.0));
        assert_eq!(s.buckets().map(|(_, n)| n).sum::<u64>(), 3);
        s.add(f64::NAN);
        s.add(-1.0);
        assert_eq!(s.count(), 3);
    }
}
