//! Secret values in memory. Each lives in memory pages of its own, locked out of swap where the
//! system allows it, is zeroed when dropped, and is never shown by `Debug` or `Display`. It has no
//! `Serialize`: a secret leaves the process only through [`Secret::expose`].

use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};

use zeroize::Zeroize;

static UNLOCKED: AtomicBool = AtomicBool::new(false);

/// A secret string.
pub struct Secret {
    /// Unlocks the pages; dropped before `buf` is freed.
    _lock: Option<region::LockGuard>,
    /// Holds the value in page-aligned pages no other allocation shares.
    buf: Vec<u8>,
    start: usize,
    len: usize,
}

impl Secret {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Secret {
        let mut value: String = value.into();
        let secret = Secret::from_bytes(value.as_bytes());
        value.zeroize();
        secret
    }

    fn from_bytes(bytes: &[u8]) -> Secret {
        let page = region::page::size().max(1);
        let span = bytes.len().div_ceil(page).max(1).saturating_mul(page);
        let mut buf = vec![0u8; span.saturating_add(page)];
        let start = (page - buf.as_ptr().addr() % page) % page;
        if let Some(window) = buf.get_mut(start..start + bytes.len()) {
            window.copy_from_slice(bytes);
        }
        let lock = buf
            .get(start..start + span)
            .and_then(|w| region::lock(w.as_ptr(), span).ok());
        if lock.is_none() {
            UNLOCKED.store(true, Ordering::Relaxed);
        }
        Secret {
            _lock: lock,
            buf,
            start,
            len: bytes.len(),
        }
    }

    /// The value. Keep what is derived from it as short-lived as possible.
    #[must_use]
    pub fn expose(&self) -> &str {
        self.buf
            .get(self.start..self.start + self.len)
            .and_then(|b| std::str::from_utf8(b).ok())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Whether some secret could not be locked into memory (the system's lock limit was reached, or
/// locking is not permitted).
#[must_use]
pub fn some_unlocked() -> bool {
    UNLOCKED.load(Ordering::Relaxed)
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.buf.zeroize();
    }
}

impl Clone for Secret {
    fn clone(&self) -> Secret {
        Secret::from_bytes(self.expose().as_bytes())
    }
}

impl PartialEq for Secret {
    fn eq(&self, other: &Secret) -> bool {
        self.expose() == other.expose()
    }
}

impl Eq for Secret {}

impl Hash for Secret {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.expose().hash(state);
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl From<String> for Secret {
    fn from(s: String) -> Secret {
        Secret::new(s)
    }
}

impl From<&str> for Secret {
    fn from(s: &str) -> Secret {
        Secret::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_is_never_shown() {
        let s = Secret::new("sk-canary-0123456789");
        assert_eq!(format!("{s:?}"), "[REDACTED]");
        assert_eq!(format!("{s}"), "[REDACTED]");
        assert_eq!(s.expose(), "sk-canary-0123456789");
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Holder {
            key: Secret,
        }
        assert!(!format!("{:?}", Holder { key: s.clone() }).contains("canary"));
    }

    #[test]
    fn values_live_in_pages_of_their_own() {
        let page = region::page::size();
        let long = "x".repeat(page + 10);
        for v in ["", "short", long.as_str()] {
            let s = Secret::new(v);
            assert_eq!(s.expose(), v);
            assert_eq!(s.clone(), s);
            assert_eq!((s.buf.as_ptr().addr() + s.start) % page, 0);
            assert!(s.buf.len() - s.start >= v.len().max(1));
        }
    }
}
