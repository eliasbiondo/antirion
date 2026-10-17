//! The codes of the device authorization grant (RFC 8628): the user code a person types, the
//! device code the client polls with, and the digests they are stored under.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

use crate::keyfmt::uniform_char;

/// Consonants without vowels or look-alikes, so a code never spells a word and reads unambiguously
/// (RFC 8628 §6.1).
pub const USER_CODE_ALPHABET: &[u8] = b"BCDFGHJKLMNPQRSTVWXZ";

/// Letters in a user code.
const USER_CODE_LEN: usize = 8;

/// A new user code: two groups of four letters joined by a dash (`BCDF-GHJK`).
pub fn user_code() -> Result<String, getrandom::Error> {
    let mut out = String::with_capacity(USER_CODE_LEN + 1);
    for i in 0..USER_CODE_LEN {
        if i == USER_CODE_LEN / 2 {
            out.push('-');
        }
        out.push(uniform_char(USER_CODE_ALPHABET)?);
    }
    Ok(out)
}

/// A user code as typed, without dashes, spaces or case; `None` when it cannot be one.
#[must_use]
pub fn normalize_user_code(typed: &str) -> Option<String> {
    let code: String = typed
        .chars()
        .filter(|c| !matches!(c, '-' | ' '))
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (code.len() == USER_CODE_LEN && code.bytes().all(|b| USER_CODE_ALPHABET.contains(&b))).then_some(code)
}

/// The normalized user code written for people.
#[must_use]
pub fn display_user_code(normalized: &str) -> String {
    match normalized.split_at_checked(USER_CODE_LEN / 2) {
        Some((a, b)) => format!("{a}-{b}"),
        None => normalized.to_owned(),
    }
}

/// A new device code: 32 random bytes in base64url.
pub fn device_code() -> Result<String, getrandom::Error> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b)?;
    Ok(URL_SAFE_NO_PAD.encode(b))
}

/// The SHA-256 of a code, in hexadecimal: codes are stored only as digests.
#[must_use]
pub fn digest(code: &str) -> String {
    Sha256::digest(code.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_codes_round_trip() {
        let code = user_code().unwrap();
        assert_eq!(code.len(), 9);
        let n = normalize_user_code(&code.to_ascii_lowercase()).unwrap();
        assert_eq!(display_user_code(&n), code);
        assert_eq!(normalize_user_code("bcdf ghjk").as_deref(), Some("BCDFGHJK"));
        assert!(
            normalize_user_code("ABCD-EFGH").is_none(),
            "vowels are not in the alphabet"
        );
        assert!(normalize_user_code("BCDF").is_none());
    }

    #[test]
    fn device_codes_are_long_and_digests_hex() {
        let c = device_code().unwrap();
        assert_eq!(c.len(), 43);
        assert_ne!(c, device_code().unwrap());
        assert_eq!(digest("x").len(), 64);
    }
}
