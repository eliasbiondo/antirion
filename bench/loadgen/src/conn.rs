//! Client connections: an HTTP/1.1 keep-alive pool that opens connections on demand up to a cap, and
//! a fixed set of h2c connections with a stream cap each.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http_body_util::Full;
use hyper::client::conn::{http1, http2};
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::TcpStream;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

pub type ReqBody = Full<Bytes>;

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("cannot connect to {addr}: {source}")]
    Io {
        addr: SocketAddr,
        source: std::io::Error,
    },
    #[error("handshake with {addr} failed: {source}")]
    Handshake { addr: SocketAddr, source: hyper::Error },
    #[error("the pool is closed")]
    Closed,
}

async fn tcp(addr: SocketAddr) -> Result<TokioIo<TcpStream>, ConnectError> {
    let stream = TcpStream::connect(addr)
        .await
        .map_err(|source| ConnectError::Io { addr, source })?;
    let _ = stream.set_nodelay(true);
    Ok(TokioIo::new(stream))
}

/// A connection checked out of a pool; give it back with [`Pool::checkin`] once its response is read.
#[derive(Debug)]
pub enum Lease {
    H1(http1::SendRequest<ReqBody>, OwnedSemaphorePermit),
    H2(http2::SendRequest<ReqBody>, OwnedSemaphorePermit),
}

/// The connections to one node.
#[derive(Debug)]
pub enum Pool {
    H1(H1Pool),
    H2(H2Pool),
}

impl Pool {
    pub async fn checkout(&self) -> Result<Lease, ConnectError> {
        match self {
            Pool::H1(p) => p.checkout().await,
            Pool::H2(p) => p.checkout().await,
        }
    }

    /// Returns a connection whose last response was read completely.
    pub fn checkin(&self, lease: Lease) {
        if let (Pool::H1(p), Lease::H1(send, permit)) = (self, lease) {
            p.checkin(send, permit);
        }
    }

    /// Connections opened so far.
    #[must_use]
    pub fn opened(&self) -> u64 {
        match self {
            Pool::H1(p) => p.opened.load(Ordering::Relaxed),
            Pool::H2(p) => p.conns.len() as u64,
        }
    }
}

/// HTTP/1.1 keep-alive connections, opened when no idle one is free, at most `max` at a time.
#[derive(Debug)]
pub struct H1Pool {
    addr: SocketAddr,
    idle: Mutex<Vec<http1::SendRequest<ReqBody>>>,
    slots: Arc<Semaphore>,
    drivers: TaskTracker,
    stop: CancellationToken,
    opened: AtomicU64,
}

impl H1Pool {
    #[must_use]
    pub fn new(addr: SocketAddr, max: usize, drivers: TaskTracker, stop: CancellationToken) -> Self {
        Self {
            addr,
            idle: Mutex::new(Vec::new()),
            slots: Arc::new(Semaphore::new(max.max(1))),
            drivers,
            stop,
            opened: AtomicU64::new(0),
        }
    }

    fn pop_idle(&self) -> Option<http1::SendRequest<ReqBody>> {
        let mut idle = self.idle.lock().ok()?;
        while let Some(s) = idle.pop() {
            if !s.is_closed() {
                return Some(s);
            }
        }
        None
    }

    async fn checkout(&self) -> Result<Lease, ConnectError> {
        let permit = Arc::clone(&self.slots)
            .acquire_owned()
            .await
            .map_err(|_| ConnectError::Closed)?;
        if let Some(send) = self.pop_idle() {
            return Ok(Lease::H1(send, permit));
        }
        let io = tcp(self.addr).await?;
        let addr = self.addr;
        let (send, conn) = http1::handshake(io)
            .await
            .map_err(|source| ConnectError::Handshake { addr, source })?;
        self.opened.fetch_add(1, Ordering::Relaxed);
        let stop = self.stop.clone();
        self.drivers.spawn(async move {
            tokio::select! {
                _ = conn => {}
                () = stop.cancelled() => {}
            }
        });
        Ok(Lease::H1(send, permit))
    }

    fn checkin(&self, send: http1::SendRequest<ReqBody>, permit: OwnedSemaphorePermit) {
        if !send.is_closed()
            && let Ok(mut idle) = self.idle.lock()
        {
            idle.push(send);
        }
        drop(permit);
    }
}

/// A fixed set of h2c (prior knowledge) connections, used round robin, each with a stream cap.
#[derive(Debug)]
pub struct H2Pool {
    conns: Vec<(http2::SendRequest<ReqBody>, Arc<Semaphore>)>,
    next: AtomicUsize,
}

impl H2Pool {
    pub async fn connect(
        addr: SocketAddr,
        connections: usize,
        streams: usize,
        drivers: &TaskTracker,
        stop: &CancellationToken,
    ) -> Result<Self, ConnectError> {
        let mut conns = Vec::with_capacity(connections);
        for _ in 0..connections.max(1) {
            let io = tcp(addr).await?;
            let (send, conn) = http2::Builder::new(TokioExecutor::new())
                .initial_stream_window_size(1 << 20)
                .initial_connection_window_size(1 << 24)
                .handshake(io)
                .await
                .map_err(|source| ConnectError::Handshake { addr, source })?;
            let stop = stop.clone();
            drivers.spawn(async move {
                tokio::select! {
                    _ = conn => {}
                    () = stop.cancelled() => {}
                }
            });
            conns.push((send, Arc::new(Semaphore::new(streams.max(1)))));
        }
        Ok(Self {
            conns,
            next: AtomicUsize::new(0),
        })
    }

    async fn checkout(&self) -> Result<Lease, ConnectError> {
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        let (send, streams) = self
            .conns
            .get(n % self.conns.len().max(1))
            .ok_or(ConnectError::Closed)?;
        let permit = Arc::clone(streams)
            .acquire_owned()
            .await
            .map_err(|_| ConnectError::Closed)?;
        Ok(Lease::H2(send.clone(), permit))
    }
}
