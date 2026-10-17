//! One-time codes of the second factor: TOTP (RFC 6238: HMAC-SHA-1, 30-second steps, 6 digits) and
//! recovery codes, with the base32 alphabet of RFC 4648 both use.

use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;
use subtle::ConstantTimeEq;

const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
/// Seconds per TOTP step.
pub const STEP_SECONDS: i64 = 30;
/// Characters of a recovery code.
pub const RECOVERY_CODE_LEN: usize = 10;

/// RFC 4648 base32 without padding.
#[must_use]
pub fn base32(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for &b in bytes {
        buffer = (buffer << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(symbol(buffer >> bits));
        }
    }
    if bits > 0 {
        out.push(symbol(buffer << (5 - bits)));
    }
    out
}

fn symbol(v: u32) -> char {
    let i = usize::try_from(v & 31).unwrap_or(0);
    char::from(ALPHABET.get(i).copied().unwrap_or(b'A'))
}

/// Decodes RFC 4648 base32, ignoring case, padding and spaces.
#[must_use]
pub fn base32_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for c in text.chars().filter(|c| *c != '=' && !c.is_whitespace()) {
        let up = u8::try_from(c.to_ascii_uppercase()).ok()?;
        let v = ALPHABET.iter().position(|&a| a == up)?;
        buffer = (buffer << 5) | u32::try_from(v).ok()?;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((buffer >> bits) & 0xff).ok()?);
        }
    }
    Some(out)
}

/// A fresh 160-bit TOTP secret.
pub fn new_secret() -> Result<Vec<u8>, getrandom::Error> {
    let mut s = vec![0u8; 20];
    getrandom::fill(&mut s)?;
    Ok(s)
}

/// The step a Unix time (seconds) falls in.
#[must_use]
pub fn step_at(unix_seconds: i64) -> i64 {
    unix_seconds.div_euclid(STEP_SECONDS)
}

/// The 6-digit code of one step (RFC 4226 dynamic truncation).
#[must_use]
pub fn code(secret: &[u8], step: i64) -> String {
    let Ok(mut mac) = <Hmac<Sha1> as KeyInit>::new_from_slice(secret) else {
        return String::new();
    };
    mac.update(&step.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = usize::from(digest.last().copied().unwrap_or(0) & 0x0f);
    let word = digest
        .get(offset..offset + 4)
        .and_then(|s| <[u8; 4]>::try_from(s).ok())
        .map_or(0, u32::from_be_bytes);
    format!("{:06}", (word & 0x7fff_ffff) % 1_000_000)
}

/// The step `presented` is the code of, within one step of `now_step`, and later than
/// `last_used_step` so that a code is accepted once.
#[must_use]
pub fn verify(secret: &[u8], presented: &str, now_step: i64, last_used_step: Option<i64>) -> Option<i64> {
    if presented.len() != 6 || !presented.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut found = None;
    for step in [now_step - 1, now_step, now_step + 1] {
        let matches: bool = code(secret, step).as_bytes().ct_eq(presented.as_bytes()).into();
        if matches && last_used_step.is_none_or(|last| step > last) {
            found = Some(step);
        }
    }
    found
}

/// The `otpauth://` URI authenticator apps read from a QR code.
#[must_use]
pub fn otpauth_uri(issuer: &str, account: &str, secret: &[u8]) -> String {
    let enc = |s: &str| {
        let mut out = String::new();
        for b in s.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'@' => {
                    out.push(char::from(b));
                }
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    };
    format!(
        "otpauth://totp/{}:{}?secret={}&issuer={}&algorithm=SHA1&digits=6&period={STEP_SECONDS}",
        enc(issuer),
        enc(account),
        base32(secret),
        enc(issuer)
    )
}

/// `n` fresh recovery codes of [`RECOVERY_CODE_LEN`] base32 characters (50 bits each).
pub fn recovery_codes(n: usize) -> Result<Vec<String>, getrandom::Error> {
    (0..n)
        .map(|_| {
            let mut b = [0u8; 7];
            getrandom::fill(&mut b)?;
            let mut s = base32(&b);
            s.truncate(RECOVERY_CODE_LEN);
            Ok(s)
        })
        .collect()
}

/// A presented recovery code as it was issued: upper case, without spaces or dashes.
#[must_use]
pub fn normalize_recovery_code(code: &str) -> String {
    code.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_matches_rfc4648() {
        assert_eq!(base32(b""), "");
        assert_eq!(base32(b"f"), "MY");
        assert_eq!(base32(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32_decode("mzxw6ytboi======").unwrap(), b"foobar");
        assert!(base32_decode("0").is_none());
    }

    #[test]
    fn rfc6238_sha1_vectors() {
        let secret = b"12345678901234567890";
        for (t, want) in [
            (59_i64, "287082"),
            (1_111_111_109, "081804"),
            (1_234_567_890, "005924"),
            (2_000_000_000, "279037"),
        ] {
            assert_eq!(code(secret, step_at(t)), want, "t={t}");
        }
    }

    #[test]
    fn a_code_is_accepted_once_and_within_one_step() {
        let secret = b"12345678901234567890";
        let now = step_at(1_234_567_890);
        let current = code(secret, now);
        assert_eq!(verify(secret, &current, now, None), Some(now));
        assert_eq!(verify(secret, &current, now, Some(now)), None);
        assert_eq!(verify(secret, &code(secret, now - 1), now, None), Some(now - 1));
        assert_eq!(verify(secret, &code(secret, now - 2), now, None), None);
        assert_eq!(verify(secret, "12345", now, None), None);
    }

    #[test]
    fn recovery_codes_and_uri() {
        let codes = recovery_codes(10).unwrap();
        assert_eq!(codes.len(), 10);
        assert!(codes.iter().all(|c| c.len() == 10 && base32_decode(c).is_some()));
        assert_eq!(normalize_recovery_code("abcde-fgh23 "), "ABCDEFGH23");
        let uri = otpauth_uri("Antirion", "alice@acme.example", b"12345678901234567890");
        assert!(uri.starts_with(
            "otpauth://totp/Antirion:alice@acme.example?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&"
        ));
    }
}
