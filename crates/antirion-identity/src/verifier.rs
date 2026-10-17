//! Peppered verifiers: the store never holds a credential's plaintext, only
//! `hmac-sha256:<pepper_id>:<hex HMAC-SHA256(pepper, credential)>`.

use std::fmt;

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// The secret key every verifier is computed with.
#[derive(Clone)]
pub struct Pepper {
    id: u16,
    /// The HMAC keyed with the pepper once, cloned for each credential.
    keyed: Option<Hmac<Sha256>>,
}

impl fmt::Debug for Pepper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pepper")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Pepper {
    #[must_use]
    pub fn new(id: u16, key: Vec<u8>) -> Pepper {
        Pepper {
            id,
            keyed: <Hmac<Sha256> as KeyInit>::new_from_slice(&key).ok(),
        }
    }

    /// The verifier of a full credential string.
    #[must_use]
    pub fn verifier(&self, credential: &str) -> String {
        let mut out = format!("hmac-sha256:{}:", self.id);
        push_hex(&mut out, &self.mac(credential));
        out
    }

    fn mac(&self, credential: &str) -> [u8; 32] {
        let Some(mut mac) = self.keyed.clone() else {
            return [0u8; 32];
        };
        mac.update(credential.as_bytes());
        mac.finalize().into_bytes().into()
    }

    /// Compares a presented credential with a stored verifier in constant time.
    #[must_use]
    pub fn verify(&self, credential: &str, stored: &str) -> bool {
        let computed = self.verifier(credential);
        computed.as_bytes().ct_eq(stored.as_bytes()).into()
    }
}

fn push_hex(out: &mut String, bytes: &[u8]) {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    out.reserve(bytes.len() * 2);
    let digit = |n: u8| char::from(DIGITS.get(usize::from(n)).copied().unwrap_or(b'0'));
    for b in bytes {
        out.push(digit(b >> 4));
        out.push(digit(b & 0x0f));
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::new();
    push_hex(&mut s, bytes);
    s
}

/// A fresh opaque identifier of 256 random bits, base64url without padding (43 characters).
pub fn random_id() -> Result<String, getrandom::Error> {
    use base64::Engine;
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

/// Lowercase hex SHA-256 of `text`: how a session id is looked up without being stored.
#[must_use]
pub fn digest_hex(text: &str) -> String {
    use sha2::Digest;
    hex(&Sha256::digest(text.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_shape_and_verification() {
        let p = Pepper::new(1, vec![7u8; 32]);
        let v = p.verifier("sk-antirion-abc");
        assert!(v.starts_with("hmac-sha256:1:"));
        assert_eq!(v.len(), "hmac-sha256:1:".len() + 64);
        assert!(p.verify("sk-antirion-abc", &v));
        assert!(!p.verify("sk-antirion-abd", &v));
        let other = Pepper::new(1, vec![8u8; 32]);
        assert!(!other.verify("sk-antirion-abc", &v));
    }

    #[test]
    fn random_ids_and_digests() {
        let a = random_id().unwrap();
        assert_eq!(a.len(), 43);
        assert_ne!(a, random_id().unwrap());
        assert_eq!(
            digest_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn rfc4231_case_2() {
        let p = Pepper::new(0, b"Jefe".to_vec());
        let v = p.verifier("what do ya want for nothing?");
        assert!(v.ends_with("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"));
    }
}
