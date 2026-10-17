//! The credential-acquisition client: requests to token and metadata endpoints, never carrying client
//! data or client headers.

use std::sync::Arc;
use std::time::Duration;

use antirion_egress::{EgressClient, EgressPolicy, OutboundRequest, TlsProfile, TrustClass};
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use http::{HeaderMap, Method, Uri};
use http_body_util::BodyExt;

use super::CredError;

const MAX_BODY: usize = 1 << 20;
const TIMEOUT: Duration = Duration::from_secs(10);

/// One response of a credential endpoint.
#[derive(Debug)]
pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) body: Bytes,
}

/// The credential-acquisition client with the serving egress policy.
#[derive(Clone)]
pub(crate) struct CredClient {
    pub(crate) client: EgressClient,
    pub(crate) policy: Arc<EgressPolicy>,
}

/// Sends one request to a configurable credential endpoint (a token endpoint, an authority, an
/// STS endpoint) as operator traffic under the serving egress policy, and reads the whole reply.
pub(crate) async fn send(
    client: &CredClient,
    tls: &TlsProfile,
    method: Method,
    url: &str,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Reply, CredError> {
    exchange(client, tls, Endpoint::Configured, method, url, headers, body).await
}

/// Sends one request to a compiled-in instance metadata endpoint (or the one the node's environment
/// names), the only traffic the credential trust class exempts from the address rules.
pub(crate) async fn send_metadata(
    client: &CredClient,
    tls: &TlsProfile,
    method: Method,
    url: &str,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Reply, CredError> {
    exchange(client, tls, Endpoint::Metadata, method, url, headers, body).await
}

enum Endpoint {
    Metadata,
    Configured,
}

async fn exchange(
    client: &CredClient,
    tls: &TlsProfile,
    endpoint: Endpoint,
    method: Method,
    url: &str,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Reply, CredError> {
    let (trust, policy) = match endpoint {
        Endpoint::Metadata => (TrustClass::Credential, Arc::new(EgressPolicy::default())),
        Endpoint::Configured => (TrustClass::Operator, Arc::clone(&client.policy)),
    };
    let uri: Uri = url.parse().map_err(|_| CredError::Endpoint)?;
    let request = OutboundRequest {
        method,
        uri,
        headers,
        body: body.into(),
        response_timeout: TIMEOUT,
        connect_timeout: None,
        fresh_connection: false,
        trust,
        tls: tls.clone(),
        policy,
        resend_when_closed: false,
    };
    let resp = client
        .client
        .send(request)
        .await
        .map_err(|_| CredError::Unreachable)?;
    let mut chunks = resp.body.into_data_stream();
    let mut out = BytesMut::new();
    let read = async {
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|_| CredError::Unreachable)?;
            if out.len() + chunk.len() > MAX_BODY {
                return Err(CredError::Malformed);
            }
            out.extend_from_slice(&chunk);
        }
        Ok(())
    };
    tokio::time::timeout(TIMEOUT, read)
        .await
        .map_err(|_| CredError::Unreachable)??;
    Ok(Reply {
        status: resp.status.as_u16(),
        body: out.freeze(),
    })
}

/// `application/x-www-form-urlencoded` encoding of one value.
pub(crate) fn form_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'*' => out.push(char::from(b)),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// A form body from name/value pairs.
pub(crate) fn form(pairs: &[(&str, &str)]) -> Bytes {
    Bytes::from(
        pairs
            .iter()
            .map(|(k, v)| format!("{}={}", form_encode(k), form_encode(v)))
            .collect::<Vec<_>>()
            .join("&"),
    )
}
