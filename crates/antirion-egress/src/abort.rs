//! Cutting off an HTTP/1.1 connection in the middle of a response body. A reader that stops early
//! (a size limit) must not leave the connection to be drained or pooled: the connection's next read
//! or write fails, so the client drops it unread, and its socket closes with data still pending,
//! which the upstream sees as a reset.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Context;

use futures_util::task::AtomicWaker;
use tokio::sync::watch;

#[derive(Debug, Default)]
struct Shared {
    aborted: AtomicBool,
    waker: AtomicWaker,
}

/// Cuts off the HTTP/1.1 connection a response arrived on.
#[derive(Clone, Debug)]
pub struct Abort {
    shared: Arc<Shared>,
    /// Ends when the connection is dropped.
    open: watch::Receiver<()>,
}

/// The connection's half: it fails the connection's I/O once aborted, and dropping it tells
/// `Abort` the connection is gone.
#[derive(Debug)]
pub(crate) struct Tripwire {
    shared: Arc<Shared>,
    _open: watch::Sender<()>,
}

pub(crate) fn pair() -> (Tripwire, Abort) {
    let shared = Arc::new(Shared::default());
    let (tx, rx) = watch::channel(());
    (
        Tripwire {
            shared: Arc::clone(&shared),
            _open: tx,
        },
        Abort { shared, open: rx },
    )
}

impl Tripwire {
    /// An error once the connection was aborted. Registers the task, so an abort wakes it.
    pub(crate) fn check(&self, cx: &Context<'_>) -> io::Result<()> {
        self.shared.waker.register(cx.waker());
        if self.shared.aborted.load(Ordering::Acquire) {
            return Err(io::ErrorKind::ConnectionAborted.into());
        }
        Ok(())
    }
}

impl Abort {
    /// Fails the connection's I/O and waits until the connection is dropped. Drop the response
    /// body first: the client does not touch the connection while a body nobody reads is pending.
    pub async fn abort(&self) {
        self.shared.aborted.store(true, Ordering::Release);
        self.shared.waker.wake();
        let mut open = self.open.clone();
        while open.changed().await.is_ok() {}
    }
}

#[cfg(test)]
mod tests {
    use std::task::Waker;

    use super::*;

    #[tokio::test]
    async fn an_abort_fails_the_io_and_waits_for_the_drop() {
        let (wire, abort) = pair();
        let cx = Context::from_waker(Waker::noop());
        assert!(wire.check(&cx).is_ok());
        let mut waiting = tokio::task::JoinSet::new();
        waiting.spawn({
            let abort = abort.clone();
            async move { abort.abort().await }
        });
        while !abort.shared.aborted.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
        let err = wire.check(&cx).err().map(|e| e.kind());
        assert_eq!(err, Some(io::ErrorKind::ConnectionAborted));
        assert!(waiting.try_join_next().is_none());
        drop(wire);
        assert!(waiting.join_next().await.is_some_and(|r| r.is_ok()));
    }

    #[tokio::test]
    async fn aborting_a_dropped_connection_returns_at_once() {
        let (wire, abort) = pair();
        drop(wire);
        abort.abort().await;
    }
}
