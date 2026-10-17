//! Test mode: exists only when the process was started with `--test-mode`. Nothing else constructs it,
//! so production code paths cannot reach a test hook.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use antirion_core::{Clock, SplitMix64, SystemClock, UtcMicros};
use parking_lot::Mutex;

/// A clock that runs with real time and can only be moved forward.
#[derive(Debug, Default)]
pub struct TestClock {
    offset_us: AtomicI64,
}

/// A move that would take the clock backwards.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("the test clock only moves forward")]
pub struct Backwards;

impl TestClock {
    pub fn advance(&self, by: Duration) {
        let us = i64::try_from(by.as_micros()).unwrap_or(i64::MAX);
        self.offset_us.fetch_add(us, Ordering::SeqCst);
    }

    /// Moves the clock to `to`, which must not be in its past.
    pub fn set(&self, to: UtcMicros) -> Result<(), Backwards> {
        let now = self.now();
        if to < now {
            return Err(Backwards);
        }
        self.offset_us
            .fetch_add(to.0.saturating_sub(now.0), Ordering::SeqCst);
        Ok(())
    }
}

impl Clock for TestClock {
    fn now(&self) -> UtcMicros {
        UtcMicros(
            SystemClock
                .now()
                .0
                .saturating_add(self.offset_us.load(Ordering::SeqCst)),
        )
    }
}

/// The test-mode capability: the movable clock and the seedable random stream.
#[derive(Debug, Default)]
pub struct TestMode {
    pub clock: Arc<TestClock>,
    seeded: Mutex<Option<SplitMix64>>,
}

impl TestMode {
    #[must_use]
    pub fn new() -> TestMode {
        TestMode::default()
    }

    /// Seeds every later random choice.
    pub fn seed(&self, seed: u64) {
        *self.seeded.lock() = Some(SplitMix64::new(seed));
    }

    /// The seeded stream, when a seed was set. Callers hold it only for one selection.
    pub fn seeded(&self) -> parking_lot::MutexGuard<'_, Option<SplitMix64>> {
        self.seeded.lock()
    }

    pub fn clear_seed(&self) {
        *self.seeded.lock() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_moves_forward_only() {
        let c = TestClock::default();
        let before = c.now();
        c.advance(Duration::from_secs(61));
        assert!(c.now().since(before) >= Duration::from_secs(61));
        assert_eq!(c.set(UtcMicros(0)), Err(Backwards));
        let target = c.now().saturating_add(Duration::from_secs(3600));
        c.set(target).unwrap();
        assert!(c.now() >= target);
    }

    #[test]
    fn seeding_makes_the_stream_repeatable() {
        let t = TestMode::new();
        assert!(t.seeded().is_none());
        t.seed(7);
        let a = t.seeded().as_mut().map(SplitMix64::next_u64);
        t.seed(7);
        assert_eq!(t.seeded().as_mut().map(SplitMix64::next_u64), a);
    }
}
