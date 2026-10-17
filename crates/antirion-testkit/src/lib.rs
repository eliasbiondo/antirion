//! Test support shared by the workspace's tests: a hand-driven clock and scratch directories.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use antirion_core::{Clock, ResourceUid, UtcMicros};

/// A clock that only moves when the test moves it.
#[derive(Debug, Default)]
pub struct FakeClock {
    now_us: AtomicI64,
}

impl FakeClock {
    #[must_use]
    pub fn at(t: UtcMicros) -> FakeClock {
        FakeClock {
            now_us: AtomicI64::new(t.0),
        }
    }

    pub fn advance(&self, by: Duration) {
        self.now_us.fetch_add(
            i64::try_from(by.as_micros()).unwrap_or(i64::MAX),
            Ordering::SeqCst,
        );
    }
}

impl Clock for FakeClock {
    fn now(&self) -> UtcMicros {
        UtcMicros(self.now_us.load(Ordering::SeqCst))
    }
}

/// A fresh directory under the system temporary directory, removed when dropped.
#[derive(Debug)]
pub struct ScratchDir(PathBuf);

impl ScratchDir {
    #[must_use]
    pub fn new(label: &str) -> ScratchDir {
        let dir = std::env::temp_dir().join(format!("antirion-{label}-{}", ResourceUid::random().to_hex()));
        let _ = std::fs::create_dir_all(&dir);
        ScratchDir(dir)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_clock_moves_only_when_told() {
        let c = FakeClock::at(UtcMicros(5));
        assert_eq!(c.now(), UtcMicros(5));
        c.advance(Duration::from_micros(10));
        assert_eq!(c.now(), UtcMicros(15));
    }

    #[test]
    fn scratch_dirs_are_removed() {
        let path = {
            let d = ScratchDir::new("t");
            assert!(d.path().is_dir());
            d.path().to_owned()
        };
        assert!(!path.exists());
    }
}
