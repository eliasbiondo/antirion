//! Wall-clock time with microsecond resolution and the gateway clock abstraction.

use std::fmt;
use std::time::Duration;

/// Whether `name` is a time zone of the bundled IANA database.
#[must_use]
pub fn is_time_zone(name: &str) -> bool {
    jiff::tz::TimeZone::get(name).is_ok()
}

/// Microseconds since the Unix epoch, UTC.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct UtcMicros(pub i64);

impl UtcMicros {
    pub const EPOCH: UtcMicros = UtcMicros(0);

    #[must_use]
    pub fn saturating_add(self, d: Duration) -> UtcMicros {
        let micros = i64::try_from(d.as_micros()).unwrap_or(i64::MAX);
        UtcMicros(self.0.saturating_add(micros))
    }

    /// Time elapsed from `earlier` to `self`; zero when `earlier` is later.
    #[must_use]
    pub fn since(self, earlier: UtcMicros) -> Duration {
        let delta = self.0.saturating_sub(earlier.0);
        Duration::from_micros(u64::try_from(delta).unwrap_or(0))
    }

    /// Parses an RFC 3339 timestamp with an offset.
    pub fn parse_rfc3339(s: &str) -> Result<UtcMicros, TimeParseError> {
        let ts: jiff::Timestamp = s.parse().map_err(|_| TimeParseError)?;
        Ok(UtcMicros(ts.as_microsecond()))
    }

    /// RFC 3339 in UTC, with fractional seconds only when they are not zero.
    #[must_use]
    pub fn to_rfc3339(self) -> String {
        match jiff::Timestamp::from_microsecond(self.0) {
            Ok(ts) => ts.to_string(),
            Err(_) => String::from("1970-01-01T00:00:00Z"),
        }
    }

    /// RFC 3339 in UTC with exactly six fractional digits, the form of request-log timestamps.
    #[must_use]
    pub fn to_rfc3339_micros(self) -> String {
        let whole = UtcMicros::to_rfc3339_seconds(self);
        let micros = self.0.rem_euclid(1_000_000);
        match whole.strip_suffix('Z') {
            Some(head) => format!("{head}.{micros:06}Z"),
            None => whole,
        }
    }

    /// RFC 5322 (`Mon, 05 Oct 2026 10:00:00 +0000`), the form of a mail `Date` header.
    #[must_use]
    pub fn to_rfc2822(self) -> String {
        jiff::Timestamp::from_microsecond(self.0)
            .ok()
            .and_then(|ts| jiff::fmt::rfc2822::to_string(&ts.to_zoned(jiff::tz::TimeZone::UTC)).ok())
            .unwrap_or_else(|| String::from("Thu, 01 Jan 1970 00:00:00 +0000"))
    }

    /// An HTTP-date in its IMF-fixdate form (`Sun, 06 Nov 1994 08:49:37 GMT`, RFC 9110).
    #[must_use]
    pub fn to_http_date(self) -> String {
        jiff::Timestamp::from_microsecond(self.0)
            .map(|ts| ts.strftime("%a, %d %b %Y %H:%M:%S GMT").to_string())
            .unwrap_or_else(|_| String::from("Thu, 01 Jan 1970 00:00:00 GMT"))
    }

    /// RFC 3339 in UTC truncated to whole seconds, the form used in resource metadata.
    #[must_use]
    pub fn to_rfc3339_seconds(self) -> String {
        UtcMicros(self.0 - self.0.rem_euclid(1_000_000)).to_rfc3339()
    }
}

impl fmt::Display for UtcMicros {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_rfc3339())
    }
}

#[cfg(test)]
mod http_date_tests {
    use super::UtcMicros;

    #[test]
    fn imf_fixdate() {
        let t = UtcMicros::parse_rfc3339("1994-11-06T08:49:37Z").unwrap();
        assert_eq!(t.to_http_date(), "Sun, 06 Nov 1994 08:49:37 GMT");
    }
}

#[derive(Debug, thiserror::Error)]
#[error("not an RFC 3339 timestamp")]
pub struct TimeParseError;

/// The gateway clock. Everything derived from wall-clock time (rate-limit windows, budget periods,
/// TTLs, expiry) reads it, so test mode can move it. Real I/O timeouts never do.
pub trait Clock: Send + Sync + fmt::Debug + 'static {
    fn now(&self) -> UtcMicros;
}

/// The process's real clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> UtcMicros {
        UtcMicros(jiff::Timestamp::now().as_microsecond())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc2822_dates() {
        let t = UtcMicros::parse_rfc3339("2026-10-05T10:00:00Z").unwrap();
        assert_eq!(t.to_rfc2822(), "Mon, 5 Oct 2026 10:00:00 +0000");
    }

    #[test]
    fn rfc3339_round_trip() {
        let t = UtcMicros::parse_rfc3339("2026-09-30T12:00:00Z").unwrap();
        assert_eq!(t.to_rfc3339(), "2026-09-30T12:00:00Z");
        let frac = UtcMicros(t.0 + 250_000);
        assert_eq!(frac.to_rfc3339(), "2026-09-30T12:00:00.25Z");
        assert_eq!(frac.to_rfc3339_seconds(), "2026-09-30T12:00:00Z");
        assert_eq!(frac.to_rfc3339_micros(), "2026-09-30T12:00:00.250000Z");
        assert_eq!(t.to_rfc3339_micros(), "2026-09-30T12:00:00.000000Z");
    }

    #[test]
    fn offsets_are_normalised_to_utc() {
        let t = UtcMicros::parse_rfc3339("2026-09-30T14:00:00+02:00").unwrap();
        assert_eq!(t.to_rfc3339(), "2026-09-30T12:00:00Z");
    }

    #[test]
    fn since_never_goes_negative() {
        let a = UtcMicros(10);
        let b = UtcMicros(5);
        assert_eq!(a.since(b), Duration::from_micros(5));
        assert_eq!(b.since(a), Duration::ZERO);
    }
}
