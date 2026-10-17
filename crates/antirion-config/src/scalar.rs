//! Scalar formats: durations, sizes and IP ranges.

use std::net::IpAddr;
use std::time::Duration;

const NANOS_PER: [(&str, u128); 5] = [
    ("ms", 1_000_000),
    ("s", 1_000_000_000),
    ("m", 60_000_000_000),
    ("h", 3_600_000_000_000),
    ("d", 86_400_000_000_000),
];

/// Parses `^[0-9]+(\.[0-9]+)?(ms|s|m|h|d)$` into nanoseconds; fractions below 1 ns are truncated.
#[must_use]
pub fn duration_nanos(s: &str) -> Option<u128> {
    let unit_at = s.find(|c: char| c.is_ascii_alphabetic())?;
    let (number, unit) = s.split_at(unit_at);
    let per = NANOS_PER.iter().find(|(u, _)| *u == unit)?.1;
    let (int, frac) = match number.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (number, None),
    };
    if int.is_empty() || !int.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut total = int.parse::<u128>().ok()?.checked_mul(per)?;
    if let Some(frac) = frac {
        if frac.is_empty() || !frac.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let mut scale = per;
        for digit in frac.bytes() {
            scale /= 10;
            total = total.checked_add(u128::from(digit - b'0') * scale)?;
        }
    }
    Some(total)
}

/// Parses a duration string.
#[must_use]
pub fn parse_duration(s: &str) -> Option<Duration> {
    let nanos = duration_nanos(s)?;
    let secs = u64::try_from(nanos / 1_000_000_000).ok()?;
    let sub = u32::try_from(nanos % 1_000_000_000).ok()?;
    Some(Duration::new(secs, sub))
}

/// A whole-second duration in the largest unit that divides it: `30d`, `12h`, `90m`, `45s`.
#[must_use]
pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    for (unit, size) in [("d", 86_400), ("h", 3_600), ("m", 60)] {
        if secs > 0 && secs.is_multiple_of(size) {
            return format!("{}{unit}", secs / size);
        }
    }
    format!("{secs}s")
}

/// Parses `^[0-9]+(B|KiB|MiB|GiB)$` into bytes.
#[must_use]
pub fn parse_size(s: &str) -> Option<u64> {
    let unit_at = s.find(|c: char| !c.is_ascii_digit())?;
    let (number, unit) = s.split_at(unit_at);
    let mult: u64 = match unit {
        "B" => 1,
        "KiB" => 1 << 10,
        "MiB" => 1 << 20,
        "GiB" => 1 << 30,
        _ => return None,
    };
    if number.is_empty() {
        return None;
    }
    number.parse::<u64>().ok()?.checked_mul(mult)
}

/// An IP address or CIDR range with its host bits zero. IPv4-mapped IPv6 addresses compare as IPv4.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Cidr {
    net: IpAddr,
    prefix: u8,
}

/// Why a string is not a [`Cidr`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CidrError {
    Malformed,
    HostBitsSet,
}

fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        IpAddr::V4(_) => ip,
    }
}

fn bits(ip: IpAddr) -> (u128, u8) {
    match ip {
        IpAddr::V4(v4) => (u128::from(u32::from(v4)), 32),
        IpAddr::V6(v6) => (u128::from(v6), 128),
    }
}

fn mask(prefix: u8, width: u8) -> u128 {
    let all = if width == 128 {
        u128::MAX
    } else {
        (1u128 << width) - 1
    };
    if prefix == 0 {
        0
    } else {
        all & !((1u128 << (width - prefix)) - 1)
    }
}

impl Cidr {
    /// Parses `addr` or `addr/prefix`.
    pub fn parse(s: &str) -> Result<Cidr, CidrError> {
        let (addr, prefix) = match s.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (s, None),
        };
        let net: IpAddr = addr.parse().map_err(|_| CidrError::Malformed)?;
        let width = bits(net).1;
        let prefix = match prefix {
            None => width,
            Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => {
                p.parse::<u8>().map_err(|_| CidrError::Malformed)?
            }
            Some(_) => return Err(CidrError::Malformed),
        };
        if prefix > width {
            return Err(CidrError::Malformed);
        }
        if bits(net).0 & !mask(prefix, width) & mask(width, width) != 0 {
            return Err(CidrError::HostBitsSet);
        }
        let (net, prefix) = match (net, canonical(net)) {
            (IpAddr::V6(_), v4 @ IpAddr::V4(_)) if prefix >= 96 => (v4, prefix - 96),
            _ => (net, prefix),
        };
        Ok(Cidr { net, prefix })
    }

    /// Whether `ip` falls inside the range.
    #[must_use]
    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = canonical(ip);
        let (net, width) = bits(self.net);
        let (addr, addr_width) = bits(ip);
        width == addr_width && (addr & mask(self.prefix, width)) == net
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_are_written_in_their_largest_unit() {
        assert_eq!(format_duration(Duration::from_secs(30 * 86_400)), "30d");
        assert_eq!(format_duration(Duration::from_secs(36 * 3_600)), "36h");
        assert_eq!(format_duration(Duration::from_secs(90 * 60)), "90m");
        assert_eq!(format_duration(Duration::from_secs(61)), "61s");
        assert_eq!(format_duration(Duration::ZERO), "0s");
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("250ms"), Some(Duration::from_millis(250)));
        assert_eq!(parse_duration("1.5s"), Some(Duration::from_millis(1500)));
        assert_eq!(parse_duration("5m"), Some(Duration::from_secs(300)));
        assert_eq!(parse_duration("1d"), Some(Duration::from_secs(86_400)));
        for bad in ["10", "1mo", "-1s", "1,5s", "1w", "", "s", ".5s", "1.s"] {
            assert_eq!(parse_duration(bad), None, "{bad}");
        }
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("64KiB"), Some(65_536));
        assert_eq!(parse_size("1MiB"), Some(1_048_576));
        for bad in ["10MB", "1.5MiB", "10", "MiB"] {
            assert_eq!(parse_size(bad), None, "{bad}");
        }
    }

    #[test]
    fn cidrs() {
        let v4 = Cidr::parse("10.0.0.0/8").unwrap();
        assert!(v4.contains("10.1.2.3".parse().unwrap()));
        assert!(v4.contains("::ffff:10.1.2.3".parse().unwrap()));
        assert!(!v4.contains("11.0.0.1".parse().unwrap()));
        assert!(
            Cidr::parse("0.0.0.0/0")
                .unwrap()
                .contains("203.0.113.7".parse().unwrap())
        );
        assert!(
            Cidr::parse("::/0")
                .unwrap()
                .contains("2001:db8::1".parse().unwrap())
        );
        assert!(
            !Cidr::parse("::/0")
                .unwrap()
                .contains("127.0.0.1".parse().unwrap())
        );
        assert!(
            Cidr::parse("127.0.0.1")
                .unwrap()
                .contains("127.0.0.1".parse().unwrap())
        );
        assert!(
            Cidr::parse("::ffff:10.0.0.0/104")
                .unwrap()
                .contains("10.9.9.9".parse().unwrap())
        );
        assert_eq!(Cidr::parse("10.0.0.1/8"), Err(CidrError::HostBitsSet));
        for bad in ["10.0.0.0/33", "x", "10.0.0.0/", "10.0.0.0/a"] {
            assert_eq!(Cidr::parse(bad), Err(CidrError::Malformed), "{bad}");
        }
    }
}
