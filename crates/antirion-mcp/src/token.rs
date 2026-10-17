//! Session tokens and list cursors: authenticated encryption under the session keys, so any node can
//! read them and nobody can forge or read one.
//!
//! Format `a1`: `"a1." || base64url_nopad(key_id || nonce(24) || ciphertext_and_tag)` with
//! XChaCha20-Poly1305 and the additional data `gateway || 0x00 || "a1"`. Cursors use the same
//! construction with additional data `"cursor" || 0x00 || gateway`, so the two cannot be swapped.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{Key, KeyInit, XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::compile::SessionKey;

const PREFIX: &str = "a1.";
const NONCE: usize = 24;

/// The keys of one request: the first encrypts, every one decrypts.
#[derive(Clone, Debug)]
pub(crate) struct Keyring {
    keys: Vec<SessionKey>,
}

impl Keyring {
    pub(crate) fn new(keys: Vec<SessionKey>) -> Option<Keyring> {
        (!keys.is_empty()).then_some(Keyring { keys })
    }

    fn seal_raw(&self, aad: &[u8], plaintext: &[u8]) -> Option<String> {
        let key = self.keys.first()?;
        let cipher = XChaCha20Poly1305::new(Key::from_slice(&key.secret));
        let mut nonce = [0u8; NONCE];
        getrandom::fill(&mut nonce).ok()?;
        let ct = cipher
            .encrypt(XNonce::from_slice(&nonce), Payload { msg: plaintext, aad })
            .ok()?;
        let mut raw = Vec::with_capacity(1 + NONCE + ct.len());
        raw.push(key.id);
        raw.extend_from_slice(&nonce);
        raw.extend_from_slice(&ct);
        Some(URL_SAFE_NO_PAD.encode(raw))
    }

    fn open_raw(&self, aad: &[u8], text: &str) -> Option<Vec<u8>> {
        let raw = URL_SAFE_NO_PAD.decode(text).ok()?;
        let (&id, rest) = raw.split_first()?;
        let nonce = rest.get(..NONCE)?;
        let ct = rest.get(NONCE..)?;
        let key = self.keys.iter().find(|k| k.id == id)?;
        let cipher = XChaCha20Poly1305::new(Key::from_slice(&key.secret));
        cipher
            .decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad })
            .ok()
    }

    pub(crate) fn seal_session(&self, claims: &SessionClaims) -> Option<String> {
        let plain = serde_json::to_vec(claims).ok()?;
        Some(format!(
            "{PREFIX}{}",
            self.seal_raw(&session_aad(&claims.gw), &plain)?
        ))
    }

    /// The claims of a token sealed for `gateway`; `None` for anything else.
    pub(crate) fn open_session(&self, gateway: &str, token: &str) -> Option<SessionClaims> {
        let body = token.strip_prefix(PREFIX)?;
        let plain = self.open_raw(&session_aad(gateway), body)?;
        let claims: SessionClaims = serde_json::from_slice(&plain).ok()?;
        (claims.v == 1 && claims.gw == gateway).then_some(claims)
    }

    pub(crate) fn seal_cursor(&self, gateway: &str, cursor: &CursorClaims) -> Option<String> {
        let plain = serde_json::to_vec(cursor).ok()?;
        self.seal_raw(&cursor_aad(gateway), &plain)
    }

    pub(crate) fn open_cursor(&self, gateway: &str, text: &str) -> Option<CursorClaims> {
        let plain = self.open_raw(&cursor_aad(gateway), text)?;
        serde_json::from_slice(&plain).ok()
    }
}

fn session_aad(gateway: &str) -> Vec<u8> {
    let mut aad = gateway.as_bytes().to_vec();
    aad.push(0);
    aad.extend_from_slice(b"a1");
    aad
}

fn cursor_aad(gateway: &str) -> Vec<u8> {
    let mut aad = b"cursor".to_vec();
    aad.push(0);
    aad.extend_from_slice(gateway.as_bytes());
    aad
}

/// One upstream server of a session.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Upstream {
    pub(crate) s: String,
    pub(crate) sid: Option<String>,
    pub(crate) pv: String,
    #[serde(rename = "gen")]
    pub(crate) generation: u64,
}

/// What a session token carries: everything any node needs to serve the session.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SessionClaims {
    pub(crate) v: u8,
    pub(crate) gw: String,
    pub(crate) sub: String,
    pub(crate) iat: i64,
    pub(crate) exp: i64,
    pub(crate) pv: String,
    pub(crate) caps: Value,
    pub(crate) ci: Value,
    pub(crate) up: Vec<Upstream>,
}

/// A composite list cursor: bound to the gateway, the principal and the list method.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct CursorClaims {
    pub(crate) sub: String,
    pub(crate) m: String,
    pub(crate) o: usize,
}

/// A sealed session token with the keys that open it: decoding and verification measured from
/// outside the crate (the micro-benchmarks), on a token shaped like a multiplexed session's.
#[derive(Debug, Clone)]
pub struct SessionTokenSample {
    keys: Keyring,
    gateway: String,
    token: String,
}

impl SessionTokenSample {
    /// A token for a session multiplexing `upstreams` servers, each with its own session id.
    #[must_use]
    pub fn new(upstreams: usize) -> Option<Self> {
        let keys = Keyring::new(vec![SessionKey {
            id: 1,
            secret: [7; 32],
        }])?;
        let claims = SessionClaims {
            v: 1,
            gw: "g".to_owned(),
            sub: "key:k00000".to_owned(),
            iat: 1_790_000_000,
            exp: 1_790_086_400,
            pv: "2025-06-18".to_owned(),
            caps: serde_json::json!({"roots": {"listChanged": true}, "sampling": {}, "elicitation": {}}),
            ci: serde_json::json!({"name": "client", "version": "1.0.0"}),
            up: (0..upstreams)
                .map(|i| Upstream {
                    s: format!("server-{i}"),
                    sid: Some(format!("upstream-session-{i:032}")),
                    pv: "2025-06-18".to_owned(),
                    generation: 1,
                })
                .collect(),
        };
        let token = keys.seal_session(&claims)?;
        Some(Self {
            keys,
            gateway: claims.gw,
            token,
        })
    }

    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Decodes and verifies `token`; whether it opened.
    #[must_use]
    pub fn open(&self, token: &str) -> bool {
        self.keys.open_session(&self.gateway, token).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring(ids: &[u8]) -> Keyring {
        Keyring::new(
            ids.iter()
                .map(|&id| SessionKey { id, secret: [id; 32] })
                .collect(),
        )
        .unwrap()
    }

    fn claims() -> SessionClaims {
        SessionClaims {
            v: 1,
            gw: "g".into(),
            sub: "k".into(),
            iat: 1,
            exp: 2,
            pv: "2025-06-18".into(),
            caps: Value::Null,
            ci: Value::Null,
            up: vec![Upstream {
                s: "a".into(),
                sid: Some("secret-upstream-sid".into()),
                pv: "2025-06-18".into(),
                generation: 7,
            }],
        }
    }

    #[test]
    fn sample_opens_its_own_token_only() {
        let s = SessionTokenSample::new(2).unwrap();
        assert!(s.open(s.token()));
        assert!(!s.open("a1.AAAA"));
    }

    #[test]
    fn round_trip_and_binding() {
        let r = ring(&[1]);
        let t = r.seal_session(&claims()).unwrap();
        assert!(t.starts_with("a1."));
        assert!(!t.contains("secret-upstream-sid"));
        assert_eq!(r.open_session("g", &t).unwrap().up[0].generation, 7);
        assert!(r.open_session("other", &t).is_none());
    }

    #[test]
    fn any_flipped_character_fails() {
        let r = ring(&[1]);
        let t = r.seal_session(&claims()).unwrap();
        for i in 3..t.len() {
            let mut b = t.clone().into_bytes();
            b[i] = if b[i] == b'A' { b'B' } else { b'A' };
            let forged = String::from_utf8(b).unwrap();
            if forged != t {
                assert!(r.open_session("g", &forged).is_none(), "position {i}");
            }
        }
    }

    #[test]
    fn rotation() {
        let old = ring(&[1]);
        let t = old.seal_session(&claims()).unwrap();
        assert!(ring(&[2, 1]).open_session("g", &t).is_some());
        assert!(ring(&[2]).open_session("g", &t).is_none());
    }

    #[test]
    fn cursors_and_sessions_do_not_mix() {
        let r = ring(&[3]);
        let c = r
            .seal_cursor(
                "g",
                &CursorClaims {
                    sub: "k".into(),
                    m: "tools/list".into(),
                    o: 4,
                },
            )
            .unwrap();
        assert_eq!(r.open_cursor("g", &c).unwrap().o, 4);
        assert!(r.open_cursor("h", &c).is_none());
        assert!(r.open_session("g", &format!("a1.{c}")).is_none());
    }
}
