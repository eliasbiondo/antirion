//! Message encryption for Web Push (RFC 8291) in the `aes128gcm` content coding (RFC 8188): an
//! ephemeral ECDH P-256 key agreed with the browser's key, the browser's auth secret mixed in with
//! HKDF-SHA-256, and one AES-128-GCM record.

use aws_lc_rs::aead::{AES_128_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use aws_lc_rs::agreement::{self, ECDH_P256, PrivateKey, UnparsedPublicKey};
use aws_lc_rs::hkdf::{HKDF_SHA256, KeyType, Salt};

/// The record size every message is sent with; one record holds the whole payload.
const RECORD_SIZE: u32 = 4096;
/// What one record adds to its plaintext: the padding delimiter and the AEAD tag.
const OVERHEAD: usize = 1 + 16;
/// The header: salt, record size, key id length and the 65-byte key id.
const HEADER: usize = 16 + 4 + 1 + 65;

/// The longest payload that fits one record.
pub(crate) const MAX_PAYLOAD: usize = RECORD_SIZE as usize - HEADER - OVERHEAD;

/// Why a message could not be encrypted.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum EceError {
    #[error("the subscription's public key is not an uncompressed P-256 point")]
    PublicKey,
    #[error("the subscription's auth secret is not 16 bytes")]
    AuthSecret,
    #[error("the payload is longer than one record holds")]
    TooLong,
    #[error("the encryption failed")]
    Crypto,
}

struct Len(usize);

impl KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

fn hkdf(salt: &[u8], ikm: &[u8], info: &[&[u8]], out: &mut [u8]) -> Result<(), EceError> {
    let prk = Salt::new(HKDF_SHA256, salt).extract(ikm);
    prk.expand(info, Len(out.len()))
        .and_then(|okm| okm.fill(out))
        .map_err(|_| EceError::Crypto)
}

/// The browser's keys of one subscription.
pub(crate) struct Receiver<'a> {
    /// `p256dh`: the uncompressed P-256 public key.
    pub(crate) public: &'a [u8],
    /// `auth`: the 16-byte secret.
    pub(crate) auth: &'a [u8],
}

/// Checks a subscription's keys before they are stored.
pub(crate) fn check_keys(r: &Receiver<'_>) -> Result<(), EceError> {
    if r.public.len() != 65 || r.public.first() != Some(&4) {
        return Err(EceError::PublicKey);
    }
    if r.auth.len() != 16 {
        return Err(EceError::AuthSecret);
    }
    Ok(())
}

/// Encrypts `payload` for `to` with the sender key `ephemeral` and `salt`, as the `aes128gcm` body of
/// a push message.
pub(crate) fn encrypt_with(
    to: &Receiver<'_>,
    payload: &[u8],
    ephemeral: &PrivateKey,
    salt: &[u8; 16],
) -> Result<Vec<u8>, EceError> {
    check_keys(to)?;
    if payload.len() > MAX_PAYLOAD {
        return Err(EceError::TooLong);
    }
    let sender_public = ephemeral.compute_public_key().map_err(|_| EceError::Crypto)?;
    let sender_public = sender_public.as_ref();
    let secret = agreement::agree(
        ephemeral,
        UnparsedPublicKey::new(&ECDH_P256, to.public),
        EceError::PublicKey,
        |shared| Ok(shared.to_vec()),
    )?;
    let mut ikm = [0u8; 32];
    hkdf(
        to.auth,
        &secret,
        &[b"WebPush: info\0", to.public, sender_public],
        &mut ikm,
    )?;
    let mut cek = [0u8; 16];
    hkdf(salt, &ikm, &[b"Content-Encoding: aes128gcm\0"], &mut cek)?;
    let mut nonce = [0u8; NONCE_LEN];
    hkdf(salt, &ikm, &[b"Content-Encoding: nonce\0"], &mut nonce)?;
    let key = LessSafeKey::new(UnboundKey::new(&AES_128_GCM, &cek).map_err(|_| EceError::Crypto)?);
    let mut record = Vec::with_capacity(payload.len() + OVERHEAD);
    record.extend_from_slice(payload);
    record.push(2);
    key.seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut record)
        .map_err(|_| EceError::Crypto)?;
    let mut out = Vec::with_capacity(HEADER + record.len());
    out.extend_from_slice(salt);
    out.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    out.push(u8::try_from(sender_public.len()).map_err(|_| EceError::Crypto)?);
    out.extend_from_slice(sender_public);
    out.extend_from_slice(&record);
    Ok(out)
}

/// Encrypts `payload` for `to` with a fresh sender key and salt.
pub(crate) fn encrypt(to: &Receiver<'_>, payload: &[u8]) -> Result<Vec<u8>, EceError> {
    let ephemeral = PrivateKey::generate(&ECDH_P256).map_err(|_| EceError::Crypto)?;
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).map_err(|_| EceError::Crypto)?;
    encrypt_with(to, payload, &ephemeral, &salt)
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    use super::*;

    fn b64(s: &str) -> Vec<u8> {
        URL_SAFE_NO_PAD.decode(s).unwrap_or_default()
    }

    /// The worked example of RFC 8291 §5 (its keys, salt and message, with a record size of 4096).
    #[test]
    fn the_rfc_8291_example_encrypts_to_its_record() {
        let ua_public =
            b64("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4");
        let auth = b64("BTBZMqHH6r4Tts7J_aSIgg");
        let as_private = b64("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw");
        let salt: [u8; 16] = b64("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap_or([0; 16]);
        let ephemeral = PrivateKey::from_private_key(&ECDH_P256, &as_private).unwrap();
        let body = encrypt_with(
            &Receiver {
                public: &ua_public,
                auth: &auth,
            },
            b"When I grow up, I want to be a watermelon",
            &ephemeral,
            &salt,
        )
        .unwrap();
        let expected = b64(concat!(
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_",
            "yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
        ));
        assert_eq!(body, expected);
    }

    #[test]
    fn keys_and_length_are_checked() {
        let public = [4u8; 65];
        let auth = [1u8; 16];
        assert_eq!(
            check_keys(&Receiver {
                public: &public[..64],
                auth: &auth
            }),
            Err(EceError::PublicKey)
        );
        assert_eq!(
            check_keys(&Receiver {
                public: &public,
                auth: &auth[..8]
            }),
            Err(EceError::AuthSecret)
        );
        let ua = PrivateKey::generate(&ECDH_P256).unwrap();
        let ua_public = ua.compute_public_key().unwrap();
        let to = Receiver {
            public: ua_public.as_ref(),
            auth: &auth,
        };
        assert_eq!(encrypt(&to, &vec![0; MAX_PAYLOAD + 1]), Err(EceError::TooLong));
        assert_eq!(
            encrypt(&to, &vec![0; MAX_PAYLOAD]).map(|b| b.len()).ok(),
            Some(4096)
        );
    }
}
