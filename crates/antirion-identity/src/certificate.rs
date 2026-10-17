//! Keys bound to a client certificate: the binding a key declares, the facts read from a verified
//! certificate's DER, and the match between them.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use antirion_core::ClientCert;
use sha2::{Digest, Sha256};

/// What a key's `require_client_cert` asks of the request's certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CertBinding {
    /// The SHA-256 of the leaf certificate's DER.
    Fingerprint([u8; 32]),
    /// A subjectAltName entry.
    San(String),
}

impl CertBinding {
    /// Reads `{sha256_fingerprint}` or `{san}`; `None` for anything else.
    #[must_use]
    pub fn parse(v: &serde_json::Value) -> Option<CertBinding> {
        if let Some(f) = v.get("sha256_fingerprint").and_then(serde_json::Value::as_str) {
            return fingerprint(f).map(CertBinding::Fingerprint);
        }
        v.get("san")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
            .map(|s| CertBinding::San(s.to_owned()))
    }

    /// Whether `cert` satisfies the binding. DNS names compare case-insensitively, every other
    /// name exactly.
    #[must_use]
    pub fn matches(&self, cert: &ClientCert) -> bool {
        match self {
            CertBinding::Fingerprint(f) => *f == cert.sha256,
            CertBinding::San(want) => cert
                .names
                .iter()
                .any(|n| n == want || (!n.contains([':', '@']) && n.eq_ignore_ascii_case(want))),
        }
    }
}

/// 64 hexadecimal digits, optionally with a colon between pairs.
fn fingerprint(s: &str) -> Option<[u8; 32]> {
    let hex: String = s.chars().filter(|c| *c != ':').collect();
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// The facts of a certificate in DER: its digest and its subjectAltName entries (none when the
/// DER cannot be read, so a binding by name fails closed).
#[must_use]
pub fn client_cert(der: &[u8]) -> ClientCert {
    ClientCert {
        sha256: Sha256::digest(der).into(),
        names: subject_alt_names(der).unwrap_or_default(),
    }
}

/// One TLV: its tag, its contents and what follows it.
fn tlv(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let n = usize::from(first & 0x7f);
        if n == 0 || n > 4 {
            return None;
        }
        let (bytes, rest) = rest.split_at_checked(n)?;
        (
            bytes.iter().fold(0usize, |acc, b| (acc << 8) | usize::from(*b)),
            rest,
        )
    };
    let (contents, after) = rest.split_at_checked(len)?;
    Some((tag, contents, after))
}

/// The TLVs of a constructed value, in order.
fn members(mut input: &[u8]) -> Option<Vec<(u8, &[u8])>> {
    let mut out = Vec::new();
    while !input.is_empty() {
        let (tag, contents, rest) = tlv(input)?;
        out.push((tag, contents));
        input = rest;
    }
    Some(out)
}

/// `id-ce-subjectAltName`, 2.5.29.17.
const SAN_OID: &[u8] = &[0x55, 0x1d, 0x11];

fn subject_alt_names(der: &[u8]) -> Option<Vec<String>> {
    let (0x30, cert, _) = tlv(der)? else {
        return None;
    };
    let (0x30, tbs, _) = tlv(cert)? else {
        return None;
    };
    let extensions = members(tbs)?
        .into_iter()
        .find(|(tag, _)| *tag == 0xa3)
        .map(|(_, c)| c)?;
    let (0x30, list, _) = tlv(extensions)? else {
        return None;
    };
    for (_, ext) in members(list)? {
        let parts = members(ext)?;
        let Some((0x06, oid)) = parts.first().copied() else {
            continue;
        };
        if oid != SAN_OID {
            continue;
        }
        let (0x04, value) = parts.last().copied()? else {
            return None;
        };
        let (0x30, names, _) = tlv(value)? else {
            return None;
        };
        return Some(
            members(names)?
                .into_iter()
                .filter_map(|(tag, v)| general_name(tag, v))
                .collect(),
        );
    }
    Some(Vec::new())
}

/// The text of a GeneralName of the kinds a binding can name.
fn general_name(tag: u8, v: &[u8]) -> Option<String> {
    match tag {
        0x81 | 0x82 | 0x86 => std::str::from_utf8(v).ok().map(str::to_owned),
        0x87 => match v.len() {
            4 => {
                let b: [u8; 4] = v.try_into().ok()?;
                Some(IpAddr::V4(Ipv4Addr::from(b)).to_string())
            }
            16 => {
                let b: [u8; 16] = v.try_into().ok()?;
                Some(IpAddr::V6(Ipv6Addr::from(b)).to_string())
            }
            _ => None,
        },
        _ => None,
    }
}

/// A certificate in RFC 9440's `Client-Cert` form: a byte sequence, `:<base64 DER>:`.
#[must_use]
pub fn from_header(value: &[u8]) -> Option<Vec<u8>> {
    use base64::Engine;
    let s = std::str::from_utf8(value).ok()?.trim();
    let inner = s.strip_prefix(':')?.strip_suffix(':')?;
    base64::engine::general_purpose::STANDARD.decode(inner).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A certificate with the subjectAltName entries `DNS:svc.acme.test`,
    /// `URI:spiffe://acme/billing`, `IP:10.0.0.7` and `email:ops@acme.test`, made with OpenSSL.
    const CERT: &str = "MIIBnjCCAUSgAwIBAgIUIQiRrNmke/+pYc6D73gX6ZXvuBcwCgYIKoZIzj0EAwIwEjEQMA4GA1UEAwwHYmlsbGluZzAeFw0yNjEwMTUwODU4MjZaFw0zNjEwMTIwODU4MjZaMBIxEDAOBgNVBAMMB2JpbGxpbmcwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAASjSdn9BjNYDdJWQukefyUfuYkp9ccVOi8cdC9KSpWN90lBNErc0kwfo+ixillyRPJbjLSzEmTL2+jI26NKhe+zo3gwdjAdBgNVHQ4EFgQUS2oDeUeaxtvy1bwAwG3hAYI3MsEwDwYDVR0TAQH/BAUwAwEB/zBEBgNVHREEPTA7gg1zdmMuYWNtZS50ZXN0hhVzcGlmZmU6Ly9hY21lL2JpbGxpbmeHBAoAAAeBDW9wc0BhY21lLnRlc3QwCgYIKoZIzj0EAwIDSAAwRQIgGBZNUGKZVmHhC69UNmLxYkc+/nWoJ9qUDwkJZeKlb70CIQCApV52mGYhXnRiTDmDgj4261C8BJv/4fqRGTEAIalJCA==";

    fn der() -> Vec<u8> {
        from_header(format!(":{CERT}:").as_bytes()).unwrap()
    }

    #[test]
    fn names_of_a_certificate() {
        let c = client_cert(&der());
        assert_eq!(
            c.names,
            [
                "svc.acme.test",
                "spiffe://acme/billing",
                "10.0.0.7",
                "ops@acme.test"
            ]
        );
        let by = |s: &str| CertBinding::San(s.to_owned()).matches(&c);
        assert!(by("SVC.acme.test") && by("spiffe://acme/billing") && by("10.0.0.7"));
        assert!(!by("spiffe://ACME/billing") && !by("other.acme.test"));
    }

    #[test]
    fn fingerprints_with_or_without_colons() {
        let c = client_cert(&der());
        let hex: String = c.sha256.iter().map(|b| format!("{b:02X}")).collect();
        let colons = c
            .sha256
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(":");
        for f in [hex, colons] {
            let b = CertBinding::parse(&serde_json::json!({"sha256_fingerprint": f})).unwrap();
            assert!(b.matches(&c));
        }
        assert!(CertBinding::parse(&serde_json::json!({"sha256_fingerprint": "ab"})).is_none());
        assert!(client_cert(b"\x30\x03\x02\x01").names.is_empty());
        assert!(from_header(b"not-a-sequence").is_none());
    }
}
