//! The client response body of a relayed stream: a queue whose bytes are bounded by permits that are
//! released as the server takes each frame.

use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use http::HeaderMap;
use http_body::{Frame as HttpFrame, SizeHint};
use tokio::sync::{OwnedSemaphorePermit, mpsc};

use crate::ctx::BoxError;

/// Bytes for the client and the buffer permits they hold until the server takes them.
#[derive(Debug)]
pub(super) struct Frame {
    pub(super) bytes: Bytes,
    pub(super) permit: OwnedSemaphorePermit,
    /// The trailer section, sent after every data frame.
    pub(super) trailers: Option<HeaderMap>,
}

#[derive(Debug)]
pub(super) struct ChannelBody {
    rx: mpsc::Receiver<Frame>,
}

impl ChannelBody {
    pub(super) fn new(rx: mpsc::Receiver<Frame>) -> ChannelBody {
        ChannelBody { rx }
    }
}

impl http_body::Body for ChannelBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<HttpFrame<Bytes>, BoxError>>> {
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(frame)) => {
                drop(frame.permit);
                Poll::Ready(Some(Ok(match frame.trailers {
                    Some(t) => HttpFrame::trailers(t),
                    None => HttpFrame::data(frame.bytes),
                })))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::default()
    }
}
