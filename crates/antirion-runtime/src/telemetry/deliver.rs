//! One POST to an operator-configured collector through the egress client: operator trust, the
//! `security.egress` policy and TLS profile, never re-sent by the transport.

use std::sync::Arc;
use std::time::Duration;

use antirion_egress::{OutboundRequest, TrustClass};
use bytes::Bytes;
use http::header::{CONTENT_ENCODING, CONTENT_TYPE, USER_AGENT};
use http::{HeaderMap, HeaderName, HeaderValue, Method, Uri};
use http_body_util::BodyExt;
use tokio::io::AsyncWriteExt;

use super::Env;

/// A delivery that did not succeed.
#[derive(Debug)]
pub(crate) struct Failed;

pub(crate) async fn gzip(body: &[u8]) -> Vec<u8> {
    let mut enc = async_compression::tokio::write::GzipEncoder::new(Vec::new());
    if enc.write_all(body).await.is_err() || enc.shutdown().await.is_err() {
        return Vec::new();
    }
    enc.into_inner()
}

/// How one POST ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Sent {
    /// The response status, when one arrived.
    pub(crate) status: Option<u16>,
    pub(crate) timed_out: bool,
}

impl Sent {
    pub(crate) fn ok(self) -> bool {
        self.status.is_some_and(|s| (200..300).contains(&s))
    }

    /// `delivered`, `failed` or `timeout`, as the delivery log records it.
    pub(crate) fn result(self) -> &'static str {
        if self.ok() {
            "delivered"
        } else if self.timed_out {
            "timeout"
        } else {
            "failed"
        }
    }
}

/// POSTs `body` and succeeds on any 2xx within `timeout`.
pub(crate) async fn post(
    env: &Env,
    url: &str,
    extra: &[(String, String)],
    content_type: &'static str,
    body: Vec<u8>,
    compress: bool,
    timeout: Duration,
) -> Result<(), Failed> {
    let sent = send(env, url, extra, content_type, body, compress, timeout).await;
    if sent.ok() { Ok(()) } else { Err(Failed) }
}

/// POSTs `body` and reports the status it got within `timeout`.
pub(crate) async fn send(
    env: &Env,
    url: &str,
    extra: &[(String, String)],
    content_type: &'static str,
    body: Vec<u8>,
    compress: bool,
    timeout: Duration,
) -> Sent {
    let failed = Sent {
        status: None,
        timed_out: false,
    };
    let Ok(uri) = url.parse::<Uri>() else {
        return failed;
    };
    let Some(tls) = env.tls.clone() else {
        return failed;
    };
    let mut headers = HeaderMap::new();
    for (k, v) in extra {
        if let (Ok(name), Ok(value)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
            headers.insert(name, value);
        }
    }
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    if let Ok(ua) = HeaderValue::from_str(&format!("antirion/{}", env.version)) {
        headers.insert(USER_AGENT, ua);
    }
    let body = if compress {
        headers.insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        gzip(&body).await
    } else {
        body
    };
    let send = env.client.send(OutboundRequest {
        method: Method::POST,
        uri,
        headers,
        body: Bytes::from(body).into(),
        response_timeout: timeout,
        connect_timeout: None,
        fresh_connection: false,
        trust: TrustClass::Operator,
        tls,
        policy: Arc::clone(&env.policy),
        resend_when_closed: false,
    });
    let outcome = tokio::time::timeout(timeout, async {
        let resp = send.await.ok()?;
        let status = resp.status.as_u16();
        let _ = resp.body.collect().await;
        Some(status)
    })
    .await;
    match outcome {
        Ok(status) => Sent {
            status,
            timed_out: false,
        },
        Err(_) => Sent {
            status: None,
            timed_out: true,
        },
    }
}
