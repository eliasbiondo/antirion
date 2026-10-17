//! Which data-plane credential a request presents, decided from headers only (and, on Gemini-native
//! routes, the `key` query parameter); the body is never read here.

use antirion_core::headers as h;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use http::HeaderMap;

use crate::keyfmt::{Credential, Plane};

/// The credential a request presents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presented {
    Key(Credential),
    Jwt(String),
    /// A legacy secret whose SHA-256 (lowercase hex) is the digest of an imported key.
    Imported(String),
}

/// Whether a lowercase hex SHA-256 digest belongs to an imported key; `None` while no key has
/// been imported, so other values are never hashed.
pub type ImportedKeys<'a> = Option<&'a dyn Fn(&str) -> bool>;

/// The lowercase hex SHA-256 of a presented value, as legacy keys are imported.
#[must_use]
pub fn legacy_digest(value: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(value.as_bytes())
        .iter()
        .fold(String::with_capacity(64), |mut out, b| {
            use std::fmt::Write;
            let _ = write!(out, "{b:02x}");
            out
        })
}

/// Why no usable credential was selected.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Nothing was presented.
    Missing,
    /// Something was presented, but nothing well-formed.
    Invalid,
    /// Two distinct well-formed credentials.
    Ambiguous,
}

fn classify(value: &str, imported: ImportedKeys<'_>) -> Option<Presented> {
    match Credential::parse(value) {
        Some(c) if c.plane() == Plane::Data => return Some(Presented::Key(c)),
        Some(_) => return None,
        None => {}
    }
    if let Some(known) = imported {
        let digest = legacy_digest(value);
        if known(&digest) {
            return Some(Presented::Imported(digest));
        }
    }
    looks_like_jwt(value).then(|| Presented::Jwt(value.to_owned()))
}

/// A compact JWS whose header decodes to a JSON object with an `alg` member.
#[must_use]
pub fn looks_like_jwt(value: &str) -> bool {
    let mut parts = value.split('.');
    let (Some(header), Some(_), Some(_), None) = (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let Ok(decoded) = URL_SAFE_NO_PAD.decode(header) else {
        return false;
    };
    serde_json::from_slice::<serde_json::Value>(&decoded)
        .ok()
        .and_then(|v| v.as_object().map(|o| o.contains_key("alg")))
        .unwrap_or(false)
}

/// The token of `Authorization: Bearer <token68>`; `None` for any other form.
#[must_use]
pub fn bearer_token(value: &str) -> Option<&str> {
    let (scheme, rest) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = rest.trim_start_matches(' ');
    let body = token.trim_end_matches('=');
    let valid = !body.is_empty()
        && body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'+' | b'/'));
    valid.then_some(token)
}

fn present(value: &str) -> Option<&str> {
    let v = value.trim();
    (!v.is_empty()).then_some(v)
}

/// Selects the data-plane credential. `gemini_query_key` is the `key` query parameter on a
/// Gemini-native route, and `None` everywhere else; `imported` recognises imported legacy keys.
pub fn select(
    headers: &HeaderMap,
    gemini_query_key: Option<&str>,
    imported: ImportedKeys<'_>,
) -> Result<Presented, Refusal> {
    if let Some(v) = headers
        .get(h::API_KEY)
        .and_then(|v| v.to_str().ok())
        .and_then(present)
    {
        return classify(v, imported).ok_or(Refusal::Invalid);
    }
    let mut seen_any = false;
    let mut found: Vec<Presented> = Vec::new();
    let mut consider = |raw: &str, extract_bearer: bool| {
        let Some(v) = present(raw) else { return };
        seen_any = true;
        let candidate = if extract_bearer { bearer_token(v) } else { Some(v) };
        if let Some(p) = candidate.and_then(|c| classify(c, imported))
            && !found.contains(&p)
        {
            found.push(p);
        }
    };
    for v in headers.get_all(http::header::AUTHORIZATION) {
        match v.to_str() {
            Ok(s) => consider(s, true),
            Err(_) => consider("invalid", false),
        }
    }
    for name in [h::X_API_KEY, h::AZURE_API_KEY, h::GOOG_API_KEY] {
        for v in headers.get_all(name) {
            match v.to_str() {
                Ok(s) => consider(s, false),
                Err(_) => consider("invalid", false),
            }
        }
    }
    if let Some(q) = gemini_query_key {
        consider(q, false);
    }
    match found.len() {
        1 => found.pop().ok_or(Refusal::Invalid),
        0 if seen_any => Err(Refusal::Invalid),
        0 => Err(Refusal::Missing),
        _ => Err(Refusal::Ambiguous),
    }
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;

    use super::*;

    fn key() -> Credential {
        Credential::generate(Plane::Data).unwrap()
    }

    fn headers(pairs: &[(&'static str, String)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.append(*k, HeaderValue::from_str(v).unwrap());
        }
        m
    }

    #[test]
    fn bearer_grammar() {
        assert_eq!(bearer_token("Bearer abc"), Some("abc"));
        assert_eq!(bearer_token("bearer abc=="), Some("abc=="));
        assert_eq!(bearer_token("Basic abc"), None);
        assert_eq!(bearer_token("Bearer "), None);
        assert_eq!(bearer_token("Bearer a b"), None);
        assert_eq!(bearer_token("abc"), None);
    }

    #[test]
    fn selection_outcomes() {
        let k = key();
        let got = select(
            &headers(&[("authorization", format!("Bearer {}", k.expose()))]),
            None,
            None,
        )
        .unwrap();
        assert_eq!(got, Presented::Key(k.clone()));
        assert_eq!(select(&HeaderMap::new(), None, None), Err(Refusal::Missing));
        assert_eq!(
            select(
                &headers(&[("authorization", "Bearer garbage".into())]),
                None,
                None
            ),
            Err(Refusal::Invalid)
        );
        let other = key();
        let both = headers(&[
            ("authorization", format!("Bearer {}", k.expose())),
            ("x-api-key", other.expose().into()),
        ]);
        assert_eq!(select(&both, None, None), Err(Refusal::Ambiguous));
        let same = headers(&[
            ("authorization", format!("Bearer {}", k.expose())),
            ("x-api-key", k.expose().into()),
        ]);
        assert!(select(&same, None, None).is_ok());
    }

    #[test]
    fn explicit_header_wins_and_admin_tokens_are_invalid() {
        let k = key();
        let m = headers(&[
            ("x-antirion-api-key", k.expose().into()),
            ("authorization", "Bearer junk".into()),
        ]);
        assert_eq!(select(&m, None, None).unwrap(), Presented::Key(k));
        let admin = Credential::generate(Plane::Admin).unwrap();
        let m = headers(&[("authorization", format!("Bearer {}", admin.expose()))]);
        assert_eq!(select(&m, None, None), Err(Refusal::Invalid));
    }

    #[test]
    fn imported_digests_are_credentials_anywhere() {
        let digest = legacy_digest("sk-legacy-123");
        assert_eq!(
            digest,
            "bb33792b9e66f232274ba3505c792c611f1e88a019333fc7b90ce43716562022"
        );
        let known = |d: &str| d == digest;
        let m = headers(&[("authorization", "Bearer sk-legacy-123".into())]);
        assert_eq!(
            select(&m, None, Some(&known)).unwrap(),
            Presented::Imported(digest.clone())
        );
        assert_eq!(select(&m, None, None), Err(Refusal::Invalid));
        let m = headers(&[("x-antirion-api-key", "sk-legacy-123".into())]);
        assert!(matches!(
            select(&m, None, Some(&known)),
            Ok(Presented::Imported(_))
        ));
        let m = headers(&[("x-api-key", "sk-other".into())]);
        assert_eq!(select(&m, None, Some(&known)), Err(Refusal::Invalid));
    }

    #[test]
    fn jwts_are_recognised() {
        let token = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2ln";
        assert!(looks_like_jwt(token));
        let m = headers(&[("authorization", format!("Bearer {token}"))]);
        assert_eq!(select(&m, None, None).unwrap(), Presented::Jwt(token.into()));
    }
}
