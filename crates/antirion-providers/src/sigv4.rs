//! AWS Signature Version 4 over the exact request that is sent.

use hmac::{Hmac, KeyInit, Mac};
use http::header::{AUTHORIZATION, CONTENT_TYPE, HOST};
use http::{HeaderMap, HeaderName, HeaderValue, Method, Uri};
use sha2::{Digest, Sha256};

/// AWS credentials used to sign one request.
#[derive(Clone, PartialEq, Eq)]
pub struct AwsCredentials {
    pub access_key_id: String,
    pub secret_access_key: antirion_core::Secret,
    pub session_token: Option<antirion_core::Secret>,
}

impl std::fmt::Debug for AwsCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwsCredentials")
            .field("access_key_id", &self.access_key_id)
            .finish_non_exhaustive()
    }
}

const AMZ_DATE: &str = "x-amz-date";
const SECURITY_TOKEN: &str = "x-amz-security-token";

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// The signing time `now` as SigV4 writes it, `YYYYMMDD'T'HHMMSS'Z'`.
#[must_use]
pub fn amz_date(now: antirion_core::UtcMicros) -> String {
    jiff::Timestamp::from_microsecond(now.0)
        .map(|t| t.strftime("%Y%m%dT%H%M%SZ").to_string())
        .unwrap_or_default()
}

/// The lowercase hex SHA-256 of `data`.
#[must_use]
pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn hmac(key: &[u8], msg: &[u8]) -> Vec<u8> {
    match <Hmac<Sha256> as KeyInit>::new_from_slice(key) {
        Ok(mut mac) => {
            mac.update(msg);
            mac.finalize().into_bytes().to_vec()
        }
        Err(_) => Vec::new(),
    }
}

/// RFC 3986 encoding of everything outside the unreserved set, with uppercase hex.
#[must_use]
pub fn uri_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%'
            && let Some(h) = bytes.get(i + 1..i + 3)
            && let Ok(h) = std::str::from_utf8(h)
            && let Ok(v) = u8::from_str_radix(h, 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(if b == b'+' { b' ' } else { b });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The canonical URI of a non-S3 service: each segment of the path as sent is encoded again.
fn canonical_uri(path: &str) -> String {
    if path.is_empty() {
        return "/".to_owned();
    }
    path.split('/').map(uri_encode).collect::<Vec<_>>().join("/")
}

fn canonical_query(query: Option<&str>) -> String {
    let mut pairs: Vec<(String, String)> = query
        .unwrap_or_default()
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (uri_encode(&percent_decode(k)), uri_encode(&percent_decode(v)))
        })
        .collect();
    pairs.sort();
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn collapse(v: &str) -> String {
    v.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The `Host` value of a URI: the authority without a default port.
#[must_use]
pub fn host_of(uri: &Uri) -> Option<String> {
    let host = uri.host()?;
    let default = match uri.scheme_str() {
        Some("https") => 443,
        _ => 80,
    };
    Some(match uri.port_u16() {
        Some(p) if p != default => format!("{host}:{p}"),
        _ => host.to_owned(),
    })
}

/// What one signature is computed over.
#[derive(Debug)]
pub struct SigningInput<'a> {
    pub method: &'a Method,
    pub uri: &'a Uri,
    pub body: &'a [u8],
    pub region: &'a str,
    pub service: &'a str,
    /// The signing time, `YYYYMMDD'T'HHMMSS'Z'`.
    pub amz_date: &'a str,
}

/// Signs a request in place: sets `host`, `x-amz-date`, `x-amz-security-token` when the
/// credentials carry one, and `authorization`. The signed headers are `content-type` (when present),
/// `host`, `x-amz-date` and the session token.
pub fn sign(headers: &mut HeaderMap, input: &SigningInput<'_>, creds: &AwsCredentials) {
    let host = host_of(input.uri).unwrap_or_default();
    if let Ok(v) = HeaderValue::from_str(&host) {
        headers.insert(HOST, v);
    }
    if let Ok(v) = HeaderValue::from_str(input.amz_date) {
        headers.insert(HeaderName::from_static(AMZ_DATE), v);
    }
    headers.remove(SECURITY_TOKEN);
    if let Some(token) = &creds.session_token
        && let Ok(mut v) = HeaderValue::from_str(token.expose())
    {
        v.set_sensitive(true);
        headers.insert(HeaderName::from_static(SECURITY_TOKEN), v);
    }
    let mut signed: Vec<(&str, String)> = Vec::new();
    if let Some(ct) = headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok()) {
        signed.push(("content-type", collapse(ct)));
    }
    signed.push(("host", host));
    signed.push((AMZ_DATE, input.amz_date.to_owned()));
    if let Some(token) = &creds.session_token {
        signed.push((SECURITY_TOKEN, collapse(token.expose())));
    }
    signed.sort_by(|a, b| a.0.cmp(b.0));
    let canonical_headers: String = signed.iter().map(|(n, v)| format!("{n}:{v}\n")).collect();
    let signed_names = signed.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(";");
    let creq = [
        input.method.as_str(),
        &canonical_uri(input.uri.path()),
        &canonical_query(input.uri.query()),
        &canonical_headers,
        &signed_names,
        &sha256_hex(input.body),
    ]
    .join("\n");
    let date = input.amz_date.get(..8).unwrap_or_default();
    let scope = format!("{date}/{}/{}/aws4_request", input.region, input.service);
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{scope}\n{}",
        input.amz_date,
        sha256_hex(creq.as_bytes())
    );
    let mut key = hmac(
        format!("AWS4{}", creds.secret_access_key.expose()).as_bytes(),
        date.as_bytes(),
    );
    for part in [input.region, input.service, "aws4_request"] {
        key = hmac(&key, part.as_bytes());
    }
    let signature = hex(&hmac(&key, to_sign.as_bytes()));
    let value = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_names}, Signature={signature}",
        creds.access_key_id
    );
    if let Ok(mut v) = HeaderValue::from_str(&value) {
        v.set_sensitive(true);
        headers.insert(AUTHORIZATION, v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example_creds() -> AwsCredentials {
        AwsCredentials {
            access_key_id: "AKIDEXAMPLE".into(),
            secret_access_key: antirion_core::Secret::new("wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY"),
            session_token: None,
        }
    }

    #[test]
    fn aws_documentation_example() {
        let uri: Uri = "https://iam.amazonaws.com/?Action=ListUsers&Version=2010-05-08"
            .parse()
            .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded; charset=utf-8"),
        );
        sign(
            &mut headers,
            &SigningInput {
                method: &Method::GET,
                uri: &uri,
                body: b"",
                region: "us-east-1",
                service: "iam",
                amz_date: "20150830T123600Z",
            },
            &example_creds(),
        );
        assert_eq!(
            headers["authorization"],
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/iam/aws4_request, \
             SignedHeaders=content-type;host;x-amz-date, \
             Signature=5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7"
        );
        assert_eq!(headers["host"], "iam.amazonaws.com");
    }

    #[test]
    fn test_suite_get_vanilla() {
        let uri: Uri = "https://example.amazonaws.com/".parse().unwrap();
        let mut headers = HeaderMap::new();
        sign(
            &mut headers,
            &SigningInput {
                method: &Method::GET,
                uri: &uri,
                body: b"",
                region: "us-east-1",
                service: "service",
                amz_date: "20150830T123600Z",
            },
            &example_creds(),
        );
        let auth = headers["authorization"].to_str().unwrap();
        assert!(auth.ends_with("Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"));
    }

    #[test]
    fn path_is_encoded_twice_and_token_signed() {
        assert_eq!(
            canonical_uri("/model/a.b-v1%3A0/converse"),
            "/model/a.b-v1%253A0/converse"
        );
        assert_eq!(canonical_query(Some("b=2&a=x%20y")), "a=x%20y&b=2");
        let uri: Uri = "http://127.0.0.1:9000/model/m/converse".parse().unwrap();
        let mut headers = HeaderMap::new();
        let mut creds = example_creds();
        creds.session_token = Some(antirion_core::Secret::new("tok"));
        sign(
            &mut headers,
            &SigningInput {
                method: &Method::POST,
                uri: &uri,
                body: b"{}",
                region: "us-west-2",
                service: "bedrock",
                amz_date: "20260101T000000Z",
            },
            &creds,
        );
        assert_eq!(headers["host"], "127.0.0.1:9000");
        assert_eq!(headers["x-amz-security-token"], "tok");
        assert!(
            headers["authorization"]
                .to_str()
                .unwrap()
                .contains("SignedHeaders=host;x-amz-date;x-amz-security-token,")
        );
    }
}
