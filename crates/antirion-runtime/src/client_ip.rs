//! The client IP: the TCP peer, or, when the peer is a trusted proxy, the address its forwarding
//! header names, found by walking the chain from right to left.

use std::net::IpAddr;

use http::{HeaderMap, HeaderName};
use ipnet::IpNet;

/// Which header a trusted proxy reports the client in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum IpHeader {
    #[default]
    XForwardedFor,
    /// RFC 7239 `Forwarded`, its `for=` parameters.
    Forwarded,
    /// A header carrying one address, such as `x-real-ip`.
    Single(HeaderName),
}

/// The compiled `security.client_ip` settings.
#[derive(Clone, Debug, Default)]
pub struct ClientIpRt {
    pub trusted: Vec<IpNet>,
    pub header: IpHeader,
}

/// IPv4-mapped IPv6 addresses become IPv4.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        IpAddr::V4(_) => ip,
    }
}

/// An address as forwarding headers write it: bare, with a port, or bracketed IPv6 with a port.
fn parse_entry(raw: &str) -> Option<IpAddr> {
    let s = raw.trim().trim_matches('"');
    if let Ok(ip) = s.parse::<IpAddr>() {
        return Some(canonical(ip));
    }
    if let Some(inner) = s.strip_prefix('[') {
        let (addr, _) = inner.split_once(']')?;
        return addr.parse::<IpAddr>().ok().map(canonical);
    }
    let (host, port) = s.split_once(':')?;
    if port.bytes().all(|b| b.is_ascii_digit()) && !port.is_empty() {
        return host.parse::<IpAddr>().ok().map(canonical);
    }
    None
}

impl ClientIpRt {
    fn trusts(&self, ip: IpAddr) -> bool {
        self.trusted.iter().any(|n| n.contains(&ip))
    }

    /// Whether a TCP peer is a trusted proxy, whose forwarding headers count.
    #[must_use]
    pub fn trusts_peer(&self, peer: IpAddr) -> bool {
        self.trusts(canonical(peer))
    }

    /// The client IP of a request whose TCP peer is `peer`.
    #[must_use]
    pub fn resolve(&self, peer: IpAddr, headers: &HeaderMap) -> IpAddr {
        let peer = canonical(peer);
        if !self.trusts(peer) {
            return peer;
        }
        let values = |name: &str| -> Vec<String> {
            headers
                .get_all(name)
                .iter()
                .filter_map(|v| v.to_str().ok())
                .flat_map(|v| v.split(','))
                .map(str::to_owned)
                .collect()
        };
        match &self.header {
            IpHeader::XForwardedFor => self.walk(&values("x-forwarded-for"), peer),
            IpHeader::Forwarded => {
                let fors: Vec<String> = values("forwarded")
                    .iter()
                    .map(|element| {
                        element
                            .split(';')
                            .filter_map(|pair| pair.split_once('='))
                            .find(|(k, _)| k.trim().eq_ignore_ascii_case("for"))
                            .map(|(_, v)| v.to_owned())
                            .unwrap_or_default()
                    })
                    .collect();
                self.walk(&fors, peer)
            }
            IpHeader::Single(name) => {
                let mut all = headers.get_all(name).iter();
                match (all.next(), all.next()) {
                    (Some(v), None) => v.to_str().ok().and_then(parse_entry).unwrap_or(peer),
                    _ => peer,
                }
            }
        }
    }

    /// Right to left: the first entry outside the trusted ranges is the client; a malformed entry
    /// stops the walk at the entry to its right; when every entry is trusted, the leftmost wins.
    fn walk(&self, entries: &[String], peer: IpAddr) -> IpAddr {
        let mut right = peer;
        for raw in entries.iter().rev() {
            match parse_entry(raw) {
                None => return right,
                Some(ip) if !self.trusts(ip) => return ip,
                Some(ip) => right = ip,
            }
        }
        right
    }
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;

    use super::*;

    fn rt(trusted: &[&str], header: IpHeader) -> ClientIpRt {
        ClientIpRt {
            trusted: trusted.iter().map(|t| t.parse().unwrap()).collect(),
            header,
        }
    }

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(*k, HeaderValue::from_static(v));
        }
        h
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn untrusted_peers_are_the_client() {
        let none = rt(&[], IpHeader::XForwardedFor);
        let h = headers(&[("x-forwarded-for", "10.1.1.1")]);
        assert_eq!(none.resolve(ip("127.0.0.1"), &h), ip("127.0.0.1"));
        let other = rt(&["192.0.2.0/24"], IpHeader::XForwardedFor);
        assert_eq!(other.resolve(ip("127.0.0.1"), &h), ip("127.0.0.1"));
        assert_eq!(none.resolve(ip("::ffff:127.0.0.1"), &h), ip("127.0.0.1"));
    }

    #[test]
    fn the_chain_is_walked_from_the_right() {
        let r = rt(&["127.0.0.1/32", "10.0.0.0/8"], IpHeader::XForwardedFor);
        let peer = ip("127.0.0.1");
        for (xff, want) in [
            ("198.51.100.7, 10.9.9.9", "198.51.100.7"),
            ("203.0.113.66, 198.51.100.7, 10.9.9.9", "198.51.100.7"),
            ("10.1.1.1, 10.2.2.2", "10.1.1.1"),
            ("198.51.100.7, not-an-ip, 10.9.9.9", "10.9.9.9"),
            ("::ffff:198.51.100.8", "198.51.100.8"),
            ("garbage", "127.0.0.1"),
        ] {
            let h = headers(&[("x-forwarded-for", xff)]);
            assert_eq!(r.resolve(peer, &h), ip(want), "{xff}");
        }
        let joined = headers(&[
            ("x-forwarded-for", "198.51.100.7"),
            ("x-forwarded-for", "10.9.9.9"),
        ]);
        assert_eq!(r.resolve(peer, &joined), ip("198.51.100.7"));
    }

    #[test]
    fn forwarded_and_single_headers() {
        let peer = ip("127.0.0.1");
        let f = rt(&["127.0.0.1/32", "10.0.0.0/8"], IpHeader::Forwarded);
        let h = headers(&[
            ("forwarded", "for=198.51.100.7;proto=https, for=\"10.9.9.9\""),
            ("x-forwarded-for", "203.0.113.9"),
        ]);
        assert_eq!(f.resolve(peer, &h), ip("198.51.100.7"));
        let v6 = headers(&[("forwarded", "for=\"[2001:db8::1]:4711\"")]);
        assert_eq!(f.resolve(peer, &v6), ip("2001:db8::1"));
        let s = rt(
            &["127.0.0.1/32"],
            IpHeader::Single(HeaderName::from_static("x-real-ip")),
        );
        assert_eq!(
            s.resolve(peer, &headers(&[("x-real-ip", "198.51.100.7")])),
            ip("198.51.100.7")
        );
        assert_eq!(s.resolve(peer, &headers(&[("x-real-ip", "999.1.1.1")])), peer);
    }
}
