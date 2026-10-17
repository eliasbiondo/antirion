//! One request: send it on a pooled connection, read the response to its last byte, time it from its
//! intended send time.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use bytes::Bytes;
use http::{HeaderMap, Request, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1;
use tokio_util::sync::CancellationToken;

use crate::conn::{Lease, Pool, ReqBody};
use crate::record::{Outcome, OutcomeKind};
use crate::sse::EventClock;

/// A request ready to send, with what counts as success.
#[derive(Debug)]
pub struct Prepared {
    /// The node it goes to (an index into the run's nodes).
    pub node: usize,
    pub request: Request<ReqBody>,
    /// The status of a successful answer (401 for a rejection scenario).
    pub expect: u16,
    /// Leave the stream after its first content event.
    pub disconnect: bool,
}

/// What a run sends: request `index` of the schedule.
pub trait Source: Send + Sync {
    fn prepare(&self, index: u64) -> Prepared;
}

const OVERHEAD_HEADER: &str = "x-antirion-overhead-ms";

fn overhead_us(headers: &HeaderMap) -> Option<u64> {
    let ms: f64 = headers.get(OVERHEAD_HEADER)?.to_str().ok()?.trim().parse().ok()?;
    if !ms.is_finite() || ms < 0.0 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some((ms * 1000.0).round() as u64)
}

fn is_event_stream(headers: &HeaderMap) -> bool {
    headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"))
}

async fn send(
    lease: &mut Lease,
    req: Request<ReqBody>,
) -> Result<http::Response<hyper::body::Incoming>, hyper::Error> {
    match lease {
        Lease::H1(s, _) => {
            s.ready().await?;
            s.send_request(req).await
        }
        Lease::H2(s, _) => {
            s.ready().await?;
            s.send_request(req).await
        }
    }
}

async fn attempt(pool: &Pool, p: Prepared, intended: Instant, out: &mut Outcome) {
    let Ok(mut lease) = pool.checkout().await else {
        out.kind = OutcomeKind::Connect;
        return;
    };
    let Ok(resp) = send(&mut lease, p.request).await else {
        out.kind = OutcomeKind::Connect;
        return;
    };
    let status = resp.status().as_u16();
    out.overhead_header_us = overhead_us(resp.headers());
    let stream = is_event_stream(resp.headers());
    let mut body = resp.into_body();
    let mut clock = EventClock::default();
    loop {
        match body.frame().await {
            None => break,
            Some(Err(_)) => {
                out.kind = OutcomeKind::Body;
                return;
            }
            Some(Ok(frame)) => {
                if stream && let Some(data) = frame.data_ref() {
                    clock.feed(data, Instant::now(), &mut out.gaps);
                    if p.disconnect && clock.first_content().is_some() {
                        out.ttft = clock
                            .first_content()
                            .map(|t| t.saturating_duration_since(intended));
                        out.kind = OutcomeKind::Disconnected;
                        return;
                    }
                }
            }
        }
    }
    let done = Instant::now();
    out.ttft = clock
        .first_content()
        .map(|t| t.saturating_duration_since(intended));
    if status == p.expect {
        out.latency = Some(done.saturating_duration_since(intended));
    } else {
        out.kind = OutcomeKind::Status(status);
    }
    pool.checkin(lease);
}

/// Sends `p` and reads its answer, within `timeout` of the actual send; `hold` ends it as held.
pub async fn exchange(
    pool: &Pool,
    p: Prepared,
    intended: Instant,
    timeout: Duration,
    hold: &CancellationToken,
) -> Outcome {
    let started = Instant::now();
    let mut out = Outcome {
        lag: started.saturating_duration_since(intended),
        ..Outcome::default()
    };
    let deadline = tokio::time::Instant::from_std(started + timeout);
    tokio::select! {
        r = tokio::time::timeout_at(deadline, attempt(pool, p, intended, &mut out)) => {
            if r.is_err() {
                out.kind = OutcomeKind::Timeout;
            }
        }
        () = hold.cancelled() => out.kind = OutcomeKind::Held,
    }
    out
}

/// A one-off HTTP/1.1 request on a fresh connection (control-plane calls, probes, session setup).
pub async fn fetch(
    addr: SocketAddr,
    req: Request<Full<Bytes>>,
    timeout: Duration,
) -> Result<(StatusCode, HeaderMap, Bytes), FetchError> {
    let work = async {
        let stream = tokio::net::TcpStream::connect(addr)
            .await
            .map_err(FetchError::Io)?;
        let (mut send, conn) = http1::handshake(hyper_util::rt::TokioIo::new(stream))
            .await
            .map_err(FetchError::Http)?;
        let run = async {
            let resp = send.send_request(req).await?;
            let (parts, body) = resp.into_parts();
            let bytes = body.collect().await?.to_bytes();
            Ok::<_, hyper::Error>((parts.status, parts.headers, bytes))
        };
        tokio::select! {
            r = run => r.map_err(FetchError::Http),
            r = conn => match r {
                Ok(()) => Err(FetchError::Closed),
                Err(e) => Err(FetchError::Http(e)),
            },
        }
    };
    tokio::time::timeout(timeout, work)
        .await
        .map_err(|_| FetchError::Timeout)?
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("connection failed: {0}")]
    Io(std::io::Error),
    #[error("HTTP error: {0}")]
    Http(hyper::Error),
    #[error("the connection closed before the response")]
    Closed,
    #[error("timed out")]
    Timeout,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overhead_header_is_read_in_microseconds() {
        let mut h = HeaderMap::new();
        h.insert(OVERHEAD_HEADER, http::HeaderValue::from_static("0.125"));
        assert_eq!(overhead_us(&h), Some(125));
        h.insert(OVERHEAD_HEADER, http::HeaderValue::from_static("-1"));
        assert_eq!(overhead_us(&h), None);
    }
}
