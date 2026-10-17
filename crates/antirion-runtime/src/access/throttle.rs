//! The failed-authentication throttle: per client address, the failed authentications within a
//! sliding window. Once `max_failures` fall within it, a further failure from that address is
//! refused as rate limited until the oldest one leaves the window. A valid credential is never
//! affected: only failures consult it.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::time::Duration;

use antirion_core::UtcMicros;
use parking_lot::Mutex;
use serde_json::Value;

/// Addresses tracked at once; a flood of addresses beyond it is not throttled rather than letting
/// the map grow without bound.
const MAX_ADDRESSES: usize = 100_000;

/// `auth.failed_auth_throttle`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThrottleSettings {
    pub max_failures: u32,
    pub window: Duration,
}

impl Default for ThrottleSettings {
    fn default() -> Self {
        ThrottleSettings {
            max_failures: 50,
            window: Duration::from_secs(60),
        }
    }
}

impl ThrottleSettings {
    pub(crate) fn of(v: Option<&Value>) -> ThrottleSettings {
        let d = ThrottleSettings::default();
        let Some(v) = v else {
            return d;
        };
        ThrottleSettings {
            max_failures: v
                .get("max_failures")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(d.max_failures),
            window: v
                .get("window")
                .and_then(Value::as_str)
                .and_then(antirion_config::scalar::parse_duration)
                .filter(|w| !w.is_zero())
                .unwrap_or(d.window),
        }
    }
}

/// The address failures are counted under: IPv4 addresses whole, IPv6 by their /64 prefix (one
/// client usually holds a whole prefix).
fn bucket(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let [a, b, c, d, ..] = v6.segments();
                IpAddr::V6(std::net::Ipv6Addr::new(a, b, c, d, 0, 0, 0, 0))
            }
        },
    }
}

fn micros(d: Duration) -> i64 {
    i64::try_from(d.as_micros()).unwrap_or(i64::MAX)
}

/// Failed authentications per client address.
#[derive(Debug, Default)]
pub struct FailedAuth {
    recent: Mutex<HashMap<IpAddr, VecDeque<i64>>>,
}

impl FailedAuth {
    /// Records a failed authentication from `ip` at `now`. `Some(wait)` when the address already
    /// had `max_failures` within the window: this failure is refused, and `wait` is the time until
    /// the oldest of them leaves it.
    pub fn fail(&self, ip: IpAddr, now: UtcMicros, s: &ThrottleSettings) -> Option<Duration> {
        let window = micros(s.window);
        let since = now.0.saturating_sub(window);
        let max = usize::try_from(s.max_failures).unwrap_or(usize::MAX);
        let mut map = self.recent.lock();
        let ip = bucket(ip);
        if !map.contains_key(&ip) && map.len() >= MAX_ADDRESSES {
            map.retain(|_, list| list.back().is_some_and(|t| *t > since));
            if map.len() >= MAX_ADDRESSES {
                return None;
            }
        }
        let list = map.entry(ip).or_default();
        while list.front().is_some_and(|t| *t <= since) {
            list.pop_front();
        }
        if list.len() >= max {
            let ends = list.front().map_or(now.0, |t| t.saturating_add(window));
            let left = u64::try_from(ends.saturating_sub(now.0)).unwrap_or(0);
            return Some(Duration::from_micros(left));
        }
        list.push_back(now.0);
        None
    }

    pub(crate) fn reset(&self) {
        self.recent.lock().clear();
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_failure_after_max_is_refused_until_the_window_moves() {
        let t = FailedAuth::default();
        let s = ThrottleSettings::of(Some(&json!({"max_failures": 5, "window": "1m"})));
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        let t0 = 1_000_000_000_i64;
        for i in 0..5 {
            assert!(t.fail(ip, UtcMicros(t0 + i), &s).is_none());
        }
        let wait = t.fail(ip, UtcMicros(t0 + 10_000_000), &s).unwrap();
        assert_eq!(wait, Duration::from_micros(50_000_000));
        let other: IpAddr = "10.0.0.2".parse().unwrap();
        assert!(t.fail(other, UtcMicros(t0 + 10_000_000), &s).is_none());
        assert!(t.fail(ip, UtcMicros(t0 + 60_000_001), &s).is_none());
    }

    #[test]
    fn ipv6_counts_per_prefix_and_defaults_apply() {
        let t = FailedAuth::default();
        let s = ThrottleSettings::of(Some(&json!({"max_failures": 1})));
        assert_eq!(s.window, Duration::from_secs(60));
        let a: IpAddr = "2001:db8::1".parse().unwrap();
        let b: IpAddr = "2001:db8::2".parse().unwrap();
        assert!(t.fail(a, UtcMicros(0), &s).is_none());
        assert!(t.fail(b, UtcMicros(1), &s).is_some());
        assert_eq!(ThrottleSettings::of(None).max_failures, 50);
    }
}
