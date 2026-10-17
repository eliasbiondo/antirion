//! The timing of one data connection between requests: a connection with no request in progress is
//! closed once it has been idle for `idle_timeout`, and an HTTP/1.1 connection whose next request
//! head started arriving is closed when that head is not complete within `header_read_timeout`.
//! The first head of a connection is bounded before the HTTP stack sees it (`head.rs`).
//!
//! An HTTP/1.1 connection asked to close shows the HTTP stack the end of the stream at the first read
//! made while no request is in progress or arriving, so the stack closes it as if the client had;
//! a request whose first bytes were already read is served first. Every close then lingers: the
//! connection sends its FIN and keeps reading and discarding what the client still sends for a
//! moment, so a request that crosses the FIN meets an end of stream instead of a reset.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::{Instant, Sleep};

/// No request head is being read.
const NO_HEAD: u64 = u64::MAX;
/// How long a closing connection keeps reading after its FIN.
const LINGER: Duration = Duration::from_secs(1);

/// What a connection did last, in milliseconds since it was accepted.
#[derive(Debug)]
pub(crate) struct ConnClock {
    origin: Instant,
    last_ms: AtomicU64,
    active: AtomicUsize,
    head_from_ms: AtomicU64,
    closing: AtomicBool,
    /// HTTP/1.1: bytes arriving between requests start a request head.
    heads: bool,
    header_read: Duration,
    idle: Duration,
}

impl ConnClock {
    pub(crate) fn new(heads: bool, header_read: Duration, idle: Duration) -> Arc<ConnClock> {
        Arc::new(ConnClock {
            origin: Instant::now(),
            last_ms: AtomicU64::new(0),
            active: AtomicUsize::new(0),
            head_from_ms: AtomicU64::new(NO_HEAD),
            closing: AtomicBool::new(false),
            heads,
            header_read,
            idle,
        })
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX - 1)
    }

    fn touch(&self) {
        self.last_ms.store(self.now_ms(), Ordering::Relaxed);
    }

    /// Records bytes of the next request read from the connection.
    pub(crate) fn read(&self, n: usize) {
        if n == 0 {
            return;
        }
        self.touch();
        if self.heads && self.active.load(Ordering::Relaxed) == 0 {
            let _ = self.head_from_ms.compare_exchange(
                NO_HEAD,
                self.now_ms(),
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
        }
    }

    /// A request reached the service; the returned guard ends it.
    pub(crate) fn begin(self: &Arc<Self>) -> InRequest {
        self.active.fetch_add(1, Ordering::Relaxed);
        self.head_from_ms.store(NO_HEAD, Ordering::Relaxed);
        self.touch();
        InRequest(Arc::clone(self))
    }

    /// Closes an HTTP/1.1 connection once no request is in progress or arriving on it.
    pub(crate) fn close_when_idle(&self) {
        self.closing.store(true, Ordering::Relaxed);
    }

    fn reads_end(&self) -> bool {
        self.closing.load(Ordering::Relaxed)
            && self.active.load(Ordering::Relaxed) == 0
            && self.head_from_ms.load(Ordering::Relaxed) == NO_HEAD
    }

    /// Completes when the connection should be closed.
    pub(crate) async fn expired(&self) {
        loop {
            let now = self.now_ms();
            let wait = if self.active.load(Ordering::Relaxed) > 0 {
                self.idle
            } else {
                let head = self.head_from_ms.load(Ordering::Relaxed);
                let deadline = if head == NO_HEAD {
                    self.last_ms.load(Ordering::Relaxed).saturating_add(ms(self.idle))
                } else {
                    head.saturating_add(ms(self.header_read))
                };
                if now >= deadline {
                    return;
                }
                Duration::from_millis(deadline - now)
            };
            tokio::time::sleep(wait.min(Duration::from_secs(1))).await;
        }
    }
}

fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX / 2)
}

/// A request in progress on a connection, until its response body ends or is dropped.
#[derive(Debug)]
pub(crate) struct InRequest(Arc<ConnClock>);

impl Drop for InRequest {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::Relaxed);
        self.0.touch();
    }
}

/// A connection's stream, reporting its reads and writes to the clock.
pub(crate) struct Clocked<S> {
    inner: S,
    clock: Arc<ConnClock>,
    shut: bool,
    linger: Option<Pin<Box<Sleep>>>,
}

impl<S> Clocked<S> {
    pub(crate) fn new(inner: S, clock: Arc<ConnClock>) -> Clocked<S> {
        Clocked {
            inner,
            clock,
            shut: false,
            linger: None,
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> Clocked<S> {
    /// After the FIN: reads and discards until the client closes, the read fails, or `LINGER`.
    fn poll_linger(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        let deadline = self
            .linger
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(LINGER)));
        if deadline.as_mut().poll(cx).is_ready() {
            return Poll::Ready(());
        }
        let mut scratch = [0u8; 2048];
        loop {
            let mut buf = ReadBuf::new(&mut scratch);
            match Pin::new(&mut self.inner).poll_read(cx, &mut buf) {
                Poll::Ready(Ok(())) if !buf.filled().is_empty() => {}
                Poll::Ready(_) => return Poll::Ready(()),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Clocked<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.clock.heads && self.clock.reads_end() {
            return Poll::Ready(Ok(()));
        }
        let before = buf.filled().len();
        let r = Pin::new(&mut self.inner).poll_read(cx, buf);
        if r.is_ready() {
            self.clock.read(buf.filled().len().saturating_sub(before));
        }
        r
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for Clocked<S> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let r = Pin::new(&mut self.inner).poll_write(cx, buf);
        if matches!(r, Poll::Ready(Ok(n)) if n > 0) {
            self.clock.touch();
        }
        r
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if !self.shut {
            std::task::ready!(Pin::new(&mut self.inner).poll_shutdown(cx))?;
            self.shut = true;
        }
        self.poll_linger(cx).map(Ok)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let r = Pin::new(&mut self.inner).poll_write_vectored(cx, bufs);
        if matches!(r, Poll::Ready(Ok(n)) if n > 0) {
            self.clock.touch();
        }
        r
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: Duration = Duration::from_millis(100);
    const IDLE: Duration = Duration::from_millis(250);

    async fn expiry(c: &ConnClock) -> Duration {
        let t0 = Instant::now();
        c.expired().await;
        t0.elapsed()
    }

    #[tokio::test]
    async fn idle_connections_and_stalled_heads_expire() {
        let idle = expiry(&ConnClock::new(true, HEAD, IDLE)).await;
        assert!(idle >= IDLE && idle < IDLE * 3, "{idle:?}");
        let c = ConnClock::new(true, HEAD, IDLE);
        c.read(3);
        let head = expiry(&c).await;
        assert!(head >= HEAD && head < IDLE, "{head:?}");
        let h2 = ConnClock::new(false, HEAD, IDLE);
        h2.read(3);
        assert!(expiry(&h2).await >= IDLE, "HTTP/2 frames start no head");
    }

    #[tokio::test]
    async fn a_request_in_progress_keeps_the_connection() {
        let c = ConnClock::new(true, HEAD, IDLE);
        let r = c.begin();
        assert!(tokio::time::timeout(IDLE * 2, c.expired()).await.is_err());
        drop(r);
        assert!(expiry(&c).await >= IDLE);
    }

    #[tokio::test]
    async fn a_closing_connection_reads_an_end_only_between_requests() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut client, server) = tokio::io::duplex(64);
        let c = ConnClock::new(true, HEAD, IDLE);
        let mut io = Clocked::new(server, Arc::clone(&c));
        client.write_all(b"GET").await.unwrap();
        let mut buf = [0u8; 8];
        assert_eq!(io.read(&mut buf).await.unwrap(), 3);
        c.close_when_idle();
        client.write_all(b" /").await.unwrap();
        assert_eq!(
            io.read(&mut buf).await.unwrap(),
            2,
            "a head in progress is read on"
        );
        let request = c.begin();
        drop(request);
        client.write_all(b"GET").await.unwrap();
        assert_eq!(
            io.read(&mut buf).await.unwrap(),
            0,
            "between requests the stack sees the end"
        );

        let (shut, seen) = tokio::join!(tokio::time::timeout(LINGER / 2, io.shutdown()), async move {
            let mut rest = Vec::new();
            client.read_to_end(&mut rest).await.unwrap()
        });
        assert_eq!(seen, 0, "the client gets an end of stream");
        shut.unwrap().unwrap();
    }
}
