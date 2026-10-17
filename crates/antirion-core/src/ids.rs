//! Request and resource identifiers.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::rng::fast_u64;
use crate::time::{Clock, SystemClock};

/// The last UUIDv7 time and counter: Unix milliseconds above 12 counter bits.
static LAST_V7: AtomicU64 = AtomicU64::new(0);

/// A UUIDv7 from the fast random stream, strictly increasing on this node: within a millisecond a
/// 12-bit counter (started at a random value below 2048) orders the ids, and its overflow carries
/// into the time field.
fn fast_v7() -> uuid::Uuid {
    let now_ms = u64::try_from(SystemClock.now().0 / 1000).unwrap_or(0) & ((1 << 48) - 1);
    let fresh = (now_ms << 12) | (fast_u64() & 0x7ff);
    let mut prev = LAST_V7.load(Ordering::Relaxed);
    let next = loop {
        let next = if prev >> 12 < now_ms { fresh } else { prev + 1 };
        match LAST_V7.compare_exchange_weak(prev, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break next,
            Err(seen) => prev = seen,
        }
    };
    let [_, _, _, _, _, _, hi, lo] = (next & 0xfff).to_be_bytes();
    let [a, b, c, d, e, f, g, h] = fast_u64().to_be_bytes();
    uuid::Builder::from_unix_timestamp_millis(next >> 12, &[hi, lo, a, b, c, d, e, f, g, h]).into_uuid()
}

/// Client-visible request id: the client's valid `x-request-id`, or a generated UUIDv7.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RequestId(String);

impl RequestId {
    const MAX_LEN: usize = 128;

    /// Accepts the client's value when it is a valid token, otherwise generates a new id.
    #[must_use]
    pub fn from_client(value: Option<&[u8]>) -> RequestId {
        match value {
            Some(v) if Self::is_valid(v) => RequestId(String::from_utf8_lossy(v).into_owned()),
            _ => Self::generate(),
        }
    }

    /// A new UUIDv7, from the fast random stream: unique and ordered, not unguessable.
    #[must_use]
    pub fn generate() -> RequestId {
        RequestId(fast_v7().to_string())
    }

    /// A new UUIDv7 whose random bits come from the operating system, for ids that grant access
    /// to what they name.
    #[must_use]
    pub fn generate_unguessable() -> RequestId {
        RequestId(uuid::Uuid::now_v7().to_string())
    }

    fn is_valid(v: &[u8]) -> bool {
        !v.is_empty()
            && v.len() <= Self::MAX_LEN
            && v.iter()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Stable identity of one incarnation of a configured resource. A delete followed by a re-create
/// gets a new value, so runtime state keyed by it starts fresh.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ResourceUid(pub u128);

impl ResourceUid {
    /// The owner of gateway-level declarations, which is never deleted.
    pub const GATEWAY: ResourceUid = ResourceUid(0);

    #[must_use]
    pub fn random() -> ResourceUid {
        let mut buf = [0u8; 16];
        if getrandom::fill(&mut buf).is_err() {
            return ResourceUid(uuid::Uuid::now_v7().as_u128());
        }
        ResourceUid(u128::from_le_bytes(buf))
    }

    #[must_use]
    pub fn to_hex(self) -> String {
        format!("{:032x}", self.0)
    }

    #[must_use]
    pub fn from_hex(s: &str) -> Option<ResourceUid> {
        u128::from_str_radix(s, 16).ok().map(ResourceUid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_are_v7_and_strictly_increasing() {
        let ids: Vec<uuid::Uuid> = (0..10_000).map(|_| fast_v7()).collect();
        assert!(ids.windows(2).all(|w| w[0] < w[1]));
        assert!(ids.iter().all(|u| u.get_version_num() == 7));
        let threads: Vec<_> = (0..4)
            .map(|_| std::thread::spawn(|| (0..2_000).map(|_| RequestId::generate()).collect::<Vec<_>>()))
            .collect();
        let mut all: Vec<RequestId> = threads.into_iter().flat_map(|t| t.join().unwrap()).collect();
        let n = all.len();
        all.sort_by(|a, b| a.0.cmp(&b.0));
        all.dedup();
        assert_eq!(all.len(), n);
        let now = SystemClock.now().0 / 1_000_000;
        let (secs, _) = ids[0].get_timestamp().unwrap().to_unix();
        assert!((i64::try_from(secs).unwrap() - now).abs() < 5);
        assert_eq!(RequestId::generate_unguessable().as_str().len(), 36);
    }

    #[test]
    fn valid_client_ids_are_kept() {
        assert_eq!(RequestId::from_client(Some(b"req-42.a_b")).as_str(), "req-42.a_b");
    }

    #[test]
    fn invalid_client_ids_are_replaced() {
        for bad in [&b""[..], b"has space", b"semi;colon", &[b'a'; 129]] {
            let id = RequestId::from_client(Some(bad));
            assert_ne!(id.as_str().as_bytes(), bad);
            assert_eq!(id.as_str().len(), 36);
        }
    }

    #[test]
    fn uid_hex_round_trip() {
        let uid = ResourceUid::random();
        assert_eq!(ResourceUid::from_hex(&uid.to_hex()), Some(uid));
    }
}
