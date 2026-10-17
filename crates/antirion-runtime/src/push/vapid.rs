//! Voluntary application server identification (RFC 8292): the gateway's P-256 key pair and the
//! ES256 JWT that each push message carries in `Authorization: vapid t=…, k=…`.

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::json;

/// How long a VAPID token is valid; push services refuse more than 24 hours.
const TOKEN_LIFETIME_SECS: i64 = 12 * 3600;

/// Why the key pair could not be made, read or used.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum VapidError {
    #[error("the key pair could not be generated")]
    Generate,
    #[error("the stored key pair cannot be read")]
    Stored,
    #[error("the token could not be signed")]
    Sign,
}

/// The gateway's application server key.
pub(crate) struct Vapid {
    pair: EcdsaKeyPair,
}

impl std::fmt::Debug for Vapid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vapid").finish_non_exhaustive()
    }
}

impl Vapid {
    /// A new key pair, as the PKCS #8 document it is stored as.
    pub(crate) fn generate() -> Result<Vec<u8>, VapidError> {
        EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &SystemRandom::new())
            .map(|doc| doc.as_ref().to_vec())
            .map_err(|_| VapidError::Generate)
    }

    pub(crate) fn from_pkcs8(pkcs8: &[u8]) -> Result<Vapid, VapidError> {
        EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8)
            .map(|pair| Vapid { pair })
            .map_err(|_| VapidError::Stored)
    }

    /// The public key as browsers take it (`applicationServerKey`): the uncompressed point,
    /// base64url without padding.
    pub(crate) fn public_key(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.pair.public_key().as_ref())
    }

    /// The `Authorization` header value for a message to `endpoint_origin` (`https://host[:port]`),
    /// valid from `now_secs` for twelve hours, naming `subject` (a `mailto:` or `https:` contact)
    /// when there is one.
    pub(crate) fn authorization(
        &self,
        endpoint_origin: &str,
        subject: Option<&str>,
        now_secs: i64,
    ) -> Result<String, VapidError> {
        let header = URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let mut claims = json!({"aud": endpoint_origin, "exp": now_secs + TOKEN_LIFETIME_SECS});
        if let Some(sub) = subject {
            claims["sub"] = json!(sub);
        }
        let claims = URL_SAFE_NO_PAD.encode(claims.to_string());
        let signing_input = format!("{header}.{claims}");
        let signature = self
            .pair
            .sign(&SystemRandom::new(), signing_input.as_bytes())
            .map_err(|_| VapidError::Sign)?;
        let token = format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(signature.as_ref()));
        Ok(format!("vapid t={token}, k={}", self.public_key()))
    }
}

#[cfg(test)]
mod tests {
    use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED, UnparsedPublicKey};

    use super::*;

    #[test]
    fn the_token_is_an_es256_jwt_for_the_push_service_origin() {
        let pkcs8 = Vapid::generate().unwrap();
        let v = Vapid::from_pkcs8(&pkcs8).unwrap();
        let public = URL_SAFE_NO_PAD.decode(v.public_key()).unwrap();
        assert_eq!((public.len(), public.first()), (65, Some(&4)));
        let auth = v
            .authorization("https://push.example.net", Some("mailto:ops@example.com"), 1_000)
            .unwrap();
        let (token, key) = auth
            .strip_prefix("vapid t=")
            .and_then(|r| r.split_once(", k="))
            .unwrap();
        assert_eq!(key, v.public_key());
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["aud"], "https://push.example.net");
        assert_eq!(claims["exp"], 1_000 + TOKEN_LIFETIME_SECS);
        assert_eq!(claims["sub"], "mailto:ops@example.com");
        let signature = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, &public)
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
            .unwrap();
        assert_eq!(Vapid::from_pkcs8(b"not a key").err(), Some(VapidError::Stored));
    }
}
