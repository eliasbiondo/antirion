//! The SSRF validator catalogue: URL rules checked on the raw string before anything canonicalises
//! it, and address rules checked on every address a name resolves to, at connect time.

use std::hash::{Hash, Hasher};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::LazyLock;

use ipnet::{IpNet, Ipv4Net, Ipv6Net};

/// Who chose a destination.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum TrustClass {
    /// Configured by a gateway-scope principal.
    Operator,
    /// Taken from request content, or configured below gateway scope.
    ClientInfluenced,
    /// The credential-acquisition client: the fixed token and metadata endpoints of the cloud
    /// credential sources, or the endpoints an operator names for them. The one class that may reach
    /// cloud metadata hosts.
    Credential,
}

/// The rule that refused a destination.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Denied {
    #[error("the URL scheme is not allowed")]
    Scheme,
    #[error("the URL carries user information")]
    Userinfo,
    #[error("the host is not an ASCII name or an IP literal")]
    Host,
    #[error("the host name or one of its labels is too long")]
    HostLength,
    #[error("the IP literal is not in canonical form")]
    IpLiteral,
    #[error("the port is not allowed")]
    Port,
    #[error("the host is a special-use name")]
    SpecialUse,
    #[error("the host is a cloud metadata name")]
    Metadata,
    #[error("the address is always denied")]
    AlwaysDenied,
    #[error("the address is not public")]
    NonPublic,
}

/// Exemptions from the non-public address rule.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct AllowList {
    pub cidrs: Vec<IpNet>,
    /// Lowercase exact hosts, or `*.suffix` entries.
    pub hosts: Vec<String>,
}

impl AllowList {
    fn host(&self, host: &str) -> bool {
        self.hosts.iter().any(|h| match h.strip_prefix("*.") {
            Some(suffix) => host
                .strip_suffix(suffix)
                .is_some_and(|rest| rest.len() > 1 && rest.ends_with('.')),
            None => h == host,
        })
    }

    fn addr(&self, ip: IpAddr) -> bool {
        self.cidrs.iter().any(|c| c.contains(&ip))
    }
}

/// The compiled `security.egress` settings.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EgressPolicy {
    pub operator_deny_private: bool,
    pub operator_allow: AllowList,
    pub client_ports: Vec<u16>,
    pub client_allow: AllowList,
    /// Extra metadata names (lowercase) and addresses, always denied.
    pub metadata_names: Vec<String>,
    pub metadata_addrs: Vec<IpAddr>,
    /// `security.egress.proxy`: connections are tunnelled through it.
    pub proxy: Option<crate::proxy::Proxy>,
}

impl Default for EgressPolicy {
    fn default() -> EgressPolicy {
        EgressPolicy {
            operator_deny_private: false,
            operator_allow: AllowList::default(),
            client_ports: vec![443],
            client_allow: AllowList::default(),
            metadata_names: Vec::new(),
            metadata_addrs: Vec::new(),
            proxy: None,
        }
    }
}

impl EgressPolicy {
    /// A fingerprint of the settings, so connections made under other settings are never reused.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        let mut h = std::hash::DefaultHasher::new();
        self.hash(&mut h);
        h.finish()
    }
}

/// A URL that passed the URL rules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedUrl {
    pub scheme: String,
    /// Lowercase; an IPv6 literal without its brackets.
    pub host: String,
    pub port: u16,
    pub ip: Option<IpAddr>,
}

const METADATA_NAMES: &[&str] = &[
    "metadata",
    "metadata.google.internal",
    "metadata.goog",
    "metadata.amazonaws.com",
    "instance-data",
    "instance-data.ec2.internal",
];

const SPECIAL_SUFFIXES: &[&str] = &[
    "localhost",
    "local",
    "internal",
    "home.arpa",
    "lan",
    "corp",
    "intranet",
    "svc",
    "cluster.local",
];

/// Applies the URL rules for `class` to `raw`, before any parsing that could canonicalise it.
pub fn validate_url(raw: &str, class: TrustClass, policy: &EgressPolicy) -> Result<ValidatedUrl, Denied> {
    let (scheme, rest) = raw.split_once("://").ok_or(Denied::Scheme)?;
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match (scheme.as_str(), class) {
        ("https", _) => 443,
        ("http", TrustClass::Operator | TrustClass::Credential) => 80,
        _ => return Err(Denied::Scheme),
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = rest.get(..end).unwrap_or_default();
    if authority.contains('@') {
        return Err(Denied::Userinfo);
    }
    if authority.contains('\\') {
        return Err(Denied::Host);
    }
    let (host, port) = split_authority(authority)?;
    let port = match port {
        None => default_port,
        Some(p) => parse_port(p)?,
    };
    if class == TrustClass::ClientInfluenced && !policy.client_ports.contains(&port) {
        return Err(Denied::Port);
    }
    let (host, ip) = match host {
        HostPart::V6(ip) => (ip.to_string(), Some(IpAddr::V6(ip))),
        HostPart::Name(name) => check_name(name)?,
    };
    if class == TrustClass::Credential {
        return Ok(ValidatedUrl {
            scheme,
            host,
            port,
            ip,
        });
    }
    if let Some(ip) = ip {
        if policy.metadata_addrs.contains(&ip) || always_denied(ip) {
            return Err(Denied::AlwaysDenied);
        }
    } else {
        let bare = host.strip_suffix('.').unwrap_or(&host);
        if METADATA_NAMES.contains(&bare) || policy.metadata_names.iter().any(|m| m == bare) {
            return Err(Denied::Metadata);
        }
        if class == TrustClass::ClientInfluenced && special_use(bare) && !policy.client_allow.host(bare) {
            return Err(Denied::SpecialUse);
        }
    }
    Ok(ValidatedUrl {
        scheme,
        host,
        port,
        ip,
    })
}

enum HostPart<'a> {
    V6(Ipv6Addr),
    Name(&'a str),
}

fn split_authority(authority: &str) -> Result<(HostPart<'_>, Option<&str>), Denied> {
    if let Some(inner) = authority.strip_prefix('[') {
        let (literal, after) = inner.split_once(']').ok_or(Denied::IpLiteral)?;
        if literal.contains('%') {
            return Err(Denied::IpLiteral);
        }
        let ip: Ipv6Addr = literal.parse().map_err(|_| Denied::IpLiteral)?;
        let port = match after {
            "" => None,
            p => Some(p.strip_prefix(':').ok_or(Denied::Host)?),
        };
        return Ok((HostPart::V6(ip), port));
    }
    match authority.split_once(':') {
        None => Ok((HostPart::Name(authority), None)),
        Some((_, p)) if p.contains(':') => Err(Denied::IpLiteral),
        Some((h, p)) => Ok((HostPart::Name(h), Some(p))),
    }
}

fn parse_port(p: &str) -> Result<u16, Denied> {
    if p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Denied::Port);
    }
    match p.parse::<u32>() {
        Ok(n @ 1..=65535) => u16::try_from(n).map_err(|_| Denied::Port),
        _ => Err(Denied::Port),
    }
}

/// A label a URL parser would read as part of an IPv4 number: decimal, or `0x` hex.
fn numeric_label(label: &str) -> bool {
    if label.is_empty() {
        return false;
    }
    if let Some(hex) = label.strip_prefix("0x").or_else(|| label.strip_prefix("0X")) {
        return hex.bytes().all(|b| b.is_ascii_hexdigit());
    }
    label.bytes().all(|b| b.is_ascii_digit())
}

fn check_name(raw: &str) -> Result<(String, Option<IpAddr>), Denied> {
    if raw.is_empty() || raw.contains('%') || !raw.is_ascii() {
        return Err(Denied::Host);
    }
    if raw.len() > 253 {
        return Err(Denied::HostLength);
    }
    let host = raw.to_ascii_lowercase();
    let bare = host.strip_suffix('.').unwrap_or(&host);
    let last = bare.rsplit('.').next().unwrap_or_default();
    if numeric_label(last) {
        return canonical_v4(bare)
            .map(|ip| (ip.to_string(), Some(IpAddr::V4(ip))))
            .ok_or(Denied::IpLiteral);
    }
    for label in bare.split('.') {
        if label.is_empty() {
            return Err(Denied::Host);
        }
        if label.len() > 63 {
            return Err(Denied::HostLength);
        }
        let ldh = label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
        if !ldh || label.starts_with('-') || label.ends_with('-') {
            return Err(Denied::Host);
        }
    }
    Ok((host, None))
}

/// Exactly four decimal parts, each 0–255 without leading zeros.
fn canonical_v4(s: &str) -> Option<Ipv4Addr> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut octets = [0u8; 4];
    for (slot, part) in octets.iter_mut().zip(&parts) {
        let ok = !part.is_empty()
            && part.len() <= 3
            && part.bytes().all(|b| b.is_ascii_digit())
            && (part.len() == 1 || !part.starts_with('0'));
        if !ok {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    Some(Ipv4Addr::from(octets))
}

fn special_use(host: &str) -> bool {
    !host.contains('.')
        || SPECIAL_SUFFIXES
            .iter()
            .any(|s| host == *s || host.strip_suffix(s).is_some_and(|rest| rest.ends_with('.')))
}

fn v4(a: u8, b: u8, c: u8, d: u8, len: u8) -> IpNet {
    IpNet::V4(Ipv4Net::new_assert(Ipv4Addr::new(a, b, c, d), len))
}

fn v6(segments: [u16; 8], len: u8) -> IpNet {
    IpNet::V6(Ipv6Net::new_assert(Ipv6Addr::from(segments), len))
}

/// R1: never reachable, whatever the class or the allow-lists.
pub static ALWAYS_DENIED: LazyLock<Vec<IpNet>> = LazyLock::new(|| {
    vec![
        v4(0, 0, 0, 0, 8),
        v4(169, 254, 0, 0, 16),
        v4(100, 100, 100, 200, 32),
        v4(168, 63, 129, 16, 32),
        v4(224, 0, 0, 0, 4),
        v4(240, 0, 0, 0, 4),
        v4(255, 255, 255, 255, 32),
        v6([0, 0, 0, 0, 0, 0, 0, 0], 128),
        v6([0xfe80, 0, 0, 0, 0, 0, 0, 0], 10),
        v6([0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x254], 128),
        v6([0xfd20, 0xce, 0, 0, 0, 0, 0, 0x254], 128),
        v6([0xff00, 0, 0, 0, 0, 0, 0, 0], 8),
    ]
});

/// R2: special-purpose blocks that are not globally reachable.
static NON_PUBLIC: LazyLock<Vec<IpNet>> = LazyLock::new(|| {
    vec![
        v4(127, 0, 0, 0, 8),
        v4(10, 0, 0, 0, 8),
        v4(172, 16, 0, 0, 12),
        v4(192, 168, 0, 0, 16),
        v4(100, 64, 0, 0, 10),
        v4(192, 0, 0, 0, 24),
        v4(192, 0, 2, 0, 24),
        v4(198, 18, 0, 0, 15),
        v4(198, 51, 100, 0, 24),
        v4(203, 0, 113, 0, 24),
        v4(192, 88, 99, 0, 24),
        v6([0, 0, 0, 0, 0, 0, 0, 1], 128),
        v6([0xfc00, 0, 0, 0, 0, 0, 0, 0], 7),
        v6([0xfec0, 0, 0, 0, 0, 0, 0, 0], 10),
        v6([0x2001, 0xdb8, 0, 0, 0, 0, 0, 0], 32),
        v6([0x0100, 0, 0, 0, 0, 0, 0, 0], 64),
        v6([0x2001, 0x2, 0, 0, 0, 0, 0, 0], 48),
        v6([0x2001, 0, 0, 0, 0, 0, 0, 0], 32),
        v6([0x2001, 0x10, 0, 0, 0, 0, 0, 0], 28),
        v6([0x64, 0xff9b, 0x1, 0, 0, 0, 0, 0], 48),
        v6([0x3fff, 0, 0, 0, 0, 0, 0, 0], 20),
        v6([0x5f00, 0, 0, 0, 0, 0, 0, 0], 16),
    ]
});

fn always_denied(ip: IpAddr) -> bool {
    candidates(ip)
        .iter()
        .any(|a| ALWAYS_DENIED.iter().any(|n| n.contains(a)))
}

/// The address itself, plus the IPv4 address embedded in IPv4-mapped, IPv4-translated (SIIT,
/// `::ffff:0:0:0/96`), IPv4-compatible, NAT64 and 6to4 forms.
fn candidates(ip: IpAddr) -> Vec<IpAddr> {
    let mut out = vec![ip];
    if let IpAddr::V6(a) = ip {
        let s = a.segments();
        let o = a.octets();
        let low = |o: &[u8; 16]| IpAddr::V4(Ipv4Addr::new(o[12], o[13], o[14], o[15]));
        let mapped = s[..6] == [0, 0, 0, 0, 0, 0xffff] || s[..6] == [0, 0, 0, 0, 0xffff, 0];
        let compatible = s[..6] == [0; 6] && (s[6] != 0 || s[7] > 1);
        let nat64 = s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] || s[..3] == [0x64, 0xff9b, 1];
        if mapped || compatible || nat64 {
            out.push(low(&o));
        }
        if s[0] == 0x2002 {
            out.push(IpAddr::V4(Ipv4Addr::new(o[2], o[3], o[4], o[5])));
        }
    }
    out
}

/// Applies the address rules to one resolved address of `host`.
pub fn check_addr(ip: IpAddr, host: &str, class: TrustClass, policy: &EgressPolicy) -> Result<(), Denied> {
    if class == TrustClass::Credential {
        return Ok(());
    }
    let all = candidates(ip);
    if all.iter().any(|a| policy.metadata_addrs.contains(a)) || always_denied(ip) {
        return Err(Denied::AlwaysDenied);
    }
    let allow = match class {
        TrustClass::Operator if !policy.operator_deny_private => return Ok(()),
        TrustClass::Operator => &policy.operator_allow,
        TrustClass::ClientInfluenced => &policy.client_allow,
        TrustClass::Credential => return Ok(()),
    };
    let private = all.iter().any(|a| NON_PUBLIC.iter().any(|n| n.contains(a)));
    if private && !allow.addr(ip) && !allow.host(host) {
        return Err(Denied::NonPublic);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(url: &str) -> Result<ValidatedUrl, Denied> {
        validate_url(url, TrustClass::ClientInfluenced, &EgressPolicy::default())
    }

    fn operator(url: &str) -> Result<ValidatedUrl, Denied> {
        validate_url(url, TrustClass::Operator, &EgressPolicy::default())
    }

    #[test]
    fn url_rules_refuse_every_bypass_form() {
        for (url, rule) in [
            ("http://x.example", Denied::Scheme),
            ("file:///etc/passwd", Denied::Scheme),
            ("gopher://x", Denied::Scheme),
            ("https://user:p@x.example", Denied::Userinfo),
            ("https://2130706433/", Denied::IpLiteral),
            ("https://0x7f000001/", Denied::IpLiteral),
            ("https://0177.0.0.1/", Denied::IpLiteral),
            ("https://127.1/", Denied::IpLiteral),
            ("https://0x7f.1/", Denied::IpLiteral),
            ("https://\u{430}pi.example", Denied::Host),
            ("https://ap%69.example", Denied::Host),
            ("https://svc.internal", Denied::SpecialUse),
            ("https://localhost", Denied::SpecialUse),
            ("https://intranet", Denied::SpecialUse),
            ("https://metadata.google.internal", Denied::Metadata),
            ("https://[fe80::1%25eth0]/", Denied::IpLiteral),
            ("https://x.example:70000/", Denied::Port),
            ("https://x.example:8443/", Denied::Port),
            ("https://169.254.169.254/", Denied::AlwaysDenied),
        ] {
            assert_eq!(client(url), Err(rule), "{url}");
        }
        assert_eq!(operator("http://127.1:9100"), Err(Denied::IpLiteral));
        assert_eq!(operator("http://169.254.169.254/"), Err(Denied::AlwaysDenied));
        assert_eq!(operator("http://metadata/"), Err(Denied::Metadata));
    }

    #[test]
    fn url_rules_accept_ordinary_destinations() {
        let ok = client("https://api.example.com/v1?x=1").unwrap();
        assert_eq!((ok.host.as_str(), ok.port), ("api.example.com", 443));
        let lit = operator("http://127.0.0.1:9100/v1").unwrap();
        assert_eq!(lit.ip, Some(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(operator("http://host.docker.internal:9100").is_ok());
        assert!(operator("https://[::1]:8443/").is_ok());
        assert!(client("https://xn--pi-7ka.example/").is_ok());
    }

    #[test]
    fn address_rules() {
        let p = EgressPolicy::default();
        let c = TrustClass::ClientInfluenced;
        let deny = |s: &str| check_addr(s.parse().unwrap(), "h.example", c, &p);
        assert_eq!(deny("169.254.169.254"), Err(Denied::AlwaysDenied));
        assert_eq!(deny("64:ff9b::a9fe:a9fe"), Err(Denied::AlwaysDenied));
        assert_eq!(deny("::ffff:0:a9fe:a9fe"), Err(Denied::AlwaysDenied));
        assert_eq!(deny("::ffff:0:a00:1"), Err(Denied::NonPublic));
        assert_eq!(deny("::ffff:10.0.0.1"), Err(Denied::NonPublic));
        assert_eq!(deny("64:ff9b::a00:1"), Err(Denied::NonPublic));
        assert_eq!(deny("2002:0a00:0001::"), Err(Denied::NonPublic));
        assert_eq!(deny("2001::1"), Err(Denied::NonPublic));
        assert_eq!(deny("127.0.0.1"), Err(Denied::NonPublic));
        assert_eq!(deny("8.8.8.8"), Ok(()));
        let op = |s: &str, p: &EgressPolicy| check_addr(s.parse().unwrap(), "h", TrustClass::Operator, p);
        assert_eq!(op("127.0.0.1", &p), Ok(()));
        assert_eq!(op("fd00:ec2::254", &p), Err(Denied::AlwaysDenied));
        let strict = EgressPolicy {
            operator_deny_private: true,
            operator_allow: AllowList {
                cidrs: vec!["10.0.0.0/8".parse().unwrap()],
                hosts: vec!["*.corp.example".into()],
            },
            ..EgressPolicy::default()
        };
        assert_eq!(op("192.168.1.1", &strict), Err(Denied::NonPublic));
        assert_eq!(op("10.1.2.3", &strict), Ok(()));
        assert_eq!(
            check_addr(
                "192.168.1.1".parse().unwrap(),
                "a.corp.example",
                TrustClass::Operator,
                &strict
            ),
            Ok(())
        );
    }

    #[test]
    fn allow_lists_never_lift_metadata_or_always_deny() {
        let p = EgressPolicy {
            client_allow: AllowList {
                cidrs: vec!["0.0.0.0/0".parse().unwrap()],
                hosts: vec!["metadata.google.internal".into()],
            },
            metadata_names: vec!["meta.example".into()],
            ..EgressPolicy::default()
        };
        let c = TrustClass::ClientInfluenced;
        assert_eq!(
            check_addr("169.254.169.254".parse().unwrap(), "x", c, &p),
            Err(Denied::AlwaysDenied)
        );
        assert_eq!(check_addr("10.0.0.1".parse().unwrap(), "x", c, &p), Ok(()));
        assert_eq!(
            validate_url("https://metadata.google.internal", c, &p),
            Err(Denied::Metadata)
        );
        assert_eq!(validate_url("https://meta.example", c, &p), Err(Denied::Metadata));
    }
}
