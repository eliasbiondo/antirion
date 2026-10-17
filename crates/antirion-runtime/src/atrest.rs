//! Encryption of the secret values the node stores: AES-256-GCM with a random 96-bit nonce per value
//! and the value's location as associated data. The first key encrypts; every key decrypts.

use std::sync::Arc;

use antirion_config::DocPath;
use antirion_config::Seg;
use antirion_config::secrets::has_reference;
use aws_lc_rs::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;

/// The marker of a sealed value; what follows is the base64 of nonce, ciphertext and tag.
pub const PREFIX: &str = "enc:v1:";

/// Why a value could not be sealed or opened.
#[derive(Debug, thiserror::Error)]
pub enum SealError {
    #[error("an encryption key must be 32 bytes")]
    KeyLength,
    #[error("no encryption key is configured")]
    NoKey,
    #[error("no random source")]
    Random,
    #[error("a stored secret at {0} cannot be decrypted with the configured keys")]
    Open(String),
}

fn keys_ids(keys: &[Vec<u8>]) -> Vec<String> {
    keys.iter()
        .map(|k| {
            let digest = <sha2::Sha256 as sha2::Digest>::digest(k);
            digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
        })
        .collect()
}

/// The node's encryption keys.
#[derive(Clone)]
pub struct Sealer {
    keys: Arc<Vec<LessSafeKey>>,
    /// Each key's id: the first 16 hex digits of its SHA-256.
    ids: Arc<Vec<String>>,
    /// How many of the first keys were configured; the others were generated into the data
    /// directory.
    configured: usize,
}

impl std::fmt::Debug for Sealer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sealer").field("keys", &self.keys.len()).finish()
    }
}

impl Sealer {
    pub fn new(keys: &[Vec<u8>]) -> Result<Sealer, SealError> {
        if keys.is_empty() {
            return Err(SealError::NoKey);
        }
        let ids = keys_ids(keys);
        let keys = keys
            .iter()
            .map(|k| {
                UnboundKey::new(&AES_256_GCM, k)
                    .map(LessSafeKey::new)
                    .map_err(|_| SealError::KeyLength)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Sealer {
            keys: Arc::new(keys),
            ids: Arc::new(ids),
            configured: 0,
        })
    }

    /// Records that the first `n` keys are configured ones.
    #[must_use]
    pub fn with_configured(mut self, n: usize) -> Sealer {
        self.configured = n.min(self.ids.len());
        self
    }

    /// The ids of the keys, the encrypting one first, each with whether it was configured; never
    /// the keys.
    #[must_use]
    pub fn key_ids(&self) -> Vec<(&str, bool)> {
        self.ids
            .iter()
            .enumerate()
            .map(|(i, id)| (id.as_str(), i < self.configured))
            .collect()
    }

    /// Encrypts `plain` bound to `location`.
    pub fn seal(&self, location: &str, plain: &[u8]) -> Result<String, SealError> {
        let key = self.keys.first().ok_or(SealError::NoKey)?;
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|_| SealError::Random)?;
        let mut sealed = plain.to_vec();
        key.seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(location.as_bytes()),
            &mut sealed,
        )
        .map_err(|_| SealError::Random)?;
        let mut out = nonce.to_vec();
        out.extend_from_slice(&sealed);
        Ok(format!("{PREFIX}{}", STANDARD.encode(out)))
    }

    /// Decrypts a value sealed for `location` with any of the keys.
    pub fn open(&self, location: &str, sealed: &str) -> Result<Vec<u8>, SealError> {
        let fail = || SealError::Open(location.to_owned());
        let raw = sealed
            .strip_prefix(PREFIX)
            .and_then(|b| STANDARD.decode(b).ok())
            .ok_or_else(fail)?;
        if raw.len() < NONCE_LEN {
            return Err(fail());
        }
        let (nonce, body) = raw.split_at(NONCE_LEN);
        let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| fail())?;
        for key in self.keys.iter() {
            let mut buf = body.to_vec();
            if let Ok(plain) = key.open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(location.as_bytes()),
                &mut buf,
            ) {
                return Ok(plain.to_vec());
            }
        }
        Err(fail())
    }

    /// Opens `stored` when it was sealed; values written before encryption are returned as they are.
    pub fn open_value(&self, location: &str, stored: Vec<u8>) -> Result<Vec<u8>, SealError> {
        match std::str::from_utf8(&stored) {
            Ok(s) if s.starts_with(PREFIX) => self.open(location, s),
            _ => Ok(stored),
        }
    }

    /// The document as it is stored: every literal secret value sealed under its JSON pointer.
    pub fn seal_document(&self, doc: &Value, secret_paths: &[DocPath]) -> Result<Value, SealError> {
        let mut out = doc.clone();
        for path in secret_paths {
            let pointer = pointer(path);
            if let Some(slot) = out.pointer_mut(&pointer)
                && let Value::String(s) = slot
                && !s.is_empty()
                && !has_reference(s)
                && !s.starts_with(PREFIX)
            {
                *slot = Value::String(self.seal(&pointer, s.as_bytes())?);
            }
        }
        Ok(out)
    }

    /// Opens every sealed value of a stored document in place.
    pub fn open_document(&self, doc: &mut Value) -> Result<(), SealError> {
        self.open_at(doc, &mut String::new())
    }

    fn open_at(&self, v: &mut Value, at: &mut String) -> Result<(), SealError> {
        match v {
            Value::String(s) if s.starts_with(PREFIX) => {
                let plain = self.open(at, s)?;
                *s = String::from_utf8(plain).map_err(|_| SealError::Open(at.clone()))?;
            }
            Value::Object(map) => {
                for (k, child) in map.iter_mut() {
                    let len = at.len();
                    at.push('/');
                    at.push_str(&k.replace('~', "~0").replace('/', "~1"));
                    self.open_at(child, at)?;
                    at.truncate(len);
                }
            }
            Value::Array(items) => {
                for (i, child) in items.iter_mut().enumerate() {
                    let len = at.len();
                    at.push('/');
                    at.push_str(&i.to_string());
                    self.open_at(child, at)?;
                    at.truncate(len);
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// The JSON pointer of a document path.
fn pointer(path: &DocPath) -> String {
    let mut out = String::new();
    for seg in path.segments() {
        out.push('/');
        match seg {
            Seg::Key(k) => out.push_str(&k.replace('~', "~0").replace('/', "~1")),
            Seg::Index(i) => out.push_str(&i.to_string()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sealer(byte: u8) -> Sealer {
        Sealer::new(&[vec![byte; 32]]).unwrap()
    }

    #[test]
    fn a_value_opens_only_at_its_location_and_with_a_listed_key() {
        let s = sealer(1);
        let sealed = s.seal("/providers/p/api_key", b"sk-secret").unwrap();
        assert!(sealed.starts_with(PREFIX) && !sealed.contains("sk-secret"));
        assert_ne!(
            sealed,
            s.seal("/providers/p/api_key", b"sk-secret").unwrap(),
            "a nonce per value"
        );
        assert_eq!(s.open("/providers/p/api_key", &sealed).unwrap(), b"sk-secret");
        assert!(s.open("/providers/q/api_key", &sealed).is_err());
        assert!(sealer(2).open("/providers/p/api_key", &sealed).is_err());
        let rotated = Sealer::new(&[vec![2; 32], vec![1; 32]]).unwrap();
        assert_eq!(
            rotated.open("/providers/p/api_key", &sealed).unwrap(),
            b"sk-secret"
        );
    }

    #[test]
    fn documents_seal_literal_secrets_and_keep_references() {
        let s = sealer(7);
        let doc =
            json!({"providers": {"p": {"api_key": "sk-1", "base_url": "u"}, "q": {"api_key": "${env:K}"}}});
        let paths = vec![
            DocPath::root().key("providers").key("p").key("api_key"),
            DocPath::root().key("providers").key("q").key("api_key"),
        ];
        let mut stored = s.seal_document(&doc, &paths).unwrap();
        assert!(!stored.to_string().contains("sk-1"));
        assert_eq!(stored["providers"]["q"]["api_key"], "${env:K}");
        s.open_document(&mut stored).unwrap();
        assert_eq!(stored, doc);
    }

    #[test]
    fn values_stored_before_encryption_are_read_as_they_are() {
        assert_eq!(sealer(3).open_value("/x", b"plain".to_vec()).unwrap(), b"plain");
    }
}
