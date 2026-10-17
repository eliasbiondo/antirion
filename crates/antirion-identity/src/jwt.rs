//! JWT bearer tokens: compact JWS parsing, JWKS keys, signature verification, the claim checks and
//! the mapping of a valid token to a key. No I/O: the caller supplies the JWKS and the time.

use aws_lc_rs::signature::{
    self, ECDSA_P256_SHA256_FIXED, ECDSA_P384_SHA384_FIXED, ED25519, RSA_PKCS1_2048_8192_SHA256,
    RSA_PKCS1_2048_8192_SHA384, RSA_PKCS1_2048_8192_SHA512, RSA_PSS_2048_8192_SHA256,
    RSA_PSS_2048_8192_SHA384, RSA_PSS_2048_8192_SHA512, RsaPublicKeyComponents, UnparsedPublicKey,
    VerificationAlgorithm,
};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Map, Value};
use sha2::{Sha256, Sha384, Sha512};

/// The largest token accepted.
pub const MAX_TOKEN: usize = 16 * 1024;

/// The signature algorithms the gateway knows.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Alg {
    Rs256,
    Rs384,
    Rs512,
    Ps256,
    Ps384,
    Ps512,
    Es256,
    Es384,
    EdDsa,
    Hs256,
    Hs384,
    Hs512,
}

impl Alg {
    #[must_use]
    pub fn parse(s: &str) -> Option<Alg> {
        Some(match s {
            "RS256" => Alg::Rs256,
            "RS384" => Alg::Rs384,
            "RS512" => Alg::Rs512,
            "PS256" => Alg::Ps256,
            "PS384" => Alg::Ps384,
            "PS512" => Alg::Ps512,
            "ES256" => Alg::Es256,
            "ES384" => Alg::Es384,
            "EdDSA" => Alg::EdDsa,
            "HS256" => Alg::Hs256,
            "HS384" => Alg::Hs384,
            "HS512" => Alg::Hs512,
            _ => return None,
        })
    }

    /// The default list: every asymmetric algorithm.
    pub const DEFAULT: [Alg; 9] = [
        Alg::Rs256,
        Alg::Rs384,
        Alg::Rs512,
        Alg::Ps256,
        Alg::Ps384,
        Alg::Ps512,
        Alg::Es256,
        Alg::Es384,
        Alg::EdDsa,
    ];

    #[must_use]
    pub fn hmac(self) -> bool {
        matches!(self, Alg::Hs256 | Alg::Hs384 | Alg::Hs512)
    }
}

/// Which check a token failed; the message names it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    Malformed,
    Algorithm,
    Signature,
    Issuer,
    Audience,
    Expired,
    NotYetValid,
    /// A `kid` the JWKS does not hold.
    UnknownKey,
}

impl Failure {
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Failure::Malformed => "The token is malformed.",
            Failure::Algorithm => "The token's algorithm is not accepted.",
            Failure::Signature | Failure::UnknownKey => "The token's signature does not verify.",
            Failure::Issuer => "The token's issuer is not configured.",
            Failure::Audience => "The token's audience is not accepted.",
            Failure::Expired => "The token has expired or its lifetime is not accepted.",
            Failure::NotYetValid => "The token is not yet valid.",
        }
    }
}

/// One public key of a JWKS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Jwk {
    Rsa {
        kid: Option<String>,
        n: Vec<u8>,
        e: Vec<u8>,
    },
    Ec {
        kid: Option<String>,
        curve: Curve,
        point: Vec<u8>,
    },
    Ed25519 {
        kid: Option<String>,
        x: Vec<u8>,
    },
    /// A key of a type or curve the gateway does not verify with (an X448 OKP key, a symmetric key).
    Other {
        kid: Option<String>,
    },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Curve {
    P256,
    P384,
}

impl Jwk {
    #[must_use]
    pub fn kid(&self) -> Option<&str> {
        match self {
            Jwk::Rsa { kid, .. } | Jwk::Ec { kid, .. } | Jwk::Ed25519 { kid, .. } | Jwk::Other { kid } => {
                kid.as_deref()
            }
        }
    }

    fn compatible(&self, alg: Alg) -> bool {
        matches!(
            (self, alg),
            (
                Jwk::Rsa { .. },
                Alg::Rs256 | Alg::Rs384 | Alg::Rs512 | Alg::Ps256 | Alg::Ps384 | Alg::Ps512,
            ) | (
                Jwk::Ec {
                    curve: Curve::P256,
                    ..
                },
                Alg::Es256
            ) | (
                Jwk::Ec {
                    curve: Curve::P384,
                    ..
                },
                Alg::Es384
            ) | (Jwk::Ed25519 { .. }, Alg::EdDsa)
        )
    }
}

fn b64(v: Option<&Value>) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(v?.as_str()?.trim_end_matches('=')).ok()
}

/// The keys of a JWKS document; keys that cannot be read are skipped.
#[must_use]
pub fn parse_jwks(doc: &Value) -> Vec<Jwk> {
    let mut out = Vec::new();
    for k in doc.get("keys").and_then(Value::as_array).into_iter().flatten() {
        let kid = k.get("kid").and_then(Value::as_str).map(str::to_owned);
        let key = match (
            k.get("kty").and_then(Value::as_str),
            k.get("crv").and_then(Value::as_str),
        ) {
            (Some("RSA"), _) => match (b64(k.get("n")), b64(k.get("e"))) {
                (Some(n), Some(e)) => Jwk::Rsa { kid, n, e },
                _ => continue,
            },
            (Some("EC"), Some(crv @ ("P-256" | "P-384"))) => match (b64(k.get("x")), b64(k.get("y"))) {
                (Some(x), Some(y)) => {
                    let mut point = vec![4u8];
                    point.extend(x);
                    point.extend(y);
                    let curve = if crv == "P-256" { Curve::P256 } else { Curve::P384 };
                    Jwk::Ec { kid, curve, point }
                }
                _ => continue,
            },
            (Some("OKP"), Some("Ed25519")) => match b64(k.get("x")) {
                Some(x) => Jwk::Ed25519 { kid, x },
                None => continue,
            },
            _ => Jwk::Other { kid },
        };
        out.push(key);
    }
    out
}

/// A token split into its parts, before verification.
#[derive(Clone, Debug)]
pub struct Unverified {
    pub alg: Option<Alg>,
    pub alg_name: String,
    pub kid: Option<String>,
    pub claims: Map<String, Value>,
    signing_input: String,
    signature: Vec<u8>,
}

/// Splits and decodes a compact JWS.
pub fn decode(token: &str) -> Result<Unverified, Failure> {
    if token.len() > MAX_TOKEN {
        return Err(Failure::Malformed);
    }
    let mut parts = token.split('.');
    let (Some(h), Some(c), Some(s), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err(Failure::Malformed);
    };
    let header: Value = URL_SAFE_NO_PAD
        .decode(h)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or(Failure::Malformed)?;
    let claims: Value = URL_SAFE_NO_PAD
        .decode(c)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or(Failure::Malformed)?;
    let Value::Object(claims) = claims else {
        return Err(Failure::Malformed);
    };
    let alg_name = header
        .get("alg")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Ok(Unverified {
        alg: Alg::parse(&alg_name),
        alg_name,
        kid: header.get("kid").and_then(Value::as_str).map(str::to_owned),
        claims,
        signing_input: format!("{h}.{c}"),
        signature: URL_SAFE_NO_PAD.decode(s).map_err(|_| Failure::Malformed)?,
    })
}

/// What a provider checks a token against.
#[derive(Clone, Debug, Default)]
pub struct Rules {
    pub issuer: String,
    pub audiences: Vec<String>,
    pub algorithms: Vec<Alg>,
    pub hmac_secret: Option<Vec<u8>>,
    pub clock_skew: i64,
    pub max_lifetime: Option<i64>,
}

fn hmac_ok(alg: Alg, secret: &[u8], input: &[u8], sig: &[u8]) -> bool {
    fn check<M: Mac + KeyInit>(secret: &[u8], input: &[u8], sig: &[u8]) -> bool {
        match <M as KeyInit>::new_from_slice(secret) {
            Ok(mut m) => {
                m.update(input);
                m.verify_slice(sig).is_ok()
            }
            Err(_) => false,
        }
    }
    match alg {
        Alg::Hs256 => check::<Hmac<Sha256>>(secret, input, sig),
        Alg::Hs384 => check::<Hmac<Sha384>>(secret, input, sig),
        Alg::Hs512 => check::<Hmac<Sha512>>(secret, input, sig),
        _ => false,
    }
}

fn asymmetric_ok(alg: Alg, key: &Jwk, input: &[u8], sig: &[u8]) -> bool {
    let rsa = |a: &'static signature::RsaParameters, n: &[u8], e: &[u8]| {
        RsaPublicKeyComponents { n, e }.verify(a, input, sig).is_ok()
    };
    let raw = |a: &'static dyn VerificationAlgorithm, k: &[u8]| {
        UnparsedPublicKey::new(a, k).verify(input, sig).is_ok()
    };
    match (key, alg) {
        (Jwk::Rsa { n, e, .. }, Alg::Rs256) => rsa(&RSA_PKCS1_2048_8192_SHA256, n, e),
        (Jwk::Rsa { n, e, .. }, Alg::Rs384) => rsa(&RSA_PKCS1_2048_8192_SHA384, n, e),
        (Jwk::Rsa { n, e, .. }, Alg::Rs512) => rsa(&RSA_PKCS1_2048_8192_SHA512, n, e),
        (Jwk::Rsa { n, e, .. }, Alg::Ps256) => rsa(&RSA_PSS_2048_8192_SHA256, n, e),
        (Jwk::Rsa { n, e, .. }, Alg::Ps384) => rsa(&RSA_PSS_2048_8192_SHA384, n, e),
        (Jwk::Rsa { n, e, .. }, Alg::Ps512) => rsa(&RSA_PSS_2048_8192_SHA512, n, e),
        (Jwk::Ec { point, .. }, Alg::Es256) => raw(&ECDSA_P256_SHA256_FIXED, point),
        (Jwk::Ec { point, .. }, Alg::Es384) => raw(&ECDSA_P384_SHA384_FIXED, point),
        (Jwk::Ed25519 { x, .. }, Alg::EdDsa) => raw(&ED25519, x),
        _ => false,
    }
}

/// The token's algorithm, when the rules accept it: only `HS*` with an HMAC secret, else only the
/// listed asymmetric algorithms.
pub fn check_alg(t: &Unverified, rules: &Rules) -> Result<Alg, Failure> {
    let alg = t.alg.ok_or(Failure::Algorithm)?;
    let allowed = if rules.hmac_secret.is_some() {
        alg.hmac()
    } else {
        !alg.hmac() && rules.algorithms.contains(&alg)
    };
    if allowed { Ok(alg) } else { Err(Failure::Algorithm) }
}

/// Checks the algorithm and the signature with `keys`.
pub fn verify_signature(t: &Unverified, rules: &Rules, keys: &[Jwk]) -> Result<(), Failure> {
    let alg = check_alg(t, rules)?;
    let input = t.signing_input.as_bytes();
    if let Some(secret) = &rules.hmac_secret {
        return if hmac_ok(alg, secret, input, &t.signature) {
            Ok(())
        } else {
            Err(Failure::Signature)
        };
    }
    let key = match &t.kid {
        Some(kid) => {
            let k = keys
                .iter()
                .find(|k| k.kid() == Some(kid.as_str()))
                .ok_or(Failure::UnknownKey)?;
            if !k.compatible(alg) {
                return Err(Failure::Signature);
            }
            k
        }
        None => {
            let mut compatible = keys.iter().filter(|k| k.compatible(alg));
            match (compatible.next(), compatible.next()) {
                (Some(k), None) => k,
                _ => return Err(Failure::Signature),
            }
        }
    };
    if asymmetric_ok(alg, key, input, &t.signature) {
        Ok(())
    } else {
        Err(Failure::Signature)
    }
}

fn int_claim(claims: &Map<String, Value>, name: &str) -> Result<Option<i64>, Failure> {
    match claims.get(name) {
        None => Ok(None),
        Some(v) => v
            .as_i64()
            .or_else(|| v.as_f64().map(|f| f as i64))
            .map(Some)
            .ok_or(Failure::Malformed),
    }
}

/// Checks `iss`, `aud` and the times at `now` (Unix seconds).
pub fn verify_claims(t: &Unverified, rules: &Rules, now: i64) -> Result<(), Failure> {
    if t.claims.get("iss").and_then(Value::as_str) != Some(rules.issuer.as_str()) {
        return Err(Failure::Issuer);
    }
    let aud_ok = match t.claims.get("aud") {
        Some(Value::String(a)) => rules.audiences.contains(a),
        Some(Value::Array(list)) => list
            .iter()
            .filter_map(Value::as_str)
            .any(|a| rules.audiences.iter().any(|x| x == a)),
        _ => false,
    };
    if !aud_ok {
        return Err(Failure::Audience);
    }
    let skew = rules.clock_skew;
    let exp = int_claim(&t.claims, "exp")?.ok_or(Failure::Expired)?;
    if now >= exp.saturating_add(skew) {
        return Err(Failure::Expired);
    }
    if let Some(nbf) = int_claim(&t.claims, "nbf")?
        && now < nbf.saturating_sub(skew)
    {
        return Err(Failure::NotYetValid);
    }
    let iat = int_claim(&t.claims, "iat")?;
    if let Some(iat) = iat
        && iat > now.saturating_add(skew)
    {
        return Err(Failure::NotYetValid);
    }
    if let Some(max) = rules.max_lifetime {
        let Some(iat) = iat else {
            return Err(Failure::Expired);
        };
        if exp.saturating_sub(iat) > max {
            return Err(Failure::Expired);
        }
    }
    Ok(())
}

/// The value at a dotted claim path.
#[must_use]
pub fn claim<'a>(claims: &'a Map<String, Value>, path: &str) -> Option<&'a Value> {
    let mut parts = path.split('.');
    let first = claims.get(parts.next()?)?;
    parts.try_fold(first, |cur, p| cur.get(p))
}

/// Whether every listed claim matches: equal to the value (or to one of a list of values), or an
/// array containing it.
#[must_use]
pub fn matches(claims: &Map<String, Value>, wanted: &Map<String, Value>) -> bool {
    wanted.iter().all(|(path, want)| {
        let wants: Vec<&Value> = match want {
            Value::Array(list) => list.iter().collect(),
            other => vec![other],
        };
        match claim(claims, path) {
            Some(Value::Array(have)) => have.iter().any(|h| wants.contains(&h)),
            Some(have) => wants.contains(&have),
            None => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use aws_lc_rs::rand::SystemRandom;
    use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair};
    use serde_json::json;

    use super::*;

    fn enc(v: &Value) -> String {
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(v).unwrap())
    }

    fn rules() -> Rules {
        Rules {
            issuer: "https://idp".into(),
            audiences: vec!["antirion".into()],
            algorithms: Alg::DEFAULT.to_vec(),
            hmac_secret: None,
            clock_skew: 60,
            max_lifetime: None,
        }
    }

    fn ed_token(kp: &Ed25519KeyPair, claims: &Value, kid: &str) -> String {
        let input = format!("{}.{}", enc(&json!({"alg": "EdDSA", "kid": kid})), enc(claims));
        let sig = kp.sign(input.as_bytes());
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig.as_ref()))
    }

    #[test]
    fn ed25519_tokens_verify_and_claims_are_checked() {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let kp = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let jwks = json!({"keys": [{"kty": "OKP", "crv": "Ed25519", "kid": "ed",
                                     "x": URL_SAFE_NO_PAD.encode(kp.public_key().as_ref())},
                                    {"kty": "OKP", "crv": "X448", "kid": "x448", "x": "AA"}]});
        let keys = parse_jwks(&jwks);
        let claims = json!({"iss": "https://idp", "aud": ["x", "antirion"], "exp": 1000, "iat": 900});
        let t = decode(&ed_token(&kp, &claims, "ed")).unwrap();
        assert_eq!(verify_signature(&t, &rules(), &keys), Ok(()));
        assert_eq!(verify_claims(&t, &rules(), 950), Ok(()));
        assert_eq!(verify_claims(&t, &rules(), 1059), Ok(()));
        assert_eq!(verify_claims(&t, &rules(), 1060), Err(Failure::Expired));
        let mut r = rules();
        r.max_lifetime = Some(50);
        assert_eq!(verify_claims(&t, &r, 950), Err(Failure::Expired));
        let x = decode(&ed_token(&kp, &claims, "x448")).unwrap();
        assert_eq!(verify_signature(&x, &rules(), &keys), Err(Failure::Signature));
        let ghost = decode(&ed_token(&kp, &claims, "ghost")).unwrap();
        assert_eq!(
            verify_signature(&ghost, &rules(), &keys),
            Err(Failure::UnknownKey)
        );
    }

    #[test]
    fn algorithm_rules() {
        let none = format!("{}.{}.", enc(&json!({"alg": "none"})), enc(&json!({})));
        let t = decode(&none).unwrap();
        assert_eq!(verify_signature(&t, &rules(), &[]), Err(Failure::Algorithm));
        let input = format!("{}.{}", enc(&json!({"alg": "HS256"})), enc(&json!({})));
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(b"secret").unwrap();
        mac.update(input.as_bytes());
        let token = format!("{input}.{}", URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()));
        let t = decode(&token).unwrap();
        assert_eq!(verify_signature(&t, &rules(), &[]), Err(Failure::Algorithm));
        let mut r = rules();
        r.hmac_secret = Some(b"secret".to_vec());
        assert_eq!(verify_signature(&t, &r, &[]), Ok(()));
        assert!(decode(&"a".repeat(MAX_TOKEN + 1)).is_err());
    }

    #[test]
    fn claim_matching() {
        let claims = json!({"groups": ["ml", "x"], "tier": "gold", "org": {"id": "a"}});
        let c = claims.as_object().unwrap();
        let w = |v: Value| matches(c, v.as_object().unwrap());
        assert!(w(json!({"groups": "ml"})));
        assert!(w(json!({"groups": "ml", "tier": "gold", "org.id": "a"})));
        assert!(!w(json!({"groups": "ml", "tier": "silver"})));
        assert!(w(json!({"tier": ["silver", "gold"]})));
        assert!(w(json!({})));
    }
}
