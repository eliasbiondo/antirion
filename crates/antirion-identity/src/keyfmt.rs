//! The credential format shared by virtual keys (`sk-antirion-…`) and admin tokens (`ak-antirion-…`):
//! a random 12-character id, a 43-character secret and a 6-character CRC-32 checksum in base62.

use std::fmt;

const BASE62: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const KID_ALPHABET: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";
const KID_LEN: usize = 12;
const SECRET_LEN: usize = 43;
const CHECK_LEN: usize = 6;

/// Which plane a credential belongs to, from its prefix.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Plane {
    /// `sk-antirion-`: a virtual key, data plane only.
    Data,
    /// `ak-antirion-`: an admin token, admin plane only.
    Admin,
}

impl Plane {
    const fn prefix(self) -> &'static str {
        match self {
            Plane::Data => "sk-antirion-",
            Plane::Admin => "ak-antirion-",
        }
    }
}

/// A well-formed credential: the grammar holds and the checksum matches.
#[derive(Clone, PartialEq, Eq)]
pub struct Credential {
    plane: Plane,
    full: String,
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credential")
            .field("plane", &self.plane)
            .field("hint", &self.hint())
            .finish()
    }
}

impl Credential {
    /// Generates a new credential from the operating system's random source.
    pub fn generate(plane: Plane) -> Result<Credential, getrandom::Error> {
        let mut kid = String::with_capacity(KID_LEN);
        while kid.len() < KID_LEN {
            kid.push(uniform_char(KID_ALPHABET)?);
        }
        let mut secret = String::with_capacity(SECRET_LEN);
        while secret.len() < SECRET_LEN {
            secret.push(uniform_char(BASE62)?);
        }
        let body = format!("{}{kid}-{secret}", plane.prefix());
        let full = format!("{body}{}", checksum(&body));
        Ok(Credential { plane, full })
    }

    /// Parses `s`; `None` unless it matches the grammar and its checksum.
    #[must_use]
    pub fn parse(s: &str) -> Option<Credential> {
        let plane = if s.starts_with(Plane::Data.prefix()) {
            Plane::Data
        } else if s.starts_with(Plane::Admin.prefix()) {
            Plane::Admin
        } else {
            return None;
        };
        let rest = s.get(plane.prefix().len()..)?;
        let (kid, tail) = rest.split_once('-')?;
        let kid_ok =
            kid.len() == KID_LEN && kid.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
        let tail_ok = tail.len() == SECRET_LEN + CHECK_LEN && tail.bytes().all(|b| b.is_ascii_alphanumeric());
        if !kid_ok || !tail_ok {
            return None;
        }
        let (body, check) = s.split_at(s.len() - CHECK_LEN);
        (checksum(body) == check).then(|| Credential {
            plane,
            full: s.to_owned(),
        })
    }

    #[must_use]
    pub fn plane(&self) -> Plane {
        self.plane
    }

    /// The key id, used to look the credential up.
    #[must_use]
    pub fn kid(&self) -> &str {
        let start = self.plane.prefix().len();
        self.full.get(start..start + KID_LEN).unwrap_or_default()
    }

    /// The plaintext; shown once, at creation.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.full
    }

    /// `sk-antirion-<kid>-…<last 4 characters>`, safe to show on every read path.
    #[must_use]
    pub fn hint(&self) -> String {
        let last4 = self
            .full
            .get(self.full.len().saturating_sub(4)..)
            .unwrap_or_default();
        format!("{}{}-…{last4}", self.plane.prefix(), self.kid())
    }
}

/// A character of `alphabet` drawn uniformly (rejection sampling over one random byte).
pub(crate) fn uniform_char(alphabet: &[u8]) -> Result<char, getrandom::Error> {
    let len = u8::try_from(alphabet.len()).unwrap_or(u8::MAX);
    let zone = u8::MAX - (u8::MAX % len);
    loop {
        let mut b = [0u8; 1];
        getrandom::fill(&mut b)?;
        if b[0] < zone {
            return Ok(char::from(
                alphabet.get(usize::from(b[0] % len)).copied().unwrap_or(b'0'),
            ));
        }
    }
}

/// base62 of CRC-32/IEEE of `body`, left-padded with `0` to 6 characters.
#[must_use]
pub fn checksum(body: &str) -> String {
    let mut n = crc32(body.as_bytes());
    let mut out = [b'0'; CHECK_LEN];
    for slot in out.iter_mut().rev() {
        *slot = BASE62
            .get(usize::try_from(n % 62).unwrap_or(0))
            .copied()
            .unwrap_or(b'0');
        n /= 62;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// CRC-32/IEEE (reflected, polynomial 0xEDB88320).
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_known_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn generated_keys_parse_and_have_the_documented_shape() {
        let k = Credential::generate(Plane::Data).unwrap();
        assert_eq!(k.expose().len(), 74);
        let parsed = Credential::parse(k.expose()).unwrap();
        assert_eq!(parsed.kid(), k.kid());
        assert_eq!(parsed.plane(), Plane::Data);
        assert!(k.hint().starts_with("sk-antirion-") && k.hint().contains('…'));
        let a = Credential::generate(Plane::Admin).unwrap();
        assert_eq!(Credential::parse(a.expose()).unwrap().plane(), Plane::Admin);
    }

    #[test]
    fn a_wrong_checksum_or_shape_is_rejected() {
        let k = Credential::generate(Plane::Data).unwrap();
        let s = k.expose();
        let last = s.chars().last().unwrap();
        let bad = format!("{}{}", &s[..s.len() - 1], if last == '0' { '1' } else { '0' });
        assert!(Credential::parse(&bad).is_none());
        assert!(Credential::parse("sk-antirion-short").is_none());
        assert!(Credential::parse("Bearer x").is_none());
    }

    #[test]
    fn checksum_is_base62_of_crc32_left_padded() {
        assert_eq!(checksum(""), "000000");
        let body = format!("sk-antirion-abcdefghijkl-{}", "A".repeat(43));
        assert_eq!(checksum(&body), "0xAEA7");
        assert!(Credential::parse(&format!("{body}0xAEA7")).is_some());
    }
}
