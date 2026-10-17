//! An outbound request body as a list of byte slices: a body that differs from the client's by a few
//! spliced members shares the client's bytes instead of copying them. A body may end with a
//! streamed tail (an upload the gateway relays without holding it whole), which can be sent once.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::{Bytes, BytesMut};
use futures_util::Stream;
use http_body::{Frame, SizeHint};
use parking_lot::Mutex;

/// The error of a streamed tail, which aborts the request it belongs to.
pub type TailError = Box<dyn std::error::Error + Send + Sync>;

/// The bytes a streamed tail yields.
pub type TailStream = Pin<Box<dyn Stream<Item = Result<Bytes, TailError>> + Send>>;

/// The streamed end of a body: taken by the first request that sends it.
#[derive(Clone)]
pub struct Tail {
    stream: Arc<Mutex<Option<TailStream>>>,
    len: Option<u64>,
}

impl Tail {
    /// A tail of `len` bytes when known.
    #[must_use]
    pub fn new(stream: TailStream, len: Option<u64>) -> Tail {
        Tail {
            stream: Arc::new(Mutex::new(Some(stream))),
            len,
        }
    }

    /// Whether a request already took the bytes.
    #[must_use]
    pub fn taken(&self) -> bool {
        self.stream.lock().is_none()
    }

    pub(crate) fn take(&self) -> Option<TailStream> {
        self.stream.lock().take()
    }
}

impl std::fmt::Debug for Tail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tail")
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

/// The bytes of an outbound body, in order.
#[derive(Clone, Debug, Default)]
pub struct Payload {
    parts: Vec<Bytes>,
    tail: Option<Tail>,
}

impl PartialEq for Payload {
    fn eq(&self, other: &Payload) -> bool {
        let same_tail = match (&self.tail, &other.tail) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(&a.stream, &b.stream),
            _ => false,
        };
        same_tail && self.parts == other.parts
    }
}

impl Eq for Payload {}

impl Payload {
    /// The concatenation of `parts`.
    #[must_use]
    pub fn from_parts(parts: Vec<Bytes>) -> Payload {
        Payload {
            parts: parts.into_iter().filter(|p| !p.is_empty()).collect(),
            tail: None,
        }
    }

    /// The body with a streamed tail after its parts.
    #[must_use]
    pub fn with_tail(mut self, tail: Tail) -> Payload {
        self.tail = Some(tail);
        self
    }

    /// The length of the parts held in memory (a streamed tail is not counted).
    #[must_use]
    pub fn len(&self) -> usize {
        self.parts.iter().map(Bytes::len).sum()
    }

    /// The whole length, when a streamed tail's is known.
    #[must_use]
    pub fn total_len(&self) -> Option<u64> {
        let held = self.len() as u64;
        match &self.tail {
            None => Some(held),
            Some(t) => t.len.map(|l| held + l),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.parts.is_empty() && self.tail.is_none()
    }

    #[must_use]
    pub fn parts(&self) -> &[Bytes] {
        &self.parts
    }

    #[must_use]
    pub fn tail(&self) -> Option<&Tail> {
        self.tail.as_ref()
    }

    /// The parts as one slice: shared when there is one, copied together otherwise. A streamed tail
    /// is not included.
    #[must_use]
    pub fn contiguous(&self) -> Bytes {
        match self.parts.as_slice() {
            [] => Bytes::new(),
            [one] => one.clone(),
            parts => {
                let mut out = BytesMut::with_capacity(self.len());
                for p in parts {
                    out.extend_from_slice(p);
                }
                out.freeze()
            }
        }
    }

    pub(crate) fn body(&self) -> PayloadBody {
        PayloadBody {
            remaining: self.total_len(),
            parts: self.parts.iter().cloned().collect(),
            tail: self.tail.clone().map(TailState::Pending),
        }
    }
}

impl From<Bytes> for Payload {
    fn from(b: Bytes) -> Payload {
        Payload::from_parts(vec![b])
    }
}

impl From<Vec<u8>> for Payload {
    fn from(v: Vec<u8>) -> Payload {
        Payload::from(Bytes::from(v))
    }
}

impl From<&'static [u8]> for Payload {
    fn from(v: &'static [u8]) -> Payload {
        Payload::from(Bytes::from_static(v))
    }
}

/// A tail before and after its first poll: it is taken only when the body is sent, so a request
/// that never reached the network leaves it for the next one.
enum TailState {
    Pending(Tail),
    Streaming(TailStream),
}

/// A [`Payload`] sent as an HTTP body: one data frame per part, then the tail's frames; its exact
/// length known when the tail's is.
pub(crate) struct PayloadBody {
    parts: VecDeque<Bytes>,
    remaining: Option<u64>,
    tail: Option<TailState>,
}

impl std::fmt::Debug for PayloadBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PayloadBody")
            .field("parts", &self.parts.len())
            .field("remaining", &self.remaining)
            .finish_non_exhaustive()
    }
}

/// A body whose tail another request already sent.
#[derive(Debug, thiserror::Error)]
#[error("the streamed request body was already sent")]
pub(crate) struct TailTaken;

impl http_body::Body for PayloadBody {
    type Data = Bytes;
    type Error = TailError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, TailError>>> {
        if let Some(p) = self.parts.pop_front() {
            if let Some(r) = self.remaining.as_mut() {
                *r = r.saturating_sub(p.len() as u64);
            }
            return Poll::Ready(Some(Ok(Frame::data(p))));
        }
        let stream = match self.tail.take() {
            None => return Poll::Ready(None),
            Some(TailState::Pending(t)) => match t.take() {
                Some(s) => s,
                None => return Poll::Ready(Some(Err(Box::new(TailTaken)))),
            },
            Some(TailState::Streaming(s)) => s,
        };
        let mut stream = stream;
        match stream.as_mut().poll_next(cx) {
            Poll::Pending => {
                self.tail = Some(TailState::Streaming(stream));
                Poll::Pending
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Ready(Some(Ok(chunk))) => {
                if let Some(r) = self.remaining.as_mut() {
                    *r = r.saturating_sub(chunk.len() as u64);
                }
                self.tail = Some(TailState::Streaming(stream));
                Poll::Ready(Some(Ok(Frame::data(chunk))))
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.parts.is_empty() && self.tail.is_none()
    }

    fn size_hint(&self) -> SizeHint {
        match self.remaining {
            Some(n) => SizeHint::with_exact(n),
            None => {
                let mut h = SizeHint::new();
                h.set_lower(self.parts.iter().map(|p| p.len() as u64).sum());
                h
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt;

    use super::*;

    #[tokio::test]
    async fn parts_are_sent_in_order_with_their_exact_length() {
        let p = Payload::from_parts(vec![
            Bytes::from_static(b"{\"a\":"),
            Bytes::new(),
            Bytes::from_static(b"1}"),
        ]);
        assert_eq!(p.parts().len(), 2);
        assert_eq!(p.len(), 7);
        assert_eq!(p.contiguous(), Bytes::from_static(b"{\"a\":1}"));
        let body = p.body();
        assert_eq!(http_body::Body::size_hint(&body).exact(), Some(7));
        assert_eq!(body.collect().await.unwrap().to_bytes(), p.contiguous());
        let one = Payload::from(Bytes::from_static(b"x"));
        assert_eq!(one.total_len(), Some(1));
        assert_eq!(one.contiguous().as_ptr(), one.parts()[0].as_ptr());
        assert!(Payload::default().is_empty());
    }

    #[tokio::test]
    async fn a_tail_follows_the_parts_and_is_sent_once() {
        let chunks: Vec<Result<Bytes, TailError>> =
            vec![Ok(Bytes::from_static(b"cd")), Ok(Bytes::from_static(b"e"))];
        let tail = Tail::new(Box::pin(futures_util::stream::iter(chunks)), Some(3));
        let p = Payload::from(Bytes::from_static(b"ab")).with_tail(tail.clone());
        assert_eq!(p.total_len(), Some(5));
        let first = p.body();
        assert_eq!(http_body::Body::size_hint(&first).exact(), Some(5));
        assert!(!tail.taken());
        assert_eq!(
            first.collect().await.unwrap().to_bytes(),
            Bytes::from_static(b"abcde")
        );
        assert!(tail.taken());
        assert!(p.body().collect().await.is_err());
        let unknown = Payload::default().with_tail(Tail::new(Box::pin(futures_util::stream::empty()), None));
        assert_eq!(http_body::Body::size_hint(&unknown.body()).exact(), None);
    }
}
