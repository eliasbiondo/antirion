//! The facts the encryption report shows of an X.509 certificate, read from its DER: the subject's
//! and issuer's common names, the DNS names of its subject alternative names and its validity.
//! Only what the report needs is decoded; nothing here verifies a certificate.

use antirion_core::UtcMicros;

/// What the report shows of one certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CertFacts {
    pub(crate) subject: Option<String>,
    pub(crate) issuer: Option<String>,
    pub(crate) dns_names: Vec<String>,
    pub(crate) not_before: UtcMicros,
    pub(crate) not_after: UtcMicros,
}

const SEQUENCE: u8 = 0x30;
const SET: u8 = 0x31;
const OID: u8 = 0x06;
const UTC_TIME: u8 = 0x17;
const GENERALIZED_TIME: u8 = 0x18;
/// `[0]` (the version) and `[3]` (the extensions) of a TBSCertificate, both explicit.
const VERSION: u8 = 0xa0;
const EXTENSIONS: u8 = 0xa3;
/// `dNSName [2] IA5String` of a GeneralName.
const DNS_NAME: u8 = 0x82;
/// 2.5.4.3, commonName.
const COMMON_NAME: &[u8] = &[0x55, 0x04, 0x03];
/// 2.5.29.17, subjectAltName.
const SUBJECT_ALT_NAME: &[u8] = &[0x55, 0x1d, 0x11];

/// One tag-length-value and what follows it.
fn tlv(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let n = usize::from(first & 0x7f);
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        let (bytes, rest) = rest.split_at(n);
        let len = bytes.iter().fold(0usize, |acc, b| (acc << 8) | usize::from(*b));
        (len, rest)
    };
    if rest.len() < len {
        return None;
    }
    let (value, rest) = rest.split_at(len);
    Some((tag, value, rest))
}

/// The members of a constructed value.
fn members(mut input: &[u8]) -> Option<Vec<(u8, &[u8])>> {
    let mut out = Vec::new();
    while !input.is_empty() {
        let (tag, value, rest) = tlv(input)?;
        out.push((tag, value));
        input = rest;
    }
    Some(out)
}

fn expect(input: &[u8], tag: u8) -> Option<&[u8]> {
    let (t, value, _) = tlv(input)?;
    (t == tag).then_some(value)
}

/// The common name of a Name, when it has one.
fn common_name(name: &[u8]) -> Option<String> {
    for (tag, rdn) in members(name)? {
        if tag != SET {
            continue;
        }
        for (_, attribute) in members(rdn)? {
            let parts = members(attribute)?;
            if let [(OID, oid), (_, value)] = parts.as_slice()
                && *oid == COMMON_NAME
            {
                return String::from_utf8(value.to_vec()).ok();
            }
        }
    }
    None
}

fn digits(s: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(s).ok()?;
    text.bytes().all(|b| b.is_ascii_digit()).then_some(text)
}

/// A UTCTime or GeneralizedTime in its DER form (`…Z`).
fn time(tag: u8, value: &[u8]) -> Option<UtcMicros> {
    let body = value.strip_suffix(b"Z")?;
    let (year, rest) = match tag {
        UTC_TIME => {
            let yy: u32 = digits(body.get(..2)?)?.parse().ok()?;
            (if yy >= 50 { 1900 + yy } else { 2000 + yy }, body.get(2..)?)
        }
        GENERALIZED_TIME => (digits(body.get(..4)?)?.parse().ok()?, body.get(4..)?),
        _ => return None,
    };
    let rest = digits(rest)?;
    if rest.len() != 10 {
        return None;
    }
    let part = |i: usize| rest.get(i..i + 2);
    let text = format!(
        "{year:04}-{}-{}T{}:{}:{}Z",
        part(0)?,
        part(2)?,
        part(4)?,
        part(6)?,
        part(8)?
    );
    UtcMicros::parse_rfc3339(&text).ok()
}

fn dns_names(extensions: &[u8]) -> Option<Vec<String>> {
    let list = expect(extensions, SEQUENCE)?;
    for (_, ext) in members(list)? {
        let parts = members(ext)?;
        let Some((OID, oid)) = parts.first().copied() else {
            continue;
        };
        if oid != SUBJECT_ALT_NAME {
            continue;
        }
        let (_, octets) = parts.last().copied()?;
        let names = expect(octets, SEQUENCE)?;
        return Some(
            members(names)?
                .into_iter()
                .filter(|(t, _)| *t == DNS_NAME)
                .filter_map(|(_, v)| String::from_utf8(v.to_vec()).ok())
                .collect(),
        );
    }
    Some(Vec::new())
}

/// The facts of a DER certificate; `None` when it is not one.
pub(crate) fn read(der: &[u8]) -> Option<CertFacts> {
    let cert = expect(der, SEQUENCE)?;
    let tbs = expect(cert, SEQUENCE)?;
    let mut fields = members(tbs)?.into_iter();
    let mut next = fields.next()?;
    if next.0 == VERSION {
        next = fields.next()?;
    }
    let _serial = next;
    let _signature = fields.next()?;
    let (_, issuer) = fields.next()?;
    let (_, validity) = fields.next()?;
    let (_, subject) = fields.next()?;
    let times = members(validity)?;
    let [(t1, v1), (t2, v2)] = times.as_slice() else {
        return None;
    };
    let dns = fields
        .find(|(t, _)| *t == EXTENSIONS)
        .and_then(|(_, e)| dns_names(e))
        .unwrap_or_default();
    Some(CertFacts {
        subject: common_name(subject),
        issuer: common_name(issuer),
        dns_names: dns,
        not_before: time(*t1, v1)?,
        not_after: time(*t2, v2)?,
    })
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;

    use super::*;

    const PEM_BODY: &str = "MIIBljCCATygAwIBAgIUQcRVs7N3dXgJIGNQf0KYqdxuCNcwCgYIKoZIzj0EAwIw\
GjEYMBYGA1UEAwwPZ3cuYWNtZS5leGFtcGxlMB4XDTI2MTAxNDE2MDA0NloXDTI2\
MTExMzE2MDA0NlowGjEYMBYGA1UEAwwPZ3cuYWNtZS5leGFtcGxlMFkwEwYHKoZI\
zj0CAQYIKoZIzj0DAQcDQgAEUuCO0TL5fbk9Ka/JywmwrfAn+lgQpW6qC35TtAvW\
6eayawQ6NPFVfejSAAAY5DhQXFhnd9I4/n6oLvbYA/JQjKNgMF4wHQYDVR0OBBYE\
FEjth2e8F2WrqUCy5yRvoEW1i4PfMA8GA1UdEwEB/wQFMAMBAf8wLAYDVR0RBCUw\
I4IPZ3cuYWNtZS5leGFtcGxlghBhbHQuYWNtZS5leGFtcGxlMAoGCCqGSM49BAMC\
A0gAMEUCID6HWH39/k9lAq7ZX8HXC5ck4OYULQ42B53MprqYNKpHAiEArNxn5u8B\
x9B7zeTyeCZwGf2oN1BCIhFk19vCz9EFhyI=";

    #[test]
    fn a_certificate_reads_its_names_and_validity() {
        let der = base64::engine::general_purpose::STANDARD
            .decode(PEM_BODY)
            .unwrap();
        let facts = read(&der).unwrap();
        assert_eq!(facts.subject.as_deref(), Some("gw.acme.example"));
        assert_eq!(facts.issuer.as_deref(), Some("gw.acme.example"));
        assert_eq!(facts.dns_names, vec!["gw.acme.example", "alt.acme.example"]);
        assert_eq!(facts.not_before.to_rfc3339(), "2026-10-14T16:00:46Z");
        assert_eq!(facts.not_after.to_rfc3339(), "2026-11-13T16:00:46Z");
    }

    #[test]
    fn anything_else_is_refused() {
        assert!(read(b"").is_none());
        assert!(read(&[0x30, 0x82, 0xff]).is_none());
        assert!(read(&[0x30, 0x00]).is_none());
        assert_eq!(
            time(UTC_TIME, b"491231235959Z").unwrap().to_rfc3339(),
            "2049-12-31T23:59:59Z"
        );
        assert_eq!(
            time(UTC_TIME, b"500101000000Z").unwrap().to_rfc3339(),
            "1950-01-01T00:00:00Z"
        );
        assert!(time(UTC_TIME, b"5001010000Z").is_none());
    }
}
