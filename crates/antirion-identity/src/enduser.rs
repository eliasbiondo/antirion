//! End-user identity: the `x-antirion-end-user` header grammar and the normalisation of body values.

use sha2::{Digest, Sha256};

const MAX_LEN: usize = 256;

/// The end user named by an `x-antirion-end-user` value: 1–256 characters from `%x21-7E`.
#[must_use]
pub fn from_header(value: &[u8]) -> Option<String> {
    let valid = (1..=MAX_LEN).contains(&value.len()) && value.iter().all(|b| (0x21..=0x7e).contains(b));
    valid.then(|| String::from_utf8_lossy(value).into_owned())
}

/// The end user named by a body field: used as is when it is 1–256 bytes without control characters,
/// otherwise replaced by `sha256:<hex>` of its bytes. An empty string names no one.
#[must_use]
pub fn from_body(value: &str) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    if value.len() <= MAX_LEN && !value.chars().any(char::is_control) {
        return Some(value.to_owned());
    }
    let digest = Sha256::digest(value.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    Some(format!("sha256:{hex}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_grammar() {
        assert_eq!(from_header(b"u1").as_deref(), Some("u1"));
        assert!(from_header(b"has space").is_none());
        assert!(from_header(b"").is_none());
        assert!(from_header("café".as_bytes()).is_none());
        assert!(from_header(&[b'x'; 257]).is_none());
        assert!(from_header(&[b'x'; 256]).is_some());
    }

    #[test]
    fn body_values_are_bounded() {
        assert_eq!(from_body("bob").as_deref(), Some("bob"));
        assert_eq!(from_body(""), None);
        let long = "x".repeat(300);
        let hashed = from_body(&long).unwrap();
        assert!(hashed.starts_with("sha256:") && hashed.len() == 7 + 64);
        assert!(from_body("bob\u{1}evil").unwrap().starts_with("sha256:"));
    }
}
