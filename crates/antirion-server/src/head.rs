//! The first request head of a data-plane connection, read before the HTTP stack sees it, so a
//! request with ambiguous framing gets the gateway's own 400 (with its error envelope) and a closed
//! connection, and a connection that never sends a head is closed without a response.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::{Buf, Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};

const MAX_HEAD: usize = 64 * 1024;
const MAX_HEADER_FIELDS: usize = 200;
pub(crate) const H2_PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n";

/// What the first bytes of a connection showed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum First {
    /// Serve the connection; these bytes were already read.
    Serve(Bytes),
    /// The connection closed or stayed silent before any byte arrived.
    Silent,
    /// A head arrived only in part before the timeout.
    Incomplete { path: String },
    /// The first request's framing is ambiguous or broken.
    BadFraming { path: String },
    /// The first request's header section is over the size or field-count limit.
    TooLarge { path: String },
}

/// Reads the first request head (or the HTTP/2 preface) from `io`.
pub(crate) async fn read_first<S: AsyncRead + Unpin>(io: &mut S, timeout: Duration) -> io::Result<First> {
    let mut buf = BytesMut::with_capacity(4096);
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if buf.len() >= H2_PREFACE.len() && buf.starts_with(H2_PREFACE) {
            return Ok(First::Serve(buf.freeze()));
        }
        if let Some(end) = find_head_end(&buf) {
            let head = buf.get(..end).unwrap_or_default();
            if head.len() > MAX_HEAD || header_fields(head) > MAX_HEADER_FIELDS {
                return Ok(First::TooLarge {
                    path: request_path(head),
                });
            }
            if framing_is_ambiguous(head) {
                return Ok(First::BadFraming {
                    path: request_path(head),
                });
            }
            return Ok(First::Serve(buf.freeze()));
        }
        if buf.len() > MAX_HEAD {
            return Ok(First::TooLarge {
                path: request_path(&buf),
            });
        }
        let read = tokio::time::timeout_at(deadline, io.read_buf(&mut buf)).await;
        match read {
            Err(_) if buf.is_empty() => return Ok(First::Silent),
            Err(_) => {
                return Ok(First::Incomplete {
                    path: request_path(&buf),
                });
            }
            Ok(Ok(0)) if buf.is_empty() => return Ok(First::Silent),
            Ok(Ok(0)) => return Ok(First::Serve(buf.freeze())),
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(e),
        }
    }
}

/// The header lines of a head, after its request line.
fn header_fields(head: &[u8]) -> usize {
    head.split(|b| *b == b'\n')
        .skip(1)
        .filter(|line| !line.iter().all(|b| b.is_ascii_whitespace()))
        .count()
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

fn request_path(head: &[u8]) -> String {
    let line = head.split(|b| *b == b'\n').next().unwrap_or_default();
    let text = String::from_utf8_lossy(line);
    text.split_whitespace()
        .nth(1)
        .map(|p| p.split('?').next().unwrap_or(p).to_owned())
        .unwrap_or_else(|| "/".to_owned())
}

/// Both `Content-Length` and `Transfer-Encoding`, differing or listed lengths, or a
/// `Transfer-Encoding` whose last coding is not `chunked`.
pub(crate) fn framing_is_ambiguous(head: &[u8]) -> bool {
    let text = String::from_utf8_lossy(head);
    let mut lengths: Vec<String> = Vec::new();
    let mut codings: Vec<String> = Vec::new();
    for line in text.split("\r\n").skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        if name == "content-length" {
            lengths.extend(value.split(',').map(|v| v.trim().to_owned()));
        } else if name == "transfer-encoding" {
            codings.extend(
                value
                    .split(',')
                    .map(|v| v.trim().to_ascii_lowercase())
                    .filter(|v| !v.is_empty()),
            );
        }
    }
    if !lengths.is_empty() && !codings.is_empty() {
        return true;
    }
    if lengths.len() > 1
        || lengths
            .iter()
            .any(|l| l.is_empty() || !l.bytes().all(|b| b.is_ascii_digit()))
    {
        return true;
    }
    !codings.is_empty() && codings.last().map(String::as_str) != Some("chunked")
}

/// A stream that replays bytes already read before reading from the socket.
#[derive(Debug)]
pub(crate) struct Rewind<S> {
    prefix: Bytes,
    inner: S,
}

impl<S> Rewind<S> {
    pub(crate) fn new(prefix: Bytes, inner: S) -> Rewind<S> {
        Rewind { prefix, inner }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Rewind<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if !self.prefix.is_empty() {
            let n = self.prefix.len().min(buf.remaining());
            if let Some(chunk) = self.prefix.get(..n) {
                buf.put_slice(chunk);
            }
            self.prefix.advance(n);
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Rewind<S> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncReadExt;

    use super::*;

    fn head(extra: &str) -> Vec<u8> {
        format!("POST /v1/chat/completions HTTP/1.1\r\nhost: x\r\n{extra}\r\n").into_bytes()
    }

    #[test]
    fn ambiguous_framing() {
        assert!(framing_is_ambiguous(&head(
            "content-length: 5\r\ntransfer-encoding: chunked\r\n"
        )));
        assert!(framing_is_ambiguous(&head(
            "content-length: 5\r\ncontent-length: 7\r\n"
        )));
        assert!(framing_is_ambiguous(&head("content-length: 5, 7\r\n")));
        assert!(framing_is_ambiguous(&head(
            "transfer-encoding: chunked, identity\r\n"
        )));
        assert!(!framing_is_ambiguous(&head("content-length: 5\r\n")));
        assert!(!framing_is_ambiguous(&head("transfer-encoding: chunked\r\n")));
        assert!(!framing_is_ambiguous(&head("")));
    }

    #[tokio::test]
    async fn first_head_and_rewind() {
        let bytes = head("content-length: 2\r\n");
        let mut all = bytes.clone();
        all.extend_from_slice(b"{}");
        let mut src: &[u8] = &all;
        let first = read_first(&mut src, Duration::from_secs(1)).await.unwrap();
        let First::Serve(prefix) = first else {
            panic!("{first:?}")
        };
        let mut r = Rewind::new(prefix, src);
        let mut out = Vec::new();
        r.read_to_end(&mut out).await.unwrap();
        assert_eq!(out, all);
        let mut bad: &[u8] =
            b"POST /v1/messages HTTP/1.1\r\ncontent-length: 5\r\ntransfer-encoding: chunked\r\n\r\n";
        assert_eq!(
            read_first(&mut bad, Duration::from_secs(1)).await.unwrap(),
            First::BadFraming {
                path: "/v1/messages".into()
            }
        );
        let mut many = format!(
            "POST /v1/chat/completions HTTP/1.1\r\nhost: x\r\n{}\r\n",
            "x-h: v\r\n".repeat(200)
        )
        .into_bytes();
        assert!(matches!(
            read_first(&mut many.as_slice(), Duration::from_secs(1))
                .await
                .unwrap(),
            First::TooLarge { .. }
        ));
        many = format!("GET / HTTP/1.1\r\nx-pad: {}\r\n", "a".repeat(70_000)).into_bytes();
        assert!(matches!(
            read_first(&mut many.as_slice(), Duration::from_secs(1))
                .await
                .unwrap(),
            First::TooLarge { .. }
        ));
        let mut silent: &[u8] = b"";
        assert_eq!(
            read_first(&mut silent, Duration::from_secs(1)).await.unwrap(),
            First::Silent
        );
    }
}
