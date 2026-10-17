//! The egress connector: resolves the destination, validates every resolved address, connects to a
//! validated address (never resolving again), then negotiates TLS for `https`.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use http::Uri;
use hyper::rt::{Read, ReadBufCursor, Write};
use hyper_util::client::legacy::connect::{Connected, Connection};
use hyper_util::rt::TokioIo;
use rustls_pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use crate::abort::{Abort, Tripwire};
use crate::resolve::Resolver;
use crate::ssrf::{Denied, EgressPolicy, TrustClass, check_addr, validate_url};
use crate::tls::TlsProfile;

/// Why no connection was made.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ConnectFailure {
    #[error("the destination is denied: {0}")]
    Denied(Denied),
    #[error("the destination name did not resolve")]
    Dns,
    #[error("the connection timed out")]
    Timeout,
    #[error("the connection failed")]
    Io(io::ErrorKind),
    #[error("the TLS handshake failed")]
    Tls,
    #[error("the forward proxy refused the tunnel with status {0}")]
    Proxy(u16),
}

/// One connector per (trust class, TLS profile, egress policy); the pool is keyed the same way.
#[derive(Clone, Debug)]
pub(crate) struct Connector {
    pub(crate) policy: Arc<EgressPolicy>,
    pub(crate) trust: TrustClass,
    pub(crate) tls: TlsProfile,
    pub(crate) connect_timeout: Duration,
    pub(crate) resolver: Resolver,
    /// Connections this connector has started: a request that failed without the count moving was
    /// sent on a pooled connection.
    pub(crate) opened: Arc<std::sync::atomic::AtomicU64>,
}

impl tower::Service<Uri> for Connector {
    type Response = Conn;
    type Error = ConnectFailure;
    type Future = Pin<Box<dyn Future<Output = Result<Conn, ConnectFailure>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let this = self.clone();
        Box::pin(async move {
            match tokio::time::timeout(this.connect_timeout, this.connect(uri)).await {
                Ok(r) => r,
                Err(_) => Err(ConnectFailure::Timeout),
            }
        })
    }
}

impl Connector {
    async fn connect(self, uri: Uri) -> Result<Conn, ConnectFailure> {
        self.opened.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let url = validate_url(&uri.to_string(), self.trust, &self.policy).map_err(ConnectFailure::Denied)?;
        let addrs: Vec<SocketAddr> = match url.ip {
            Some(ip) => vec![SocketAddr::new(ip, url.port)],
            None => self
                .resolver
                .lookup(&url.host, url.port)
                .await
                .ok_or(ConnectFailure::Dns)?,
        };
        if addrs.is_empty() {
            return Err(ConnectFailure::Dns);
        }
        for a in &addrs {
            check_addr(a.ip(), &url.host, self.trust, &self.policy).map_err(ConnectFailure::Denied)?;
        }
        let ips: Vec<std::net::IpAddr> = addrs.iter().map(SocketAddr::ip).collect();
        let tcp = match self
            .policy
            .proxy
            .as_ref()
            .filter(|p| !p.bypassed(&url.host, &ips))
        {
            Some(proxy) => tunnel_any(proxy, &addrs).await?,
            None => connect_any(&addrs).await?,
        };
        if url.scheme != "https" {
            return Ok(Conn::new(Stream::Plain(tcp), false));
        }
        let name = match &self.tls.server_name {
            Some(n) => n.clone(),
            None => match url.ip {
                Some(ip) => ServerName::IpAddress(ip.into()),
                None => ServerName::try_from(url.host.clone()).map_err(|_| ConnectFailure::Tls)?,
            },
        };
        let tls = TlsConnector::from(Arc::clone(&self.tls.config))
            .connect(name, tcp)
            .await
            .map_err(|_| ConnectFailure::Tls)?;
        let h2 = tls.get_ref().1.alpn_protocol() == Some(b"h2");
        if self.tls.http == crate::tls::HttpVersion::H2 && !h2 {
            return Err(ConnectFailure::Tls);
        }
        Ok(Conn::new(Stream::Tls(Box::new(tls)), h2))
    }
}

/// A tunnel through the proxy to the first validated address it reaches, in resolution order.
async fn tunnel_any(proxy: &crate::proxy::Proxy, addrs: &[SocketAddr]) -> Result<TcpStream, ConnectFailure> {
    let mut last = ConnectFailure::Io(io::ErrorKind::ConnectionRefused);
    for a in addrs {
        match crate::proxy::tunnel(proxy, *a).await {
            Ok(s) => return Ok(s),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Connects to the first validated address that accepts, in resolution order.
async fn connect_any(addrs: &[SocketAddr]) -> Result<TcpStream, ConnectFailure> {
    let mut last = io::ErrorKind::ConnectionRefused;
    for a in addrs {
        match TcpStream::connect(a).await {
            Ok(s) => {
                let _ = s.set_nodelay(true);
                return Ok(s);
            }
            Err(e) => last = e.kind(),
        }
    }
    Err(ConnectFailure::Io(last))
}

/// A plain or TLS connection to an upstream.
#[derive(Debug)]
pub(crate) enum Stream {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl AsyncRead for Stream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_flush(cx),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_write_vectored(cx, bufs),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_write_vectored(cx, bufs),
        }
    }

    fn is_write_vectored(&self) -> bool {
        match self {
            Stream::Plain(s) => s.is_write_vectored(),
            Stream::Tls(s) => s.is_write_vectored(),
        }
    }
}

/// An established upstream connection, as the client pool sees it. An HTTP/1.1 connection can be
/// cut off through the `Abort` its responses carry; an HTTP/2 one is shared by other streams, and
/// dropping a body already resets only its own stream.
#[derive(Debug)]
pub(crate) struct Conn {
    io: TokioIo<Stream>,
    h2: bool,
    wire: Tripwire,
    abort: Abort,
}

impl Conn {
    fn new(stream: Stream, h2: bool) -> Conn {
        let (wire, abort) = crate::abort::pair();
        Conn {
            io: TokioIo::new(stream),
            h2,
            wire,
            abort,
        }
    }

    /// The connection as a byte stream, for a client that speaks HTTP/2 itself.
    pub(crate) fn into_stream(self) -> Stream {
        self.io.into_inner()
    }
}

impl Connection for Conn {
    fn connected(&self) -> Connected {
        let c = Connected::new();
        if self.h2 {
            c.negotiated_h2()
        } else {
            c.extra(self.abort.clone())
        }
    }
}

impl Read for Conn {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: ReadBufCursor<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.wire.check(cx)?;
        Pin::new(&mut this.io).poll_read(cx, buf)
    }
}

impl Write for Conn {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        this.wire.check(cx)?;
        Pin::new(&mut this.io).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.wire.check(cx)?;
        Pin::new(&mut this.io).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.wire.check(cx)?;
        Pin::new(&mut this.io).poll_shutdown(cx)
    }

    fn is_write_vectored(&self) -> bool {
        self.io.is_write_vectored()
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        this.wire.check(cx)?;
        Pin::new(&mut this.io).poll_write_vectored(cx, bufs)
    }
}
