//! Upload bodies (`multipart/form-data`): read ahead only until the parts that route the request
//! are known, then the rest relayed to the upstream as it arrives, within `max_upload_body`.
//!
//! A body that ends within the spool size is held whole, so its attempts can be retried like any
//! other. A larger one is sent once: its streamed rest goes to the first attempt that reaches the
//! network, and an attempt after that fails with the error of the one that took it.

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use antirion_core::Stage;
use antirion_egress::{Tail, TailError};
use antirion_error::reason::hygiene;
use antirion_error::{GatewayError, Message};
use antirion_wire::decode::{Coding, DecodeError, decode_stream, parse_coding};
use antirion_wire::multipart::Scanner;
use bytes::{Bytes, BytesMut};
use futures_util::{Stream, StreamExt};
use http::header::{CONTENT_ENCODING, CONTENT_LENGTH};
use http::request::Parts;
use hyper::body::Incoming;

use crate::body::{self, BodyError};
use crate::ctx::Ctx;
use crate::reasons;

/// A body that ends within this many bytes is held whole.
const SPOOL: usize = 1 << 20;

/// How the streamed rest of an upload broke.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Broke {
    TooLarge,
    Timeout,
    Framing,
    Corrupt,
}

#[derive(Debug, Default)]
struct State {
    broke: Option<Broke>,
    /// The error of the attempt that took the rest.
    sent_with: Option<GatewayError>,
}

/// The rest of an upload body, after the head the gateway read.
#[derive(Clone)]
pub(crate) struct Upload {
    tail: Tail,
    state: Arc<Mutex<State>>,
}

impl std::fmt::Debug for Upload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Upload")
            .field("tail", &self.tail)
            .finish_non_exhaustive()
    }
}

impl Upload {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        match self.state.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }

    /// The streamed rest, for the upstream body.
    pub(crate) fn tail(&self) -> Tail {
        self.tail.clone()
    }

    /// Whether an attempt already sent the rest.
    pub(crate) fn spent(&self) -> bool {
        self.tail.taken()
    }

    /// The client's error when the client's own body broke while it was relayed.
    pub(crate) fn broken(&self) -> Option<GatewayError> {
        Some(match self.lock().broke? {
            Broke::TooLarge => crate::accept::too_large(Stage::Attempt),
            Broke::Timeout => GatewayError::with_reason(
                reasons::BODY_READ_TIMEOUT,
                Stage::Attempt,
                Message::Fixed("The request body did not arrive in time."),
            )
            .closing(),
            Broke::Framing => GatewayError::with_reason(
                hygiene::FRAMING_ERROR,
                Stage::Attempt,
                Message::Fixed("The request body is not framed correctly."),
            )
            .closing(),
            Broke::Corrupt => GatewayError::with_reason(
                reasons::INVALID_COMPRESSED_BODY,
                Stage::Attempt,
                Message::Fixed("The request body could not be decoded with its Content-Encoding."),
            ),
        })
    }

    /// Keeps the error of the attempt that sent the rest, for any attempt after it.
    pub(crate) fn sent_with(&self, e: &GatewayError) {
        let mut s = self.lock();
        if s.sent_with.is_none() {
            s.sent_with = Some(e.clone());
        }
    }

    /// The error of the attempt that sent the rest.
    pub(crate) fn earlier_error(&self) -> GatewayError {
        self.lock().sent_with.clone().unwrap_or_else(|| {
            GatewayError::with_reason(
                hygiene::FRAMING_ERROR,
                Stage::Attempt,
                Message::Fixed("The upload was already sent to an upstream and cannot be sent again."),
            )
        })
    }
}

/// The read-ahead of an upload body.
pub(crate) struct Head {
    pub(crate) raw: Bytes,
    pub(crate) scanner: Scanner,
    /// The rest, when the body did not end within the read-ahead.
    pub(crate) rest: Option<Upload>,
}

/// The file parts of an upload: the read-ahead ends once one of them started (and the spool size
/// was reached), so the parts that route the request must come before it.
pub(crate) struct Wanted<'a> {
    pub(crate) files: &'a [&'a str],
}

impl Wanted<'_> {
    fn met(&self, scanner: &Scanner) -> bool {
        let named = |n: Option<&str>| n.is_some_and(|n| self.files.contains(&n));
        scanner.open_part().is_some_and(|p| named(p.name.as_deref()))
            || scanner.form().parts().iter().any(|p| named(p.name.as_deref()))
    }
}

fn decode_error(e: &DecodeError) -> Broke {
    match e {
        DecodeError::TooLarge => Broke::TooLarge,
        DecodeError::Corrupt => Broke::Corrupt,
        DecodeError::Source(s) => match s.downcast_ref::<BodyError>() {
            Some(BodyError::TooLarge) => Broke::TooLarge,
            Some(BodyError::Timeout) => Broke::Timeout,
            _ => Broke::Framing,
        },
    }
}

fn error_of(b: Broke) -> GatewayError {
    match b {
        Broke::TooLarge => crate::accept::too_large(Stage::Parse),
        Broke::Timeout => GatewayError::with_reason(
            reasons::BODY_READ_TIMEOUT,
            Stage::Parse,
            Message::Fixed("The request body did not arrive in time."),
        )
        .closing(),
        Broke::Framing => GatewayError::with_reason(
            hygiene::FRAMING_ERROR,
            Stage::Parse,
            Message::Fixed("The request body is not framed correctly."),
        )
        .closing(),
        Broke::Corrupt => GatewayError::with_reason(
            reasons::INVALID_COMPRESSED_BODY,
            Stage::Parse,
            Message::Fixed("The request body could not be decoded with its Content-Encoding."),
        ),
    }
}

/// The 400 of a body that is not a well-formed form.
pub(crate) fn malformed() -> GatewayError {
    GatewayError::with_reason(
        reasons::INVALID_VALUE,
        Stage::Parse,
        Message::Fixed("The multipart body is malformed."),
    )
    .openai_code("invalid_value")
}

type Frames = Pin<Box<dyn Stream<Item = Result<Bytes, DecodeError>> + Send>>;

/// Reads an upload body's head: the whole body when it ends within the spool size, else up to the
/// start of its file part, the rest left to stream.
pub(crate) async fn read(
    cx: &mut Ctx,
    parts: &Parts,
    incoming: Incoming,
    boundary: &str,
    wanted: &Wanted<'_>,
) -> Result<Head, GatewayError> {
    let Ok(coding) = parse_coding(
        parts
            .headers
            .get_all(CONTENT_ENCODING)
            .iter()
            .map(|v| v.as_bytes()),
    ) else {
        return Err(GatewayError::with_reason(
            hygiene::UNSUPPORTED_CONTENT_ENCODING,
            Stage::Parse,
            Message::Fixed("The Content-Encoding of the request is not supported."),
        ));
    };
    let limit = cx.snap.security.max_upload_body;
    let read_ahead = usize::try_from(cx.snap.security.max_body.min(limit)).unwrap_or(usize::MAX);
    let gap = cx.rt.statics.http.body_read;
    let declared = parts
        .headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|_| coding == Coding::Identity);
    let mut frames: Frames = Box::pin(decode_stream(coding, body::stream(incoming, gap)));
    let mut head = BytesMut::with_capacity(usize::try_from(declared.unwrap_or(0)).unwrap_or(0).min(SPOOL));
    let mut scanner = Scanner::new(boundary);
    loop {
        let Some(chunk) = frames.next().await else {
            cx.body_read = true;
            return Ok(Head {
                raw: head.freeze(),
                scanner,
                rest: None,
            });
        };
        let chunk = chunk.map_err(|e| error_of(decode_error(&e)))?;
        if u64::try_from(head.len().saturating_add(chunk.len())).unwrap_or(u64::MAX) > limit {
            return Err(error_of(Broke::TooLarge));
        }
        head.extend_from_slice(&chunk);
        if scanner.closed() {
            continue;
        }
        scanner.feed(&head).map_err(|_| malformed())?;
        if scanner.closed() {
            continue;
        }
        if head.len() >= SPOOL && wanted.met(&scanner) {
            break;
        }
        if head.len() > read_ahead {
            return Err(GatewayError::with_reason(
                hygiene::BODY_TOO_LARGE,
                Stage::Parse,
                Message::built(format!(
                    "The file part ({}) must start within the first {read_ahead} bytes of the body.",
                    wanted.files.join(" or ")
                )),
            )
            .closing());
        }
    }
    let held = head.len() as u64;
    let state = Arc::new(Mutex::new(State::default()));
    let rest = Rest {
        frames,
        seen: held,
        limit,
        state: Arc::clone(&state),
    };
    let tail = Tail::new(Box::pin(rest), declared.map(|d| d.saturating_sub(held)));
    Ok(Head {
        raw: head.freeze(),
        scanner,
        rest: Some(Upload { tail, state }),
    })
}

/// The streamed rest of a body, counted against the limit; a break is recorded for the client's
/// error and ends the upstream request with an error.
struct Rest {
    frames: Frames,
    seen: u64,
    limit: u64,
    state: Arc<Mutex<State>>,
}

impl Rest {
    fn broke(&self, b: Broke) -> Poll<Option<Result<Bytes, TailError>>> {
        if let Ok(mut s) = self.state.lock() {
            s.broke = Some(b);
        }
        Poll::Ready(Some(Err(Box::new(BodyError::Framing))))
    }
}

impl Stream for Rest {
    type Item = Result<Bytes, TailError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.frames.as_mut().poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some(Err(e))) => {
                let b = decode_error(&e);
                self.broke(b)
            }
            Poll::Ready(Some(Ok(chunk))) => {
                self.seen = self.seen.saturating_add(chunk.len() as u64);
                if self.seen > self.limit {
                    return self.broke(Broke::TooLarge);
                }
                Poll::Ready(Some(Ok(chunk)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_read_ahead_ends_once_a_file_part_started() {
        let wanted = Wanted {
            files: &["image", "image[]"],
        };
        let head = b"--b\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nm\r\n--b\r\nContent-Disposition: form-data; name=\"image[]\"; filename=\"a.png\"\r\n\r\n\x89PNG";
        let mut sc = Scanner::new("b");
        sc.feed(&head[..40]).unwrap();
        assert!(!wanted.met(&sc));
        sc.feed(head).unwrap();
        assert!(wanted.met(&sc));
        let other = Wanted { files: &["file"] };
        assert!(!other.met(&sc));
    }

    #[test]
    fn broken_bodies_answer_the_clients_error() {
        let up = Upload {
            tail: Tail::new(Box::pin(futures_util::stream::empty()), None),
            state: Arc::new(Mutex::new(State::default())),
        };
        assert!(up.broken().is_none());
        up.lock().broke = Some(Broke::TooLarge);
        assert_eq!(
            up.broken().map(|e| e.code),
            Some(antirion_error::Code::PayloadTooLarge)
        );
        let e = GatewayError::with_reason(reasons::INVALID_VALUE, Stage::Attempt, Message::Fixed("x"));
        up.sent_with(&e);
        assert_eq!(up.earlier_error().reason, e.reason);
    }
}
