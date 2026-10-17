#![no_main]

use antirion_identity::keyfmt::Credential;
use antirion_identity::select::{bearer_token, select};
use http::{HeaderMap, HeaderValue, header::AUTHORIZATION};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(raw) = std::str::from_utf8(data) else { return };
    let _ = bearer_token(raw);
    let _ = Credential::parse(raw);
    let Ok(value) = HeaderValue::from_bytes(data) else { return };
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, value.clone());
    let _ = select(&headers, None, None);
    headers.insert("x-api-key", value);
    let _ = select(&headers, Some(raw), None);
});
