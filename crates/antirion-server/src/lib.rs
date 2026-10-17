//! The listeners: the data port (HTTP/1.1, and HTTP/2 with prior knowledge) and the admin port,
//! with connection tracking and graceful shutdown.

#![forbid(unsafe_code)]

mod clock;
mod compress;
mod head;
mod listen;
mod local;
mod threads;
mod tls;
mod waiting;

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use antirion_core::{Peer, RequestId, Stage};
use antirion_error::reason::hygiene;
use antirion_error::{GatewayError, Message};
use antirion_pipeline::DataPlane;
use antirion_runtime::Runtime;
use antirion_runtime::security::SecurityRt;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto;
use hyper_util::service::TowerToHyperService;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

use crate::head::{First, Rewind, read_first};
use crate::listen::DataListener;
pub use crate::threads::DataThreads;
pub use crate::tls::TlsLoadError;
use crate::tls::{Plane, Reloading};

/// How long an admin client may take to send a request head or finish a TLS handshake.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a connection gets to write what it has once the node stops.
const CLOSE_GRACE: Duration = Duration::from_secs(1);

/// Why the listeners could not run.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("cannot listen on {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        source: std::io::Error,
    },
    #[error("cannot set up TLS on the {plane} listener: {source}")]
    Tls {
        plane: &'static str,
        source: TlsLoadError,
    },
}

/// Bound listeners, before serving.
#[derive(Debug)]
pub struct Bound {
    data: TcpListener,
    admin: TcpListener,
}

impl Bound {
    pub async fn bind(data: SocketAddr, admin: SocketAddr) -> Result<Bound, ServeError> {
        let data_l = listen::listener(data, listen::PER_THREAD)
            .map_err(|source| ServeError::Bind { addr: data, source })?;
        let admin_l = TcpListener::bind(admin)
            .await
            .map_err(|source| ServeError::Bind { addr: admin, source })?;
        Ok(Bound {
            data: data_l,
            admin: admin_l,
        })
    }

    #[must_use]
    pub fn data_addr(&self) -> Option<SocketAddr> {
        self.data.local_addr().ok()
    }

    #[must_use]
    pub fn admin_addr(&self) -> Option<SocketAddr> {
        self.admin.local_addr().ok()
    }
}

/// Limits of the HTTP stack itself sit above the gateway's own header limits, so oversized header
/// sections reach the data plane and get its JSON 431. HTTP/2 header blocks need the larger bound
/// because the stack derives its CONTINUATION-flood cut-off from it.
const H1_MAX_HEADERS: usize = 1024;
const H2_MAX_HEADER_LIST: u32 = 2 << 20;

fn builder() -> auto::Builder<TokioExecutor> {
    let mut b = auto::Builder::new(TokioExecutor::new());
    b.http1()
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_READ_TIMEOUT)
        .max_headers(H1_MAX_HEADERS);
    b.http2()
        .max_header_list_size(H2_MAX_HEADER_LIST)
        .timer(TokioTimer::new())
        .max_concurrent_streams(256)
        .initial_stream_window_size(1 << 20)
        .initial_connection_window_size(4 << 20)
        .adaptive_window(true)
        .keep_alive_interval(Duration::from_secs(30))
        .keep_alive_timeout(Duration::from_secs(10));
    b
}

/// The TLS acceptors of both listeners, loaded from the current snapshot.
#[derive(Debug)]
pub struct ListenerTls {
    data: Option<Arc<Reloading>>,
    admin: Option<Arc<Reloading>>,
}

impl ListenerTls {
    /// Reads the certificate pairs the configuration names; a listener without one is plaintext.
    pub async fn load(rt: &Runtime) -> Result<ListenerTls, ServeError> {
        ListenerTls::for_security(&rt.gens.load().security).await
    }

    /// The listeners' TLS as compiled `security` settings configure it.
    pub async fn for_security(security: &SecurityRt) -> Result<ListenerTls, ServeError> {
        let load = |plane: Plane| async move {
            Reloading::start(security, plane)
                .await
                .map_err(|source| ServeError::Tls {
                    plane: if plane == Plane::Data { "data" } else { "admin" },
                    source,
                })
        };
        Ok(ListenerTls {
            data: load(Plane::Data).await?,
            admin: load(Plane::Admin).await?,
        })
    }
}

/// Serves both listeners until `stop` is cancelled. The listeners stop accepting when the drain
/// closes them. Once the drain begins, every HTTP/1.1 data response carries `Connection: close`, so
/// each such connection closes after its next response; when the listeners close, the idle ones
/// close too, with a FIN. An HTTP/2 data connection open when the drain began gets `GOAWAY` then;
/// admin connections and HTTP/2 data connections accepted later are asked to close when the
/// listeners close. Data connections are served on `threads`.
pub async fn serve(
    rt: Arc<Runtime>,
    bound: Bound,
    tls: ListenerTls,
    threads: Arc<DataThreads>,
    stop: CancellationToken,
) {
    let data = DataPlane::new(Arc::clone(&rt));
    rt.state.playground.install(Arc::new(local::LocalData {
        rt: Arc::downgrade(&rt),
    }));
    rt.state
        .guard_tests
        .install(Arc::new(antirion_pipeline::guardtest::Tester {
            rt: Arc::downgrade(&rt),
        }));
    let admin = antirion_admin::router(Arc::clone(&rt));
    let mut accepted = tokio::task::JoinSet::new();
    for reloading in [&tls.data, &tls.admin].into_iter().flatten() {
        accepted.spawn(Arc::clone(reloading).watch(Arc::clone(&rt), stop.clone()));
    }
    let accept = |listener: DataListener, local: bool| {
        accept_data(
            Arc::clone(&rt),
            listener,
            tls.data.clone(),
            data.clone(),
            Arc::clone(&threads),
            local,
            stop.clone(),
        )
    };
    if listen::PER_THREAD {
        let addr = bound.data.local_addr();
        let mut first = bound.data.into_std().ok().map(DataListener::Moved);
        for on in threads.handles() {
            let listener = match (first.take(), &addr) {
                (Some(l), _) => l,
                (None, Ok(addr)) => DataListener::Joined(*addr),
                (None, Err(e)) => {
                    tracing::warn!(error = %e, "the data listener has no address to share");
                    break;
                }
            };
            accepted.spawn_on(accept(listener, true), on);
        }
    } else {
        accepted.spawn(accept(DataListener::Ready(bound.data), false));
    }
    accepted.spawn(accept_admin(
        Arc::clone(&rt),
        bound.admin,
        tls.admin,
        admin,
        stop.clone(),
    ));
    while accepted.join_next().await.is_some() {}
    stop.cancelled().await;
}

/// When a connection is asked to close gracefully, besides the node stopping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseAt {
    DrainStart,
    ListenersClose,
}

#[derive(Clone)]
struct Closing {
    rt: Arc<Runtime>,
    at: CloseAt,
    stop: CancellationToken,
    /// HTTP/1.1 data connections close through their clock rather than the HTTP stack, which would
    /// close one idle in keep-alive while a request is arriving on it.
    clock: Option<Arc<clock::ConnClock>>,
}

impl Closing {
    fn new(rt: &Arc<Runtime>, data: bool, stop: &CancellationToken) -> Closing {
        let at = if data && !rt.drain.is_draining() {
            CloseAt::DrainStart
        } else {
            CloseAt::ListenersClose
        };
        Closing {
            rt: Arc::clone(rt),
            at,
            stop: stop.clone(),
            clock: None,
        }
    }

    /// An HTTP/1.1 data connection: its responses carry `Connection: close` from the drain start,
    /// and it closes once idle when the listeners close.
    fn http1_data(self, clock: Arc<clock::ConnClock>) -> Closing {
        Closing {
            at: CloseAt::ListenersClose,
            clock: Some(clock),
            ..self
        }
    }

    async fn requested(&self) {
        match self.at {
            CloseAt::DrainStart => self.rt.drain.begun.cancelled().await,
            CloseAt::ListenersClose => self.rt.drain.closed.cancelled().await,
        }
    }
}

/// Serves a connection until it ends; once closing is requested, it finishes the requests it has
/// (HTTP/1.1 closes once no request is in progress or arriving, HTTP/2 sends `GOAWAY`). When the
/// node stops, other connections get `CLOSE_GRACE` to finish; an HTTP/1.1 data connection is never
/// cut while a request on it is read or answered. A macro rather than a generic function, because
/// the connection's error type defeats the `Send` proof of a generic future.
macro_rules! drive {
    ($conn:expr, $closing:expr) => {
        drive!($conn, $closing, std::future::pending::<()>())
    };
    ($conn:expr, $closing:expr, $expired:expr) => {{
        let conn = $conn;
        let closing: Closing = $closing;
        let expired = $expired;
        tokio::pin!(conn);
        tokio::pin!(expired);
        let mut asked = false;
        let mut stopping = false;
        loop {
            let ask = tokio::select! {
                r = conn.as_mut() => break r,
                () = expired.as_mut() => break Ok(()),
                () = closing.requested(), if !asked => true,
                () = closing.stop.cancelled(), if !stopping => {
                    stopping = true;
                    if closing.clock.is_none() {
                        if !asked {
                            conn.as_mut().graceful_shutdown();
                        }
                        break tokio::time::timeout(CLOSE_GRACE, conn.as_mut())
                            .await
                            .unwrap_or(Ok(()));
                    }
                    !asked
                }
            };
            if ask {
                asked = true;
                match &closing.clock {
                    Some(clock) => clock.close_when_idle(),
                    None => conn.as_mut().graceful_shutdown(),
                }
            }
        }
    }};
}

/// A response body that counts its request as in progress, for the drain and for its connection,
/// until the body ends or is dropped.
struct Tracked {
    inner: antirion_pipeline::Body,
    _active: Option<antirion_runtime::drain::Active>,
    _request: clock::InRequest,
}

impl http_body::Body for Tracked {
    type Data = bytes::Bytes;
    type Error = antirion_pipeline::BoxError;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        std::pin::Pin::new(&mut self.inner).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

/// Accepts data connections until the listeners close; `local` serves them on the calling data
/// thread, otherwise they are handed to the data threads.
async fn accept_data(
    rt: Arc<Runtime>,
    listener: DataListener,
    tls: Option<Arc<Reloading>>,
    data: DataPlane,
    threads: Arc<DataThreads>,
    local: bool,
    stop: CancellationToken,
) {
    let listener = match listener.open() {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = %e, "a data thread could not listen; the others accept its share");
            return;
        }
    };
    loop {
        let (stream, addr) = tokio::select! {
            () = stop.cancelled() => return,
            () = rt.drain.closed.cancelled() => return,
            accepted = listener.accept() => match accepted {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::warn!(error = %e, "accept failed on the data listener");
                    continue;
                }
            },
        };
        let _ = stream.set_nodelay(true);
        let data = data.clone();
        let closing = Closing::new(&rt, true, &stop);
        let acceptor = tls.as_ref().map(|t| t.acceptor());
        let here = local.then(|| rt.tasks.clone());
        let rt = Arc::clone(&rt);
        let spread = Arc::clone(&threads);
        let serve = move |stream: TcpStream| async move {
            match acceptor {
                None => {
                    let peer = Peer {
                        addr,
                        tls: false,
                        client_cert: None,
                    };
                    serve_data_connection(stream, peer, data, rt, closing, spread).await;
                }
                Some(acceptor) => {
                    let limit = rt.statics.http.header_read;
                    if let Some(s) = handshake(&acceptor, stream, limit).await {
                        let peer = Peer {
                            addr,
                            tls: true,
                            client_cert: tls::client_certificate(s.get_ref().1),
                        };
                        serve_data_connection(s, peer, data, rt, closing, spread).await;
                    }
                }
            }
        };
        match here {
            Some(tasks) => {
                tasks.spawn(serve(stream));
            }
            None => threads.serve(stream, serve),
        }
    }
}

/// The TLS handshake of a new connection, bounded like a request head.
async fn handshake(
    acceptor: &tokio_rustls::TlsAcceptor,
    stream: TcpStream,
    limit: Duration,
) -> Option<tokio_rustls::server::TlsStream<TcpStream>> {
    match tokio::time::timeout(limit, acceptor.accept(stream)).await {
        Ok(Ok(s)) => Some(s),
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "a TLS handshake failed");
            None
        }
        Err(_) => None,
    }
}

async fn serve_data_connection<S>(
    mut stream: S,
    peer: Peer,
    data: DataPlane,
    rt: Arc<Runtime>,
    closing: Closing,
    threads: Arc<DataThreads>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let timeouts = rt.statics.http;
    let prefix = match read_first(&mut stream, timeouts.header_read).await {
        Ok(First::Serve(prefix)) => prefix,
        Ok(First::Silent) | Err(_) => return,
        Ok(First::BadFraming { path }) => {
            let e = GatewayError::with_reason(
                hygiene::FRAMING_ERROR,
                Stage::Accept,
                Message::Fixed("The request framing is ambiguous."),
            );
            reject(&mut stream, &path, &e).await;
            return;
        }
        Ok(First::TooLarge { path }) => {
            let e = GatewayError::new(
                antirion_error::Code::HeadersTooLarge,
                Stage::Accept,
                Message::Fixed("The request headers are too large."),
            );
            reject(&mut stream, &path, &e).await;
            return;
        }
        Ok(First::Incomplete { path }) => {
            let e = GatewayError::with_reason(
                reasons::HEADER_READ_TIMEOUT,
                Stage::Accept,
                Message::Fixed("The request head did not arrive in time."),
            );
            reject(&mut stream, &path, &e).await;
            return;
        }
    };
    let http1 = !prefix.starts_with(head::H2_PREFACE);
    let conn_clock = clock::ConnClock::new(http1, timeouts.header_read, timeouts.idle);
    conn_clock.read(prefix.len());
    let closing = if http1 {
        closing.http1_data(Arc::clone(&conn_clock))
    } else {
        closing
    };
    let io = TokioIo::new(clock::Clocked::new(
        Rewind::new(prefix, stream),
        Arc::clone(&conn_clock),
    ));
    let compression = Arc::new(compress::Settings::from_statics(&rt.statics.doc));
    let requests = Arc::clone(&conn_clock);
    let service = service_fn(move |mut req: http::Request<hyper::body::Incoming>| {
        let data = data.clone();
        let rt = Arc::clone(&rt);
        let compression = Arc::clone(&compression);
        let threads = Arc::clone(&threads);
        let request = requests.begin();
        req.extensions_mut().insert(peer.clone());
        async move {
            let probe = matches!(req.uri().path(), "/-/livez" | "/-/readyz");
            let http1 = req.version() <= http::Version::HTTP_11;
            let codec = (req.method() != http::Method::HEAD)
                .then(|| compression.choose(req.headers()))
                .flatten();
            let active = (!probe).then(|| rt.drain.enter());
            // An HTTP/2 connection's streams run concurrently, so they are spread over the threads.
            let on = if http1 { None } else { threads.spread() };
            let mut resp = data.serve_on(req, on).await;
            if let Some(codec) = codec {
                resp = compression.apply(resp, codec).await;
            }
            if http1 && rt.drain.is_draining() {
                resp.headers_mut()
                    .insert(http::header::CONNECTION, http::HeaderValue::from_static("close"));
            }
            let resp = resp.map(|inner| {
                http_body_util::BodyExt::boxed(Tracked {
                    inner,
                    _active: active,
                    _request: request,
                })
            });
            Ok::<_, Infallible>(resp)
        }
    });
    let mut b = builder();
    b.http1().header_read_timeout(None);
    let conn = b.serve_connection_with_upgrades(io, service).into_owned();
    if let Err(e) = drive!(conn, closing, conn_clock.expired()) {
        tracing::debug!(error = %e, "a data-plane connection ended with an error");
    }
}

/// Writes an HTTP/1.1 error response straight to the socket and closes it.
async fn reject<S: AsyncWrite + Unpin>(stream: &mut S, path: &str, err: &GatewayError) {
    let envelope = antirion_dialects::envelope_for_path(path);
    let rendered = antirion_error::render(err, envelope, &RequestId::generate(), None);
    let mut head = format!(
        "HTTP/1.1 {} {}\r\n",
        rendered.status.as_u16(),
        rendered.status.canonical_reason().unwrap_or("")
    );
    for (name, value) in &rendered.headers {
        head.push_str(&format!(
            "{}: {}\r\n",
            name.as_str(),
            String::from_utf8_lossy(value.as_bytes())
        ));
    }
    head.push_str(&format!(
        "x-content-type-options: nosniff\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        rendered.body.len()
    ));
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(&rendered.body).await;
    let _ = stream.shutdown().await;
}

async fn accept_admin(
    rt: Arc<Runtime>,
    listener: TcpListener,
    tls: Option<Arc<Reloading>>,
    router: axum::Router,
    stop: CancellationToken,
) {
    loop {
        let (stream, addr) = tokio::select! {
            () = stop.cancelled() => return,
            () = rt.drain.closed.cancelled() => return,
            accepted = listener.accept() => match accepted {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::warn!(error = %e, "accept failed on the admin listener");
                    continue;
                }
            },
        };
        let _ = stream.set_nodelay(true);
        let peer = Peer {
            addr,
            tls: tls.is_some(),
            client_cert: None,
        };
        let service = router
            .clone()
            .map_request(move |mut req: http::Request<hyper::body::Incoming>| {
                req.extensions_mut().insert(peer.clone());
                req.map(axum::body::Body::new)
            });
        let closing = Closing::new(&rt, false, &stop);
        let acceptor = tls.as_ref().map(|t| t.acceptor());
        rt.tasks.spawn(async move {
            let service = TowerToHyperService::new(service);
            let result = match acceptor {
                None => {
                    let conn = builder()
                        .serve_connection(TokioIo::new(stream), service)
                        .into_owned();
                    drive!(conn, closing)
                }
                Some(acceptor) => {
                    let Some(s) = handshake(&acceptor, stream, HEADER_READ_TIMEOUT).await else {
                        return;
                    };
                    let conn = builder().serve_connection(TokioIo::new(s), service).into_owned();
                    drive!(conn, closing)
                }
            };
            if let Err(e) = result {
                tracing::debug!(error = %e, "an admin connection ended with an error");
            }
        });
    }
}

/// Reasons of the HTTP server rules.
pub mod reasons {
    antirion_error::reasons! {
        HEADER_READ_TIMEOUT = "header_read_timeout" => RequestTimeout,
    }
}
