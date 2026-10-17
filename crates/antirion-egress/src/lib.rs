//! The one egress path: every outbound connection is validated against the SSRF rules of its trust
//! class, connects to the address that was validated, verifies TLS with the destination's profile,
//! and never follows redirects. Failures are typed, so they are classified from their kind and
//! never from their text.

#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unreachable
)]

mod abort;
pub mod config;
mod connect;
mod h2pool;
pub mod headers;
mod payload;
pub mod proxy;
pub mod resolve;
mod route_urls;
pub mod ssrf;
pub mod tls;
mod upgrade;

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use antirion_wire::decode::{Coding, DecodeError, decode_stream, parse_coding};
use bytes::Bytes;
use futures_util::{StreamExt, TryStreamExt};
use http::header::{CONTENT_ENCODING, CONTENT_LENGTH};
use http::{HeaderMap, Method, StatusCode, Uri};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, StreamBody};
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioTimer};
use parking_lot::Mutex;

pub use crate::abort::Abort;
use crate::connect::{ConnectFailure, Connector};
pub use crate::payload::{Payload, Tail, TailError, TailStream};
pub use crate::ssrf::{Denied, EgressPolicy, TrustClass};
pub use crate::tls::{HttpVersion, TlsProfile, TlsSettings};
pub use crate::upgrade::UpgradeAnswer;

/// Reasons of the egress rules.
pub mod reasons {
    antirion_error::reasons! {
        TLS_HANDSHAKE_FAILED = "tls_handshake_failed" => UpstreamUnavailable,
        EGRESS_DENIED = "egress_denied" => UpstreamUnavailable,
        UNEXPECTED_CONTENT_ENCODING = "unexpected_content_encoding" => UpstreamBadResponse,
    }
}

/// One upstream request.
#[derive(Debug)]
pub struct OutboundRequest {
    pub method: Method,
    pub uri: Uri,
    pub headers: HeaderMap,
    pub body: Payload,
    /// Bound on waiting for the response head.
    pub response_timeout: Duration,
    /// Bound on DNS, TCP and TLS of a new connection; `None` keeps the client's default.
    pub connect_timeout: Option<Duration>,
    /// Send on a new connection, never a pooled one.
    pub fresh_connection: bool,
    pub trust: TrustClass,
    pub tls: TlsProfile,
    pub policy: Arc<EgressPolicy>,
    /// Whether a request that a pooled connection's close cut off before any response is sent once
    /// more on a new connection. Callers whose requests must never be sent twice set it to false.
    pub resend_when_closed: bool,
}

/// Why an upstream response body ended before its end.
#[derive(Debug, thiserror::Error)]
pub enum BodyError {
    /// Reading failed: a reset, or a connection cut short.
    #[error("the upstream body could not be read")]
    Read(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// The bytes are not valid for the body's content coding.
    #[error("the upstream body is not valid for its content encoding")]
    Corrupt,
}

/// The body of an upstream response, decoded from its content coding.
pub type InboundBody = UnsyncBoxBody<Bytes, BodyError>;

/// The upstream's response head and its streaming, decoded body.
#[derive(Debug)]
pub struct InboundResponse {
    pub status: StatusCode,
    /// The upstream's headers, without `content-encoding` and `content-length` when the body was
    /// decoded.
    pub headers: HeaderMap,
    pub body: InboundBody,
    /// Cuts off the connection, for a reader that stops before the body's end. `None` on HTTP/2,
    /// where dropping the body resets only its stream.
    pub abort: Option<Abort>,
}

/// Why an upstream request produced no usable response.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("the URL scheme {0} is not supported")]
    UnsupportedScheme(String),
    #[error("the destination is denied by the egress rules: {0}")]
    EgressDenied(Denied),
    #[error("the destination name did not resolve")]
    Dns,
    #[error("could not connect to the upstream")]
    Connect,
    #[error("the connection to the upstream was not established in time")]
    ConnectTimeout,
    #[error("the upstream refused the request before processing it")]
    RefusedStream,
    #[error("the TLS handshake with the upstream failed")]
    Tls,
    #[error("the upstream did not answer in time")]
    Timeout,
    #[error("the connection was closed before a response")]
    Reset,
    #[error("the upstream response is in a content encoding the gateway cannot decode")]
    UnexpectedEncoding,
}

/// A pool and the count of connections its connector has opened.
type Pool = (
    Client<Connector, payload::PayloadBody>,
    Arc<std::sync::atomic::AtomicU64>,
);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PoolKey {
    trust: TrustClass,
    tls: u64,
    policy: u64,
    connect_timeout: Duration,
    fresh: bool,
    /// The thread of a single-threaded runtime: its connections are driven by that runtime, so a
    /// request reusing one wakes no other thread.
    thread: Option<std::thread::ThreadId>,
}

/// The calling thread, when it runs a single-threaded runtime.
fn runtime_thread() -> Option<std::thread::ThreadId> {
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::CurrentThread => {
            Some(std::thread::current().id())
        }
        _ => None,
    }
}

/// The connection was gone before the upstream could read the request: the client never sent it,
/// or the upstream reset it (a closed socket answers data with a reset), however the HTTP client
/// words that. A clean close after the request was written is not one: the upstream may have
/// read the request and dropped it, which is an attempt of its own.
fn closed_before_response(e: &hyper_util::client::legacy::Error) -> bool {
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        if let Some(h) = s.downcast_ref::<hyper::Error>()
            && (h.is_canceled() || h.is_closed())
        {
            return true;
        }
        if let Some(io) = s.downcast_ref::<std::io::Error>()
            && matches!(
                io.kind(),
                std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::NotConnected
            )
        {
            return true;
        }
        source = s.source();
    }
    false
}

/// The egress client. Cloning shares the connection pools.
#[derive(Clone)]
pub struct EgressClient {
    pools: Arc<Mutex<HashMap<PoolKey, Pool>>>,
    connect_timeout: Duration,
    resolver: resolve::Resolver,
    /// HTTP/2 with prior knowledge: for every plain `http` upstream when `prior_knowledge` is set,
    /// else for destinations whose profile asks for `h2c`.
    h2c: Arc<h2pool::H2Pool>,
    prior_knowledge: bool,
}

/// Connections per origin of the prior-knowledge pool when none is configured.
const DEFAULT_H2C_CONNECTIONS: usize = 64;

impl fmt::Debug for EgressClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EgressClient").finish_non_exhaustive()
    }
}

impl EgressClient {
    #[must_use]
    pub fn new(connect_timeout: Duration) -> EgressClient {
        EgressClient {
            pools: Arc::new(Mutex::new(HashMap::new())),
            connect_timeout,
            resolver: resolve::Resolver::System,
            h2c: Arc::new(h2pool::H2Pool::new(DEFAULT_H2C_CONNECTIONS)),
            prior_knowledge: false,
        }
    }

    /// The same client speaking HTTP/2 with prior knowledge to plain `http` upstreams, over at most
    /// `max_connections_per_origin` connections to each origin.
    #[must_use]
    pub fn with_prior_knowledge(mut self, max_connections_per_origin: usize) -> EgressClient {
        self.h2c = Arc::new(h2pool::H2Pool::new(max_connections_per_origin));
        self.prior_knowledge = true;
        self
    }

    /// The same client opening at most `max_connections_per_origin` HTTP/2 connections with prior
    /// knowledge to each origin whose profile asks for `h2c`.
    #[must_use]
    pub fn with_h2c_connections(mut self, max_connections_per_origin: usize) -> EgressClient {
        if !self.prior_knowledge {
            self.h2c = Arc::new(h2pool::H2Pool::new(max_connections_per_origin));
        }
        self
    }

    /// Whether a request goes over the prior-knowledge pool.
    fn prior(&self, r: &OutboundRequest) -> bool {
        match r.tls.http {
            HttpVersion::H2c => true,
            HttpVersion::Auto => self.prior_knowledge && r.uri.scheme_str() == Some("http"),
            HttpVersion::Http1 | HttpVersion::H2 => false,
        }
    }

    /// The same client resolving names with `resolver`.
    #[must_use]
    pub fn with_resolver(mut self, resolver: resolve::Resolver) -> EgressClient {
        self.resolver = resolver;
        self
    }

    /// The pool for one trust class, TLS profile and egress policy. Pools of an older policy are
    /// dropped, so a connection validated under other rules is never reused.
    fn key(&self, r: &OutboundRequest) -> PoolKey {
        PoolKey {
            trust: r.trust,
            tls: r.tls.key,
            policy: r.policy.fingerprint(),
            connect_timeout: r.connect_timeout.unwrap_or(self.connect_timeout),
            fresh: r.fresh_connection,
            thread: None,
        }
    }

    fn connector(&self, r: &OutboundRequest, opened: Arc<std::sync::atomic::AtomicU64>) -> Connector {
        Connector {
            policy: Arc::clone(&r.policy),
            trust: r.trust,
            tls: r.tls.clone(),
            connect_timeout: r.connect_timeout.unwrap_or(self.connect_timeout),
            resolver: self.resolver.clone(),
            opened,
        }
    }

    fn pool(&self, r: &OutboundRequest) -> Pool {
        let key = PoolKey {
            thread: runtime_thread(),
            ..self.key(r)
        };
        let mut pools = self.pools.lock();
        if let Some(p) = pools.get(&key) {
            return p.clone();
        }
        pools.retain(|k, _| k.policy == key.policy);
        let opened = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let connector = self.connector(r, Arc::clone(&opened));
        let mut builder = Client::builder(TokioExecutor::new());
        builder
            .pool_timer(TokioTimer::new())
            .pool_idle_timeout(Duration::from_secs(55));
        if key.fresh {
            builder.pool_max_idle_per_host(0);
        }
        let pool = (builder.build(connector), opened);
        pools.insert(key, pool.clone());
        pool
    }

    /// Sends one request: no redirects followed, and no resend except the one `resend_when_closed`
    /// allows.
    pub async fn send(&self, r: OutboundRequest) -> Result<InboundResponse, TransportError> {
        match r.uri.scheme_str() {
            Some("http" | "https") => {}
            other => return Err(TransportError::UnsupportedScheme(other.unwrap_or("").to_owned())),
        }
        let deadline = tokio::time::Instant::now() + r.response_timeout;
        if self.prior(&r) {
            let connector = self.connector(&r, Arc::new(std::sync::atomic::AtomicU64::new(0)));
            let (parts, body) = Box::pin(self.h2c.send(self.key(&r), &connector, &r, deadline)).await?;
            return decoded(parts, Box::pin(body), None);
        }
        let (pool, opened) = self.pool(&r);
        let build = || {
            let mut req = http::Request::new(r.body.body());
            *req.method_mut() = r.method.clone();
            *req.uri_mut() = r.uri.clone();
            *req.headers_mut() = r.headers.clone();
            req
        };
        let mut resent = false;
        let resp = loop {
            let before = opened.load(std::sync::atomic::Ordering::Relaxed);
            let result = tokio::time::timeout_at(deadline, pool.request(build())).await;
            let pooled = opened.load(std::sync::atomic::Ordering::Relaxed) == before;
            match result {
                Err(_) => return Err(TransportError::Timeout),
                Ok(Err(e)) if e.is_connect() && !tls_alert(&e) => return Err(connect_error(&e)),
                // A pooled connection the upstream closed while the request was being written
                // produced no response at all; the request is sent once more on a new connection.
                Ok(Err(e)) if r.resend_when_closed && !resent && pooled && closed_before_response(&e) => {
                    resent = true
                }
                Ok(Err(e)) if refused(&e) => return Err(TransportError::RefusedStream),
                Ok(Err(e)) if tls_alert(&e) => return Err(TransportError::Tls),
                Ok(Err(_)) => return Err(TransportError::Reset),
                Ok(Ok(resp)) => break resp,
            }
        };
        let (mut parts, body) = resp.into_parts();
        let abort = parts.extensions.remove::<Abort>();
        let frames = body
            .into_data_stream()
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>);
        decoded(parts, Box::pin(frames), abort)
    }
}

type Frames = std::pin::Pin<
    Box<dyn futures_util::Stream<Item = Result<Bytes, Box<dyn std::error::Error + Send + Sync>>> + Send>,
>;

/// The response with its body decoded from its content coding.
fn decoded(
    mut parts: http::response::Parts,
    frames: Frames,
    abort: Option<Abort>,
) -> Result<InboundResponse, TransportError> {
    let coding = parse_coding(
        parts
            .headers
            .get_all(CONTENT_ENCODING)
            .iter()
            .map(|v| v.as_bytes()),
    )
    .map_err(|_| TransportError::UnexpectedEncoding)?;
    if coding != Coding::Identity {
        parts.headers.remove(CONTENT_ENCODING);
        parts.headers.remove(CONTENT_LENGTH);
    }
    let decoded = decode_stream(coding, frames).map(|r| {
        r.map(http_body::Frame::data).map_err(|e| match e {
            DecodeError::Source(e) => BodyError::Read(e),
            DecodeError::Corrupt | DecodeError::TooLarge => BodyError::Corrupt,
        })
    });
    let body = BodyExt::boxed_unsync(StreamBody::new(decoded));
    Ok(InboundResponse {
        status: parts.status,
        headers: parts.headers,
        body,
        abort,
    })
}

/// An HTTP/2 stream the upstream refused before processing it: `RST_STREAM(REFUSED_STREAM)`, or a
/// `GOAWAY` whose last stream id is below the stream's, which h2 reports with the same reason.
fn refused(e: &hyper_util::client::legacy::Error) -> bool {
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        if let Some(h2) = s.downcast_ref::<h2::Error>() {
            return h2.reason() == Some(h2::Reason::REFUSED_STREAM) || (h2.is_go_away() && h2.is_remote());
        }
        source = s.source();
    }
    false
}

/// A TLS alert the upstream sent after the handshake: with TLS 1.3 a server that rejects the client
/// certificate says so at the first read.
fn tls_alert(e: &hyper_util::client::legacy::Error) -> bool {
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        let inner = s
            .downcast_ref::<std::io::Error>()
            .and_then(std::io::Error::get_ref)
            .and_then(|i| i.downcast_ref::<rustls::Error>());
        if let Some(rustls::Error::AlertReceived(_)) = s.downcast_ref::<rustls::Error>().or(inner) {
            return true;
        }
        source = s.source();
    }
    false
}

fn connect_error(e: &hyper_util::client::legacy::Error) -> TransportError {
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        if let Some(f) = s.downcast_ref::<ConnectFailure>() {
            return connect_failure(f);
        }
        source = s.source();
    }
    TransportError::Connect
}

pub(crate) fn connect_failure(f: &ConnectFailure) -> TransportError {
    match f {
        ConnectFailure::Denied(d) => TransportError::EgressDenied(*d),
        ConnectFailure::Dns => TransportError::Dns,
        ConnectFailure::Tls => TransportError::Tls,
        ConnectFailure::Timeout => TransportError::ConnectTimeout,
        ConnectFailure::Io(_) | ConnectFailure::Proxy(_) => TransportError::Connect,
    }
}

#[cfg(test)]
mod resend_tests;

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

    use super::*;

    /// An HTTP/1.1 server answering every request with an endless body; it reports how each
    /// connection's writing ended.
    async fn endless(listener: TcpListener, accepted: Arc<AtomicUsize>, ended: mpsc::Sender<io::Result<()>>) {
        let mut conns = tokio::task::JoinSet::new();
        while let Ok((mut sock, _)) = listener.accept().await {
            accepted.fetch_add(1, Ordering::SeqCst);
            let ended = ended.clone();
            conns.spawn(async move {
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(buf.get(..n).unwrap_or_default()),
                    }
                }
                let chunk = vec![b'A'; 65536];
                let mut result = sock
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 1099511627776\r\n\r\n")
                    .await;
                while result.is_ok() {
                    result = sock.write_all(&chunk).await;
                }
                let _ = ended.send(result).await;
            });
        }
    }

    fn get(addr: std::net::SocketAddr) -> OutboundRequest {
        OutboundRequest {
            method: Method::GET,
            uri: format!("http://{addr}/big").parse().unwrap(),
            headers: HeaderMap::new(),
            body: Bytes::new().into(),
            response_timeout: Duration::from_secs(5),
            connect_timeout: None,
            fresh_connection: false,
            trust: TrustClass::Operator,
            tls: TlsProfile::default_profile().unwrap(),
            policy: Arc::new(EgressPolicy::default()),
            resend_when_closed: false,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_aborted_response_closes_its_connection_and_never_returns_to_the_pool() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accepted = Arc::new(AtomicUsize::new(0));
        let (tx, mut ended) = mpsc::channel(4);
        let mut server = tokio::task::JoinSet::new();
        server.spawn(endless(listener, Arc::clone(&accepted), tx));
        let client = EgressClient::new(Duration::from_secs(2));
        for round in 1..=2 {
            let resp = client.send(get(addr)).await.unwrap();
            let abort = resp.abort.clone().unwrap();
            let mut body = resp.body.into_data_stream();
            let mut read = 0;
            while read < 1 << 20 {
                read += body.next().await.unwrap().unwrap().len();
            }
            drop(body);
            tokio::time::timeout(Duration::from_secs(2), abort.abort())
                .await
                .unwrap();
            let end = tokio::time::timeout(Duration::from_secs(2), ended.recv())
                .await
                .unwrap();
            assert!(
                end.unwrap().is_err(),
                "the upstream's write fails once the connection is cut"
            );
            assert_eq!(
                accepted.load(Ordering::SeqCst),
                round,
                "an aborted connection is not reused"
            );
        }
        server.abort_all();
    }
}
