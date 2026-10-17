//! A small deterministic generator, so that every corpus, key and mix is a function of its seed.

/// SplitMix64.
#[derive(Clone, Debug)]
pub struct SplitMix64(u64);

impl SplitMix64 {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// A generator for one named stream under `seed`, independent of every other name.
    #[must_use]
    pub fn named(seed: u64, name: &str) -> Self {
        Self::new(seed ^ fnv1a(name.as_bytes()))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..n` (n > 0), by multiply-shift.
    pub fn below(&mut self, n: u64) -> u64 {
        let wide = u128::from(self.next_u64()) * u128::from(n);
        u64::try_from(wide >> 64).unwrap_or(0)
    }

    /// A value in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        let v = (self.next_u64() >> 11) as f64;
        v / (1u64 << 53) as f64
    }
}

/// FNV-1a, 64 bits.
#[must_use]
pub fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_stream() {
        let mut a = SplitMix64::named(7, "x");
        let mut b = SplitMix64::named(7, "x");
        let mut c = SplitMix64::named(7, "y");
        let xs: Vec<u64> = (0..4).map(|_| a.next_u64()).collect();
        let ys: Vec<u64> = (0..4).map(|_| b.next_u64()).collect();
        let zs: Vec<u64> = (0..4).map(|_| c.next_u64()).collect();
        assert_eq!(xs, ys);
        assert_ne!(xs, zs);
    }

    #[test]
    fn below_stays_in_range() {
        let mut r = SplitMix64::new(1);
        assert!((0..10_000).all(|_| r.below(7) < 7));
        assert!((0..10_000).all(|_| (0.0..1.0).contains(&r.unit())));
    }
}
