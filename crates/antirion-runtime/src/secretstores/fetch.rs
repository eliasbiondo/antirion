//! One request to a secret store: operator egress under the `security.egress` policy and TLS
//! profile, bounded in time and size. Failures are described without any value.

use std::sync::Arc;
use std::time::Duration;

use antirion_egress::{EgressClient, EgressPolicy, OutboundRequest, TlsProfile, TrustClass};
use antirion_providers::Credentials;
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use http::{HeaderMap, HeaderValue, Method, Uri};
use http_body_util::BodyExt;

/// The largest answer a store may give.
const MAX_ANSWER: usize = 1 << 20;

/// What fetches are sent with.
#[derive(Clone)]
pub(crate) struct Cx {
    pub(crate) client: EgressClient,
    pub(crate) creds: Arc<Credentials>,
    pub(crate) policy: Arc<EgressPolicy>,
    pub(crate) tls: TlsProfile,
}

/// Why a value could not be read; the text names the cause, never a value.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub(crate) struct FetchError(pub(crate) String);

impl FetchError {
    pub(crate) fn new(why: impl Into<String>) -> FetchError {
        FetchError(why.into())
    }

    /// The store's refusal of a request.
    pub(crate) fn status(status: u16) -> FetchError {
        FetchError(match status {
            401 | 403 => format!("the store refused the credentials (status {status})"),
            404 => "the secret does not exist (status 404)".to_owned(),
            _ => format!("the store answered with status {status}"),
        })
    }
}

/// A store's answer.
pub(crate) struct Answer {
    pub(crate) status: u16,
    pub(crate) body: Bytes,
}

/// Sends one request and reads the whole answer within `timeout`.
pub(crate) async fn send(
    cx: &Cx,
    method: Method,
    url: &str,
    headers: HeaderMap,
    body: Bytes,
    timeout: Duration,
) -> Result<Answer, FetchError> {
    let uri: Uri = url
        .parse()
        .map_err(|_| FetchError::new("the store's URL is not valid"))?;
    let request = OutboundRequest {
        method,
        uri,
        headers,
        body: body.into(),
        response_timeout: timeout,
        connect_timeout: None,
        fresh_connection: false,
        trust: TrustClass::Operator,
        tls: cx.tls.clone(),
        policy: Arc::clone(&cx.policy),
        resend_when_closed: false,
    };
    let read = async {
        let resp = cx
            .client
            .send(request)
            .await
            .map_err(|_| FetchError::new("the store could not be reached"))?;
        let status = resp.status.as_u16();
        let mut stream = resp.body.into_data_stream();
        let mut out = BytesMut::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| FetchError::new("the store's answer was cut short"))?;
            if out.len() + chunk.len() > MAX_ANSWER {
                return Err(FetchError::new("the store's answer is too large"));
            }
            out.extend_from_slice(&chunk);
        }
        Ok(Answer {
            status,
            body: out.freeze(),
        })
    };
    tokio::time::timeout(timeout, read)
        .await
        .map_err(|_| FetchError::new("the store did not answer in time"))?
}

/// A header value that is never logged.
pub(crate) fn sensitive(value: &str) -> Result<HeaderValue, FetchError> {
    let mut v = HeaderValue::from_str(value)
        .map_err(|_| FetchError::new("a credential is not a valid header value"))?;
    v.set_sensitive(true);
    Ok(v)
}

/// The answer as JSON after a 2xx status.
pub(crate) fn json(answer: &Answer) -> Result<serde_json::Value, FetchError> {
    if !(200..300).contains(&answer.status) {
        return Err(FetchError::status(answer.status));
    }
    serde_json::from_slice(&answer.body).map_err(|_| FetchError::new("the store's answer is not JSON"))
}
