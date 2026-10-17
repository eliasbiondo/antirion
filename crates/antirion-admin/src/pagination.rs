//! Cursor pagination: opaque cursors bound to their collection and protected by an integrity tag, so
//! a malformed, tampered or foreign cursor is refused.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::call::Call;
use crate::error::AdminError;

const TAG_LEN: usize = 16;

fn tag(key: &[u8], payload: &[u8]) -> Vec<u8> {
    let Ok(mut mac) = <Hmac<Sha256> as KeyInit>::new_from_slice(key) else {
        return Vec::new();
    };
    mac.update(payload);
    mac.finalize()
        .into_bytes()
        .get(..TAG_LEN)
        .map(<[u8]>::to_vec)
        .unwrap_or_default()
}

/// A cursor that continues `scope` after `position`.
pub(crate) fn encode(key: &[u8], scope: &str, position: &str) -> String {
    let payload = format!("{scope}\n{position}");
    format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(payload.as_bytes()),
        URL_SAFE_NO_PAD.encode(tag(key, payload.as_bytes()))
    )
}

/// The position of a cursor issued for `scope`.
pub(crate) fn decode(key: &[u8], scope: &str, cursor: &str) -> Option<String> {
    let (payload_b64, tag_b64) = cursor.split_once('.')?;
    let payload = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    let given = URL_SAFE_NO_PAD.decode(tag_b64).ok()?;
    let expected = tag(key, &payload);
    if expected.is_empty() || !bool::from(given.ct_eq(&expected)) {
        return None;
    }
    let text = String::from_utf8(payload).ok()?;
    let (s, position) = text.split_once('\n')?;
    (s == scope).then(|| position.to_owned())
}

/// `limit` and the decoded `cursor` of a list call.
pub(crate) fn page_params(call: &Call, scope: &str) -> Result<(usize, Option<String>), AdminError> {
    page_params_up_to(call, scope, call.rt.statics.max_page_size)
}

/// `limit` (at most `max`) and the decoded `cursor` of a list call.
pub(crate) fn page_params_up_to(
    call: &Call,
    scope: &str,
    max: usize,
) -> Result<(usize, Option<String>), AdminError> {
    let limit = match call.param("limit") {
        None => call.rt.statics.default_page_size,
        Some(v) => match v.parse::<usize>() {
            Ok(n) if (1..=max).contains(&n) => n,
            _ => {
                return Err(AdminError::invalid_parameter(format!(
                    "limit must be an integer from 1 to {max}"
                )));
            }
        },
    };
    let after = match call.param("cursor") {
        None => None,
        Some(c) => Some(
            decode(&call.rt.cursor_key, scope, c)
                .ok_or_else(|| AdminError::invalid_parameter("the cursor is not valid"))?,
        ),
    };
    Ok((limit, after))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_tampering() {
        let key = b"k".repeat(32);
        let c = encode(&key, "models", "m1");
        assert_eq!(decode(&key, "models", &c).as_deref(), Some("m1"));
        assert_eq!(decode(&key, "providers", &c), None);
        let mut tampered = c.clone();
        tampered.truncate(c.len() - 2);
        tampered.push_str(if c.ends_with("AA") { "BB" } else { "AA" });
        assert_eq!(decode(&key, "models", &tampered), None);
        for bad in ["garbage", "", "AAAAAAAAAAAAAAAAAAAA", "eyJvZmZzZXQiOjB9"] {
            assert_eq!(decode(&key, "models", bad), None);
        }
    }
}
