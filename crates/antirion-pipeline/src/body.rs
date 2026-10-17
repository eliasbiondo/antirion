//! Reading a request body after authentication: counted against the limit while it arrives, with a
//! bound on the gap between body bytes.

use std::time::Duration;

use bytes::{Bytes, BytesMut};
use futures_util::Stream;
use http_body_util::BodyExt;
use hyper::body::Incoming;

use crate::ctx::BoxError;

/// Why a body could not be read.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum BodyError {
    #[error("the body is larger than the limit")]
    TooLarge,
    /// Broken framing: an invalid chunk, or the connection ended before the declared length.
    #[error("the body is not framed correctly")]
    Framing,
    #[error("the body did not arrive in time")]
    Timeout,
}

/// Reads the whole body; `gap` bounds the wait for each next frame. A body that arrives in one frame
/// is kept as that frame; otherwise the buffer is sized from `declared` (the `Content-Length`, within
/// the limit), so it is never copied while it grows.
pub(crate) async fn read(
    mut body: Incoming,
    limit: u64,
    gap: Duration,
    declared: Option<u64>,
) -> Result<Bytes, BodyError> {
    let mut first: Option<Bytes> = None;
    let mut buf = BytesMut::new();
    loop {
        let frame = match tokio::time::timeout(gap, body.frame()).await {
            Err(_) => return Err(BodyError::Timeout),
            Ok(None) => break,
            Ok(Some(Err(_))) => return Err(BodyError::Framing),
            Ok(Some(Ok(frame))) => frame,
        };
        if let Ok(data) = frame.into_data() {
            let held = buf.len() + first.as_ref().map_or(0, Bytes::len);
            let total = u64::try_from(held.saturating_add(data.len())).unwrap_or(u64::MAX);
            if total > limit {
                return Err(BodyError::TooLarge);
            }
            if buf.is_empty() && first.is_none() {
                first = Some(data);
                continue;
            }
            if let Some(f) = first.take() {
                let hint = declared.unwrap_or(0).min(limit);
                let want = usize::try_from(hint).unwrap_or(0).max(f.len() + data.len());
                buf.reserve(want);
                buf.extend_from_slice(&f);
            }
            buf.extend_from_slice(&data);
        }
    }
    Ok(match first {
        Some(f) => f,
        None => buf.freeze(),
    })
}

/// The body's data frames as a stream, with the same gap bound as [`read`].
pub(crate) fn stream(
    body: Incoming,
    gap: Duration,
) -> impl Stream<Item = Result<Bytes, BoxError>> + Send + Unpin + 'static {
    Box::pin(futures_util::stream::unfold(
        Some(body),
        move |state| async move {
            let mut body = state?;
            loop {
                match tokio::time::timeout(gap, body.frame()).await {
                    Err(_) => return Some((Err(Box::new(BodyError::Timeout) as BoxError), None)),
                    Ok(None) => return None,
                    Ok(Some(Err(_))) => return Some((Err(Box::new(BodyError::Framing) as BoxError), None)),
                    Ok(Some(Ok(frame))) => {
                        if let Ok(data) = frame.into_data() {
                            return Some((Ok(data), Some(body)));
                        }
                    }
                }
            }
        },
    ))
}
