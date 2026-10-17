//! The client side of a committed stream: frames handed to the response body under a byte budget,
//! and how a failed write ends the stream.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::{Semaphore, mpsc};
use tokio::time::Instant;

use super::forward::Ending;
use super::{Setup, array, body, invoke};

/// How the relay's frames are re-framed for the client.
#[derive(Debug)]
pub(super) enum Reframe {
    /// AWS event-stream `chunk` messages around Anthropic events.
    Invoke(invoke::InvokeFrames),
    /// A Gemini JSON array.
    Array(array::ArrayFrames),
}

pub(super) enum SendEnd {
    Disconnected,
    WriteTimeout,
}

pub(super) struct Writer<'a> {
    pub(super) tx: mpsc::Sender<body::Frame>,
    pub(super) sem: Arc<Semaphore>,
    pub(super) setup: &'a Setup,
    pub(super) last_write: Instant,
    pub(super) first_sent: Option<Instant>,
    /// Time spent waiting for the client to take bytes.
    pub(super) blocked: Duration,
    pub(super) tee: Option<Box<crate::cache::Tee>>,
    pub(super) frames: Option<Reframe>,
    /// The client left and the stream completes without it: nothing more is written.
    pub(super) detached: bool,
}

impl Writer<'_> {
    pub(super) async fn send(&mut self, bytes: Bytes) -> Result<(), SendEnd> {
        let bytes = match self.frames.as_mut() {
            Some(Reframe::Invoke(f)) => f.reframe(&bytes),
            Some(Reframe::Array(f)) => f.reframe(&bytes),
            None => bytes,
        };
        self.write(bytes).await
    }

    /// Ends a JSON-array stream with its closing bracket.
    pub(super) async fn close_array(&mut self) -> Result<(), SendEnd> {
        match self.frames.as_mut() {
            Some(Reframe::Array(f)) => {
                let closing = f.close();
                self.write(closing).await
            }
            _ => Ok(()),
        }
    }

    /// Stops writing: the client left, and the stream completes without it (`on_client_disconnect:
    /// complete`); nothing of it is cached.
    pub(super) fn detach(&mut self) {
        self.detached = true;
        self.tee = None;
    }

    async fn write(&mut self, bytes: Bytes) -> Result<(), SendEnd> {
        if bytes.is_empty() || self.detached {
            return Ok(());
        }
        match self.deliver(bytes).await {
            Err(SendEnd::Disconnected) if self.setup.policy.complete_on_disconnect => {
                self.detach();
                Ok(())
            }
            other => other,
        }
    }

    async fn deliver(&mut self, bytes: Bytes) -> Result<(), SendEnd> {
        if let Some(t) = self.tee.as_mut() {
            t.push(&bytes);
        }
        let n = u32::try_from(bytes.len().min(self.setup.policy.send_buffer.max(1))).unwrap_or(u32::MAX);
        let waiting = Instant::now();
        let permit = tokio::select! {
            () = self.tx.closed() => return Err(SendEnd::Disconnected),
            p = tokio::time::timeout(
                self.setup.policy.client_write_timeout,
                Arc::clone(&self.sem).acquire_many_owned(n),
            ) => match p {
                Err(_) => return Err(SendEnd::WriteTimeout),
                Ok(Err(_)) => return Err(SendEnd::Disconnected),
                Ok(Ok(p)) => p,
            },
        };
        self.blocked += waiting.elapsed();
        let sent = tokio::time::timeout(
            self.setup.policy.client_write_timeout,
            self.tx.send(body::Frame {
                bytes,
                permit,
                trailers: None,
            }),
        )
        .await;
        match sent {
            Err(_) => Err(SendEnd::WriteTimeout),
            Ok(Err(_)) => Err(SendEnd::Disconnected),
            Ok(Ok(())) => {
                self.last_write = Instant::now();
                self.first_sent.get_or_insert(self.last_write);
                Ok(())
            }
        }
    }
}

impl Writer<'_> {
    /// Sends the trailer section after the last data frame.
    pub(super) async fn send_trailers(&mut self, trailers: http::HeaderMap) -> Result<(), SendEnd> {
        let permit = tokio::time::timeout(
            self.setup.policy.client_write_timeout,
            Arc::clone(&self.sem).acquire_owned(),
        )
        .await
        .map_err(|_| SendEnd::WriteTimeout)?
        .map_err(|_| SendEnd::Disconnected)?;
        self.tx
            .send(body::Frame {
                bytes: Bytes::new(),
                permit,
                trailers: Some(trailers),
            })
            .await
            .map_err(|_| SendEnd::Disconnected)
    }
}

pub(super) fn send_end(e: SendEnd) -> Ending {
    match e {
        SendEnd::Disconnected => Ending::ok("client_disconnected"),
        SendEnd::WriteTimeout => Ending::ok("client_write_timeout"),
    }
}
