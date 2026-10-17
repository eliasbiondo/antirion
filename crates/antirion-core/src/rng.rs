//! A small deterministic random number generator for routing choices, so test mode can seed it,
//! and a fast per-thread stream of it for request-scoped randomness (ids, sampling).

use std::cell::Cell;

/// SplitMix64: one 64-bit state word, full period, good statistical quality for selection.
#[derive(Clone, Debug)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    #[must_use]
    pub const fn new(seed: u64) -> SplitMix64 {
        SplitMix64 { state: seed }
    }

    /// Seeded from the operating system's random source.
    #[must_use]
    pub fn from_entropy() -> SplitMix64 {
        SplitMix64::new(os_seed())
    }

    /// Seeded from this thread's fast stream ([`fast_u64`]): no system call.
    #[must_use]
    pub fn from_thread() -> SplitMix64 {
        SplitMix64::new(fast_u64())
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A uniform value in `0..bound`; `bound` must be non-zero, and zero yields zero.
    pub fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            return 0;
        }
        let zone = u64::MAX - (u64::MAX % bound);
        loop {
            let v = self.next_u64();
            if v < zone {
                return v % bound;
            }
        }
    }
}

fn os_seed() -> u64 {
    let mut buf = [0u8; 8];
    match getrandom::fill(&mut buf) {
        Ok(()) => u64::from_le_bytes(buf),
        Err(_) => uuid::Uuid::now_v7().as_u64_pair().1,
    }
}

thread_local! {
    static STREAM: Cell<u64> = Cell::new(os_seed());
}

/// A uniform random value from this thread's stream, seeded once per thread from the operating
/// system. Fast and statistically sound, but predictable from its outputs: keys, secrets, tokens
/// and nonces read the operating system's source instead.
#[must_use]
pub fn fast_u64() -> u64 {
    STREAM.with(|state| {
        let mut r = SplitMix64::new(state.get());
        let v = r.next_u64();
        state.set(r.state);
        v
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_streams_differ_and_advance() {
        let here: Vec<u64> = (0..4).map(|_| fast_u64()).collect();
        let there = std::thread::spawn(|| (0..4).map(|_| fast_u64()).collect::<Vec<u64>>())
            .join()
            .unwrap();
        assert_ne!(here, there);
        assert!(here.windows(2).all(|w| w[0] != w[1]));
        assert_ne!(
            SplitMix64::from_thread().next_u64(),
            SplitMix64::from_thread().next_u64()
        );
    }

    #[test]
    fn same_seed_same_sequence() {
        let mut a = SplitMix64::new(4242);
        let mut b = SplitMix64::new(4242);
        let xs: Vec<u64> = (0..16).map(|_| a.below(3)).collect();
        let ys: Vec<u64> = (0..16).map(|_| b.below(3)).collect();
        assert_eq!(xs, ys);
        assert!(xs.iter().all(|x| *x < 3));
    }

    #[test]
    fn below_covers_the_range() {
        let mut r = SplitMix64::new(1);
        let mut seen = [false; 5];
        for _ in 0..200 {
            seen[usize::try_from(r.below(5)).unwrap()] = true;
        }
        assert!(seen.iter().all(|s| *s));
    }
}
