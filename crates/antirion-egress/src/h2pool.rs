//! HTTP/2 with prior knowledge, fanned out over several connections per origin: a request takes a
//! stream on a connection that has one free under its peer's `SETTINGS_MAX_CONCURRENT_STREAMS`, and
//! a new connection is opened when every one is full, up to a bound per origin. Beyond it, requests
//! wait for a free stream within the connect timeout.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use bytes::Bytes;
use futures_util::Stream;
use http::header::{CONNECTION, HOST, TE, TRANSFER_ENCODING, UPGRADE};
use http::{HeaderMap, HeaderName, Uri};
use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio::task::JoinSet;
use tokio::time::Instant;
use tower::Service;

use crate::connect::Connector;
use crate::{OutboundRequest, PoolKey, TransportError, connect_failure};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Header fields HTTP/2 forbids (RFC 9113 §8.2.2); `host` travels as `:authority`.
const HOP_BY_HOP: [HeaderName; 5] = [
    CONNECTION,
    TRANSFER_ENCODING,
    UPGRADE,
    HOST,
    HeaderName::from_static("keep-alive"),
];

/// How often a request waiting for a free stream looks again, in case it missed a wake-up.
const RECHECK: std::time::Duration = std::time::Duration::from_millis(50);

enum Pick {
    Slot(Slot),
    Connect(Arc<Notify>),
    Wait(Arc<Notify>),
}

struct H2Conn {
    send: h2::client::SendRequest<Bytes>,
    active: AtomicUsize,
    closed: AtomicBool,
}

impl H2Conn {
    fn has_room(&self) -> bool {
        !self.closed.load(Ordering::Acquire)
            && self.active.load(Ordering::Acquire) < self.send.current_max_send_streams()
    }
}

#[derive(Default)]
struct Origin {
    conns: Vec<Arc<H2Conn>>,
    connecting: usize,
    freed: Arc<Notify>,
}

/// One stream on a connection; dropping it frees the stream.
struct Slot {
    conn: Arc<H2Conn>,
    freed: Arc<Notify>,
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.conn.active.fetch_sub(1, Ordering::AcqRel);
        self.freed.notify_waiters();
    }
}

/// The connections of every origin reached with prior knowledge.
pub(crate) struct H2Pool {
    origins: Mutex<HashMap<(PoolKey, String), Origin>>,
    max_per_origin: usize,
    drivers: Mutex<JoinSet<()>>,
}

impl H2Pool {
    pub(crate) fn new(max_per_origin: usize) -> H2Pool {
        H2Pool {
            origins: Mutex::new(HashMap::new()),
            max_per_origin: max_per_origin.max(1),
            drivers: Mutex::new(JoinSet::new()),
        }
    }

    /// Sends one request and returns the response head and its body.
    pub(crate) async fn send(
        &self,
        pool: PoolKey,
        connector: &Connector,
        r: &OutboundRequest,
        deadline: Instant,
    ) -> Result<(http::response::Parts, H2Body), TransportError> {
        let authority = r.uri.authority().map(ToString::to_string).unwrap_or_default();
        let slot = if r.fresh_connection {
            let freed = Arc::new(Notify::new());
            let conn = self.open(connector, &r.uri, Arc::clone(&freed)).await?;
            conn.active.fetch_add(1, Ordering::AcqRel);
            Slot { conn, freed }
        } else {
            self.slot((pool, authority), connector, &r.uri, deadline).await?
        };
        let mut req = http::Request::new(());
        *req.method_mut() = r.method.clone();
        *req.uri_mut() = r.uri.clone();
        *req.headers_mut() = h2_headers(&r.headers);
        let mut send = slot.conn.send.clone().ready().await.map_err(|e| failure(&e))?;
        let (response, mut body) = send
            .send_request(req, r.body.is_empty())
            .map_err(|e| failure(&e))?;
        let parts = r.body.parts();
        let tail = r.body.tail().cloned();
        for (i, part) in parts.iter().enumerate() {
            body.send_data(part.clone(), tail.is_none() && i + 1 == parts.len())
                .map_err(|e| failure(&e))?;
        }
        let response = tokio::time::timeout_at(deadline, response);
        let (resp, pumping) = match tail {
            None => (response.await, None),
            Some(tail) => {
                let mut pumping: Option<Pump> = Some(Box::pin(pump(body, tail)));
                tokio::pin!(response);
                loop {
                    tokio::select! {
                        r = &mut response => break (r, pumping),
                        sent = async { match pumping.as_mut() {
                            Some(p) => p.await,
                            None => std::future::pending().await,
                        } } => {
                            pumping = None;
                            if sent.is_err() {
                                return Err(TransportError::Reset);
                            }
                        }
                    }
                }
            }
        };
        let resp = match resp {
            Err(_) => return Err(TransportError::Timeout),
            Ok(Err(e)) => return Err(failure(&e)),
            Ok(Ok(resp)) => resp,
        };
        let (parts, recv) = resp.into_parts();
        Ok((
            parts,
            H2Body {
                recv,
                pumping,
                _slot: slot,
            },
        ))
    }

    /// A free stream on a connection to the origin, opening a connection when none has one.
    async fn slot(
        &self,
        key: (PoolKey, String),
        connector: &Connector,
        uri: &Uri,
        deadline: Instant,
    ) -> Result<Slot, TransportError> {
        loop {
            let freed = match self.take(&key) {
                Pick::Slot(slot) => return Ok(slot),
                Pick::Wait(freed) => {
                    let recheck = (Instant::now() + RECHECK).min(deadline);
                    let _ = tokio::time::timeout_at(recheck, freed.notified()).await;
                    if Instant::now() >= deadline {
                        return Err(TransportError::ConnectTimeout);
                    }
                    continue;
                }
                Pick::Connect(freed) => freed,
            };
            let opened = self.open(connector, uri, Arc::clone(&freed)).await;
            return self.add(&key, opened, freed);
        }
    }

    fn take(&self, key: &(PoolKey, String)) -> Pick {
        let mut origins = self.origins.lock();
        let origin = origins.entry(key.clone()).or_default();
        origin.conns.retain(|c| !c.closed.load(Ordering::Acquire));
        if let Some(conn) = origin.conns.iter().find(|c| c.has_room()) {
            conn.active.fetch_add(1, Ordering::AcqRel);
            return Pick::Slot(Slot {
                conn: Arc::clone(conn),
                freed: Arc::clone(&origin.freed),
            });
        }
        let freed = Arc::clone(&origin.freed);
        if origin.conns.len() + origin.connecting < self.max_per_origin {
            origin.connecting += 1;
            Pick::Connect(freed)
        } else {
            Pick::Wait(freed)
        }
    }

    fn add(
        &self,
        key: &(PoolKey, String),
        opened: Result<Arc<H2Conn>, TransportError>,
        freed: Arc<Notify>,
    ) -> Result<Slot, TransportError> {
        let mut origins = self.origins.lock();
        let origin = origins.entry(key.clone()).or_default();
        origin.connecting = origin.connecting.saturating_sub(1);
        let conn = match opened {
            Ok(conn) => conn,
            Err(e) => {
                freed.notify_waiters();
                return Err(e);
            }
        };
        conn.active.fetch_add(1, Ordering::AcqRel);
        origin.conns.push(Arc::clone(&conn));
        Ok(Slot { conn, freed })
    }

    /// Opens a connection and waits for the peer's settings (one PING round trip), so its stream
    /// limit is known before any stream is opened on it.
    async fn open(
        &self,
        connector: &Connector,
        uri: &Uri,
        freed: Arc<Notify>,
    ) -> Result<Arc<H2Conn>, TransportError> {
        let stream = connector
            .clone()
            .call(uri.clone())
            .await
            .map_err(|e| connect_failure(&e))?
            .into_stream();
        let (send, mut connection) = h2::client::Builder::new()
            .initial_window_size(1 << 20)
            .initial_connection_window_size(4 << 20)
            .handshake::<_, Bytes>(stream)
            .await
            .map_err(|_| TransportError::Connect)?;
        let ping = connection.ping_pong();
        let conn = Arc::new(H2Conn {
            send,
            active: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
        });
        {
            let conn = Arc::clone(&conn);
            let mut drivers = self.drivers.lock();
            while drivers.try_join_next().is_some() {}
            drivers.spawn(async move {
                let _ = connection.await;
                conn.closed.store(true, Ordering::Release);
                freed.notify_waiters();
            });
        }
        if let Some(mut ping) = ping {
            ping.ping(h2::Ping::opaque())
                .await
                .map_err(|_| TransportError::Connect)?;
        }
        Ok(conn)
    }
}

/// The request headers as HTTP/2 allows them.
fn h2_headers(headers: &HeaderMap) -> HeaderMap {
    let mut out = headers.clone();
    for name in &HOP_BY_HOP {
        out.remove(name);
    }
    if out.get(TE).is_some_and(|v| v != "trailers") {
        out.remove(TE);
    }
    out
}

fn failure(e: &h2::Error) -> TransportError {
    if e.reason() == Some(h2::Reason::REFUSED_STREAM) || (e.is_go_away() && e.is_remote()) {
        TransportError::RefusedStream
    } else {
        TransportError::Reset
    }
}

/// The rest of a request body still being sent.
type Pump = std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ()>> + Send>>;

/// Sends a body's streamed tail on its HTTP/2 stream, within the flow-control window, and ends the
/// stream. A tail error resets the stream.
async fn pump(mut body: h2::SendStream<Bytes>, tail: crate::payload::Tail) -> Result<(), ()> {
    use futures_util::StreamExt;
    let Some(mut stream) = tail.take() else {
        body.send_reset(h2::Reason::CANCEL);
        return Err(());
    };
    while let Some(chunk) = stream.next().await {
        let Ok(mut chunk) = chunk else {
            body.send_reset(h2::Reason::CANCEL);
            return Err(());
        };
        while !chunk.is_empty() {
            body.reserve_capacity(chunk.len());
            let granted = futures_util::future::poll_fn(|cx| body.poll_capacity(cx)).await;
            let n = match granted {
                Some(Ok(n)) if n > 0 => n.min(chunk.len()),
                Some(Ok(_)) => continue,
                _ => return Err(()),
            };
            body.send_data(chunk.split_to(n), false).map_err(|_| ())?;
        }
    }
    body.send_data(Bytes::new(), true).map_err(|_| ())
}

/// A response body read from its stream, releasing flow-control capacity as it goes; the stream
/// stays counted against its connection until the body is dropped. A request body still streaming
/// when the response began keeps being sent while the response is read: the stream is full duplex.
pub(crate) struct H2Body {
    recv: h2::RecvStream,
    pumping: Option<Pump>,
    _slot: Slot,
}

impl Stream for H2Body {
    type Item = Result<Bytes, BoxError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        if let Some(p) = self.pumping.as_mut()
            && p.as_mut().poll(cx).is_ready()
        {
            self.pumping = None;
        }
        match self.recv.poll_data(cx) {
            std::task::Poll::Ready(Some(Ok(bytes))) => {
                let _ = self.recv.flow_control().release_capacity(bytes.len());
                std::task::Poll::Ready(Some(Ok(bytes)))
            }
            std::task::Poll::Ready(Some(Err(e))) => std::task::Poll::Ready(Some(Err(Box::new(e)))),
            std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use http::Method;
    use http_body_util::BodyExt;
    use tokio::net::TcpListener;

    use crate::{EgressClient, EgressPolicy, TlsProfile, TrustClass};

    use super::*;

    /// An h2c server allowing two concurrent streams per connection; each answer takes 200 ms.
    async fn server(listener: TcpListener, conns: Arc<AtomicUsize>, peak: Arc<AtomicUsize>) {
        let mut set = JoinSet::new();
        while let Ok((sock, _)) = listener.accept().await {
            conns.fetch_add(1, Ordering::SeqCst);
            let peak = Arc::clone(&peak);
            set.spawn(async move {
                let mut conn = h2::server::Builder::new()
                    .max_concurrent_streams(2)
                    .handshake::<_, Bytes>(sock)
                    .await
                    .unwrap();
                let active = Arc::new(AtomicUsize::new(0));
                let mut streams = JoinSet::new();
                while let Some(Ok((_req, mut respond))) = conn.accept().await {
                    let n = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(n, Ordering::SeqCst);
                    let active = Arc::clone(&active);
                    streams.spawn(async move {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        active.fetch_sub(1, Ordering::SeqCst);
                        let mut body = respond.send_response(http::Response::new(()), false).unwrap();
                        body.send_data(Bytes::from_static(b"ok"), true).unwrap();
                    });
                }
            });
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn requests_beyond_a_connections_stream_limit_open_more_connections() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (conns, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let mut tasks = JoinSet::new();
        tasks.spawn(server(listener, Arc::clone(&conns), Arc::clone(&peak)));
        let client = EgressClient::new(Duration::from_secs(2)).with_prior_knowledge(8);
        let policy = Arc::new(EgressPolicy::default());
        let tls = TlsProfile::default_profile().unwrap();
        let started = Instant::now();
        let mut requests = JoinSet::new();
        for _ in 0..6 {
            let client = client.clone();
            let r = OutboundRequest {
                method: Method::POST,
                uri: format!("http://{addr}/v1/chat/completions").parse().unwrap(),
                headers: HeaderMap::new(),
                body: Bytes::from_static(b"{}").into(),
                response_timeout: Duration::from_secs(5),
                connect_timeout: None,
                fresh_connection: false,
                trust: TrustClass::Operator,
                tls: tls.clone(),
                policy: Arc::clone(&policy),
                resend_when_closed: true,
            };
            requests.spawn(async move {
                let resp = client.send(r).await.unwrap();
                resp.body.collect().await.unwrap().to_bytes()
            });
        }
        while let Some(body) = requests.join_next().await {
            assert_eq!(body.unwrap(), Bytes::from_static(b"ok"));
        }
        assert!(
            started.elapsed() < Duration::from_millis(600),
            "{:?}",
            started.elapsed()
        );
        assert!(conns.load(Ordering::SeqCst) >= 3);
        assert!(peak.load(Ordering::SeqCst) <= 2);
    }

    /// An h2c server that answers at once, then echoes each request chunk until the request ends.
    async fn echo(listener: TcpListener) {
        let (sock, _) = listener.accept().await.unwrap();
        let mut conn = h2::server::handshake(sock).await.unwrap();
        let mut streams = JoinSet::new();
        while let Some(Ok((req, mut respond))) = conn.accept().await {
            streams.spawn(async move {
                let mut out = respond.send_response(http::Response::new(()), false).unwrap();
                let mut body = req.into_body();
                while let Some(Ok(chunk)) = body.data().await {
                    let _ = body.flow_control().release_capacity(chunk.len());
                    out.send_data(chunk, false).unwrap();
                }
                out.send_data(Bytes::new(), true).unwrap();
            });
        }
    }

    #[tokio::test]
    async fn a_request_body_keeps_streaming_after_the_response_began() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut tasks = JoinSet::new();
        tasks.spawn(echo(listener));
        let client = EgressClient::new(Duration::from_secs(2)).with_prior_knowledge(1);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<Bytes, crate::payload::TailError>>();
        let tail = crate::payload::Tail::new(
            Box::pin(futures_util::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|x| (x, rx))
            })),
            None,
        );
        let r = OutboundRequest {
            method: Method::POST,
            uri: format!("http://{addr}/duplex").parse().unwrap(),
            headers: HeaderMap::new(),
            body: crate::Payload::from_parts(vec![Bytes::from_static(b"one;")]).with_tail(tail),
            response_timeout: Duration::from_secs(5),
            connect_timeout: None,
            fresh_connection: false,
            trust: TrustClass::Operator,
            tls: TlsProfile::default_profile().unwrap(),
            policy: Arc::new(EgressPolicy::default()),
            resend_when_closed: false,
        };
        let mut resp = client.send(r).await.unwrap();
        let first = resp.body.frame().await.unwrap().unwrap().into_data().unwrap();
        assert_eq!(first, Bytes::from_static(b"one;"));
        tx.send(Ok(Bytes::from_static(b"two"))).unwrap();
        let second = resp.body.frame().await.unwrap().unwrap().into_data().unwrap();
        assert_eq!(second, Bytes::from_static(b"two"), "sent after the response head");
        drop(tx);
        assert!(
            resp.body.collect().await.unwrap().to_bytes().is_empty(),
            "the request ended, so did the answer"
        );
    }
}
