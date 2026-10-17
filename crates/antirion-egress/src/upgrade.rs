//! An upstream request upgraded to another protocol (a WebSocket): sent over HTTP/1.1 on a
//! connection of its own, under the same egress rules as every other request.

use bytes::Bytes;
use http::{HeaderMap, StatusCode};
use http_body_util::BodyExt;
use hyper::upgrade::Upgraded;

use crate::{EgressClient, OutboundRequest, TransportError};

/// The upstream's answer to an upgrade request.
#[derive(Debug)]
pub enum UpgradeAnswer {
    /// `101 Switching Protocols`: the response headers and the connection, now the new protocol's.
    Switched { headers: HeaderMap, io: Upgraded },
    /// Any other answer, with its body (at most `limit` bytes of it).
    Refused {
        status: StatusCode,
        headers: HeaderMap,
        body: Bytes,
    },
}

impl EgressClient {
    /// Sends `r` (its method, URI and upgrade headers set by the caller) on a new HTTP/1.1
    /// connection and returns the upgraded connection, or the upstream's refusal.
    pub async fn upgrade(
        &self,
        mut r: OutboundRequest,
        limit: usize,
    ) -> Result<UpgradeAnswer, TransportError> {
        match r.uri.scheme_str() {
            Some("http" | "https") => {}
            other => return Err(TransportError::UnsupportedScheme(other.unwrap_or("").to_owned())),
        }
        r.tls = r.tls.http1_only();
        r.fresh_connection = true;
        let deadline = tokio::time::Instant::now() + r.response_timeout;
        let (pool, _) = self.pool(&r);
        let mut req = http::Request::new(r.body.body());
        *req.method_mut() = r.method.clone();
        *req.uri_mut() = r.uri.clone();
        *req.version_mut() = http::Version::HTTP_11;
        *req.headers_mut() = r.headers.clone();
        let resp = match tokio::time::timeout_at(deadline, pool.request(req)).await {
            Err(_) => return Err(TransportError::Timeout),
            Ok(Err(e)) if e.is_connect() => return Err(crate::connect_error(&e)),
            Ok(Err(_)) => return Err(TransportError::Reset),
            Ok(Ok(resp)) => resp,
        };
        if resp.status() == StatusCode::SWITCHING_PROTOCOLS {
            let headers = resp.headers().clone();
            let io = tokio::time::timeout_at(deadline, hyper::upgrade::on(resp))
                .await
                .map_err(|_| TransportError::Timeout)?
                .map_err(|_| TransportError::Reset)?;
            return Ok(UpgradeAnswer::Switched { headers, io });
        }
        let (parts, body) = resp.into_parts();
        let mut out = Vec::new();
        let mut frames = body;
        while out.len() < limit {
            match tokio::time::timeout_at(deadline, frames.frame()).await {
                Ok(Some(Ok(f))) => {
                    if let Ok(data) = f.into_data() {
                        out.extend_from_slice(&data);
                    }
                }
                _ => break,
            }
        }
        out.truncate(limit);
        Ok(UpgradeAnswer::Refused {
            status: parts.status,
            headers: parts.headers,
            body: Bytes::from(out),
        })
    }
}
