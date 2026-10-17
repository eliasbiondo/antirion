//! The identity provider's signing certificates: PEM or the base64 of metadata, decoded to the
//! subject public key that verifies signatures and the facts a status read shows. A certificate is
//! a carrier of the key the operator trusts; its chain and validity are not checked.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use sha2::{Digest, Sha256};

use super::SamlError;

/// One trusted signing certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Certificate {
    /// The DER `SubjectPublicKeyInfo`.
    pub(crate) spki: Vec<u8>,
    /// The subject's common name, when it has one.
    pub subject: Option<String>,
    /// The end of its validity, Unix seconds.
    pub not_after: Option<i64>,
    /// Lowercase hex SHA-256 of the DER certificate.
    pub sha256: String,
}

const SEQUENCE: u8 = 0x30;
const SET: u8 = 0x31;
const OID: u8 = 0x06;
const VERSION: u8 = 0xa0;
const UTC_TIME: u8 = 0x17;
const GENERALIZED_TIME: u8 = 0x18;
const COMMON_NAME: &[u8] = &[0x55, 0x04, 0x03];

/// One tag-length-value: the tag, the whole encoding, the contents and what follows.
type Tlv<'a> = (u8, &'a [u8], &'a [u8], &'a [u8]);

fn tlv(input: &[u8]) -> Option<Tlv<'_>> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, head, rest) = if first < 0x80 {
        (usize::from(first), 2, rest)
    } else {
        let n = usize::from(first & 0x7f);
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        let len = rest
            .get(..n)?
            .iter()
            .fold(0usize, |acc, b| (acc << 8) | usize::from(*b));
        (len, 2 + n, rest.get(n..)?)
    };
    let contents = rest.get(..len)?;
    let whole = input.get(..head + len)?;
    Some((tag, whole, contents, rest.get(len..)?))
}

fn common_name(name: &[u8]) -> Option<String> {
    let mut rdns = name;
    while let Some((SET, _, set, next)) = tlv(rdns) {
        rdns = next;
        let mut attrs = set;
        while let Some((SEQUENCE, _, atv, more)) = tlv(attrs) {
            attrs = more;
            let (OID, _, oid, value) = tlv(atv)? else { continue };
            if oid == COMMON_NAME {
                let (_, _, text, _) = tlv(value)?;
                return std::str::from_utf8(text).ok().map(str::to_owned);
            }
        }
    }
    None
}

fn time(tag: u8, raw: &[u8]) -> Option<i64> {
    let s = std::str::from_utf8(raw).ok()?;
    let (year, rest) = match tag {
        UTC_TIME => {
            let yy: i64 = s.get(..2)?.parse().ok()?;
            (if yy >= 50 { 1900 + yy } else { 2000 + yy }, s.get(2..)?)
        }
        GENERALIZED_TIME => (s.get(..4)?.parse().ok()?, s.get(4..)?),
        _ => return None,
    };
    let field = |i: usize| -> Option<i64> { rest.get(i..i + 2)?.parse().ok() };
    let text = format!(
        "{year:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        field(0)?,
        field(2)?,
        field(4)?,
        field(6)?,
        field(8)?
    );
    antirion_core::UtcMicros::parse_rfc3339(&text)
        .ok()
        .map(|t| t.0 / 1_000_000)
}

/// Reads a DER certificate.
pub(crate) fn from_der(der: &[u8]) -> Result<Certificate, SamlError> {
    let parse = || -> Option<Certificate> {
        let (SEQUENCE, _, cert, _) = tlv(der)? else {
            return None;
        };
        let (SEQUENCE, _, tbs, _) = tlv(cert)? else {
            return None;
        };
        let mut rest = tbs;
        if rest.first() == Some(&VERSION) {
            rest = tlv(rest)?.3;
        }
        let (_, _, _, rest) = tlv(rest)?;
        let (_, _, _, rest) = tlv(rest)?;
        let (_, _, _, rest) = tlv(rest)?;
        let (SEQUENCE, _, validity, rest) = tlv(rest)? else {
            return None;
        };
        let (SEQUENCE, _, subject, rest) = tlv(rest)? else {
            return None;
        };
        let (SEQUENCE, spki, _, _) = tlv(rest)? else {
            return None;
        };
        let (_, _, _, after) = tlv(validity)?;
        let not_after = tlv(after).and_then(|(tag, _, raw, _)| time(tag, raw));
        Some(Certificate {
            spki: spki.to_vec(),
            subject: common_name(subject),
            not_after,
            sha256: Sha256::digest(der)
                .iter()
                .fold(String::with_capacity(64), |mut s, b| {
                    use std::fmt::Write;
                    let _ = write!(s, "{b:02x}");
                    s
                }),
        })
    };
    parse().ok_or(SamlError::Certificate)
}

/// Reads the base64 of a DER certificate, ignoring whitespace.
pub(crate) fn from_base64(text: &str) -> Result<Certificate, SamlError> {
    let compact: String = text.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    let der = STANDARD.decode(compact).map_err(|_| SamlError::Certificate)?;
    from_der(&der)
}

/// Reads every certificate of a PEM text; a text without one is refused.
pub fn from_pem(pem: &str) -> Result<Vec<Certificate>, SamlError> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    let mut out = Vec::new();
    let mut rest = pem;
    while let Some(start) = rest.find(BEGIN) {
        let body = rest.get(start + BEGIN.len()..).unwrap_or_default();
        let end = body.find(END).ok_or(SamlError::Certificate)?;
        out.push(from_base64(body.get(..end).unwrap_or_default())?);
        rest = body.get(end + END.len()..).unwrap_or_default();
    }
    if out.is_empty() {
        return Err(SamlError::Certificate);
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A self-signed RSA certificate for `idp.test.example`, valid from 2026 to 2036.
    pub(crate) const PEM: &str = include_str!("testdata/rsa-cert.pem");

    #[test]
    fn pem_certificates_give_their_key_and_facts() {
        let certs = from_pem(PEM).unwrap();
        assert_eq!(certs.len(), 1);
        assert_eq!(certs[0].subject.as_deref(), Some("idp.test.example"));
        assert_eq!(certs[0].spki.first(), Some(&SEQUENCE));
        assert_eq!(certs[0].not_after, Some(2_082_758_400));
        assert_eq!(certs[0].sha256.len(), 64);
        assert_eq!(from_pem("nothing").unwrap_err(), SamlError::Certificate);
        assert_eq!(from_base64("AAAA").unwrap_err(), SamlError::Certificate);
    }
}
