//! HTTP content codings: parsing `Content-Encoding`, and decoding a body whole with a cap on its
//! decoded size or incrementally as its bytes arrive.

use std::io;
use std::pin::Pin;

use async_compression::tokio::bufread::{
    BrotliDecoder, DeflateDecoder, GzipDecoder, ZlibDecoder, ZstdDecoder,
};
use bytes::{Bytes, BytesMut};
use futures_util::stream::{self, BoxStream};
use futures_util::{Stream, StreamExt};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio_util::io::{ReaderStream, StreamReader};

/// Errors a body stream can yield.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A single content coding.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Coding {
    Identity,
    Gzip,
    Deflate,
    Brotli,
    Zstd,
}

impl Coding {
    /// The coding's name in `Content-Encoding`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Coding::Identity => "identity",
            Coding::Gzip => "gzip",
            Coding::Deflate => "deflate",
            Coding::Brotli => "br",
            Coding::Zstd => "zstd",
        }
    }
}

/// A `Content-Encoding` that is unknown or stacks several codings.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unsupported content encoding")]
pub struct UnsupportedCoding;

/// Why a body could not be decoded.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("the decoded body exceeds its size limit")]
    TooLarge,
    #[error("the body is not valid for its content encoding")]
    Corrupt,
    #[error("reading the body failed")]
    Source(BoxError),
}

/// Parses the `Content-Encoding` fields of a message, given as their raw values: no field, empty
/// fields and `identity` entries are [`Coding::Identity`]; exactly one known coding is that coding;
/// unknown names, non-text values and stacked codings (in one field or across fields) are refused.
pub fn parse_coding<'a>(fields: impl IntoIterator<Item = &'a [u8]>) -> Result<Coding, UnsupportedCoding> {
    let mut found = Coding::Identity;
    for field in fields {
        let text = std::str::from_utf8(field).map_err(|_| UnsupportedCoding)?;
        for token in text.split(',').map(str::trim).filter(|t| !t.is_empty()) {
            let coding = match token.to_ascii_lowercase().as_str() {
                "identity" => continue,
                "gzip" | "x-gzip" => Coding::Gzip,
                "deflate" => Coding::Deflate,
                "br" => Coding::Brotli,
                "zstd" => Coding::Zstd,
                _ => return Err(UnsupportedCoding),
            };
            if found != Coding::Identity {
                return Err(UnsupportedCoding);
            }
            found = coding;
        }
    }
    Ok(found)
}

/// Carries an error of the input stream through the decoder's `io::Error`, so it is reported as
/// [`DecodeError::Source`] and not as corrupt data.
#[derive(Debug, thiserror::Error)]
#[error("body source error")]
struct SourceError(BoxError);

fn classify(e: io::Error) -> DecodeError {
    match e.into_inner().map(|inner| inner.downcast::<SourceError>()) {
        Some(Ok(source)) => DecodeError::Source(source.0),
        _ => DecodeError::Corrupt,
    }
}

type Chunks<'a> = BoxStream<'a, Result<Bytes, BoxError>>;

/// Reads chunks until at least two bytes are known, so `deflate` can tell a zlib header from raw
/// deflate; returns the stream with those chunks put back in front.
async fn peek<'a>(mut input: Chunks<'a>) -> (Vec<u8>, Chunks<'a>) {
    let mut seen = Vec::new();
    let mut head = Vec::new();
    while seen.len() < 2 {
        match input.next().await {
            Some(Ok(chunk)) => {
                seen.extend_from_slice(
                    chunk
                        .get(..2usize.saturating_sub(seen.len()).min(chunk.len()))
                        .unwrap_or_default(),
                );
                head.push(Ok(chunk));
            }
            Some(Err(e)) => {
                head.push(Err(e));
                break;
            }
            None => break,
        }
    }
    (seen, stream::iter(head).chain(input).boxed())
}

fn is_zlib_header(first: &[u8]) -> bool {
    match first {
        [cmf, flg, ..] => {
            cmf & 0x0F == 8 && cmf >> 4 <= 7 && ((u16::from(*cmf) << 8) | u16::from(*flg)) % 31 == 0
        }
        _ => true,
    }
}

async fn reader<'a>(coding: Coding, input: Chunks<'a>) -> Pin<Box<dyn AsyncRead + Send + 'a>> {
    let (input, zlib) = if coding == Coding::Deflate {
        let (first, input) = peek(input).await;
        (input, is_zlib_header(&first))
    } else {
        (input, true)
    };
    let source = StreamReader::new(input.map(|r| r.map_err(|e| io::Error::other(SourceError(e)))));
    match coding {
        Coding::Identity => Box::pin(source),
        Coding::Gzip => Box::pin(GzipDecoder::new(source)),
        Coding::Deflate if zlib => Box::pin(ZlibDecoder::new(source)),
        Coding::Deflate => Box::pin(DeflateDecoder::new(source)),
        Coding::Brotli => Box::pin(BrotliDecoder::new(source)),
        Coding::Zstd => Box::pin(ZstdDecoder::new(source)),
    }
}

const READ_CHUNK: usize = 64 * 1024;

/// Decodes a whole body. Decoding stops with [`DecodeError::TooLarge`] as soon as the decoded size
/// exceeds `cap`, so a small body that inflates to a huge one is never inflated past the cap.
pub async fn decode_all<S>(coding: Coding, input: S, cap: u64) -> Result<Bytes, DecodeError>
where
    S: Stream<Item = Result<Bytes, BoxError>> + Send + Unpin,
{
    let mut r = reader(coding, input.boxed()).await;
    let mut out = BytesMut::new();
    loop {
        out.reserve(READ_CHUNK);
        let n = r.read_buf(&mut out).await.map_err(classify)?;
        if u64::try_from(out.len()).unwrap_or(u64::MAX) > cap {
            return Err(DecodeError::TooLarge);
        }
        if n == 0 {
            return Ok(out.freeze());
        }
    }
}

/// Decodes a body incrementally: decoded bytes are yielded as soon as the decoder produces them.
/// [`Coding::Identity`] passes the chunks through unchanged.
pub fn decode_stream<S>(coding: Coding, input: S) -> impl Stream<Item = Result<Bytes, DecodeError>> + Send
where
    S: Stream<Item = Result<Bytes, BoxError>> + Send + Unpin + 'static,
{
    if coding == Coding::Identity {
        return input.map(|r| r.map_err(DecodeError::Source)).boxed();
    }
    stream::once(async move {
        let r = reader(coding, input.boxed()).await;
        ReaderStream::with_capacity(r, READ_CHUNK).map(|r| r.map_err(classify))
    })
    .flatten()
    .boxed()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use async_compression::tokio::bufread::{
        BrotliEncoder, DeflateEncoder, GzipEncoder, ZlibEncoder, ZstdEncoder,
    };
    use tokio::io::AsyncReadExt;
    use tokio::sync::mpsc;

    use super::*;

    async fn encode(coding: Coding, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        match coding {
            Coding::Identity => out.extend_from_slice(data),
            Coding::Gzip => GzipEncoder::new(data)
                .read_to_end(&mut out)
                .await
                .map(drop)
                .unwrap(),
            Coding::Deflate => ZlibEncoder::new(data)
                .read_to_end(&mut out)
                .await
                .map(drop)
                .unwrap(),
            Coding::Brotli => BrotliEncoder::new(data)
                .read_to_end(&mut out)
                .await
                .map(drop)
                .unwrap(),
            Coding::Zstd => ZstdEncoder::new(data)
                .read_to_end(&mut out)
                .await
                .map(drop)
                .unwrap(),
        }
        out
    }

    fn chunked(
        data: Vec<u8>,
        size: usize,
    ) -> impl Stream<Item = Result<Bytes, BoxError>> + Send + Unpin + 'static {
        let chunks: Vec<Result<Bytes, BoxError>> =
            data.chunks(size).map(|c| Ok(Bytes::copy_from_slice(c))).collect();
        stream::iter(chunks)
    }

    const ALL: [Coding; 5] = [
        Coding::Identity,
        Coding::Gzip,
        Coding::Deflate,
        Coding::Brotli,
        Coding::Zstd,
    ];

    #[test]
    fn content_encoding_values() {
        let one = |v: &str| parse_coding([v.as_bytes()]);
        assert_eq!(parse_coding([]), Ok(Coding::Identity));
        assert_eq!(one(" "), Ok(Coding::Identity));
        assert_eq!(one("Identity"), Ok(Coding::Identity));
        assert_eq!(one(" GZIP "), Ok(Coding::Gzip));
        assert_eq!(one("deflate"), Ok(Coding::Deflate));
        assert_eq!(one("br"), Ok(Coding::Brotli));
        assert_eq!(one("zstd"), Ok(Coding::Zstd));
        assert_eq!(one("identity, gzip"), Ok(Coding::Gzip));
        for bad in [
            "lz4",
            "compress",
            "gzip, gzip",
            "gzip, br",
            "x-gzip-custom",
            "gzip;q=1",
        ] {
            assert_eq!(one(bad), Err(UnsupportedCoding), "{bad}");
        }
        assert_eq!(
            parse_coding([b"gzip".as_slice(), b"br".as_slice()]),
            Err(UnsupportedCoding)
        );
        assert_eq!(parse_coding([b"\xff".as_slice()]), Err(UnsupportedCoding));
    }

    #[tokio::test]
    async fn round_trips() {
        let data: Vec<u8> = (0..200_000u32).flat_map(|i| (i % 251).to_le_bytes()).collect();
        for coding in ALL {
            let enc = encode(coding, &data).await;
            let all = decode_all(coding, chunked(enc.clone(), 777), 10 << 20)
                .await
                .unwrap();
            assert_eq!(&all[..], &data[..], "{coding:?}");
            let streamed: Vec<u8> = decode_stream(coding, chunked(enc, 1))
                .map(|r| r.unwrap().to_vec())
                .concat()
                .await;
            assert_eq!(streamed, data, "{coding:?}");
        }
    }

    #[tokio::test]
    async fn raw_deflate_is_accepted() {
        let mut enc = Vec::new();
        DeflateEncoder::new(&b"hello raw deflate"[..])
            .read_to_end(&mut enc)
            .await
            .unwrap();
        let out = decode_all(Coding::Deflate, chunked(enc, 1), 1024).await.unwrap();
        assert_eq!(&out[..], b"hello raw deflate");
    }

    #[tokio::test]
    async fn a_bomb_stops_at_the_cap() {
        let bomb = encode(Coding::Gzip, &vec![0u8; 10 << 20]).await;
        assert!(bomb.len() < 64 * 1024);
        let started = std::time::Instant::now();
        let r = decode_all(Coding::Gzip, chunked(bomb, 4096), 1 << 20).await;
        assert!(matches!(r, Err(DecodeError::TooLarge)));
        assert!(started.elapsed() < Duration::from_secs(2));
        let exact = encode(Coding::Zstd, &[7u8; 1000]).await;
        assert_eq!(
            decode_all(Coding::Zstd, chunked(exact.clone(), 10), 1000)
                .await
                .unwrap()
                .len(),
            1000
        );
        assert!(matches!(
            decode_all(Coding::Zstd, chunked(exact, 10), 999).await,
            Err(DecodeError::TooLarge)
        ));
    }

    #[tokio::test]
    async fn corrupt_and_truncated_input() {
        for coding in [Coding::Gzip, Coding::Deflate, Coding::Brotli, Coding::Zstd] {
            let r = decode_all(coding, chunked(b"{\"not\": \"compressed\"}".to_vec(), 5), 1024).await;
            assert!(matches!(r, Err(DecodeError::Corrupt)), "{coding:?}: {r:?}");
            let mut enc = encode(coding, &[1u8; 5000]).await;
            enc.truncate(enc.len() / 2);
            let r = decode_all(coding, chunked(enc, 5), 1 << 20).await;
            assert!(
                matches!(r, Err(DecodeError::Corrupt)),
                "truncated {coding:?}: {r:?}"
            );
        }
    }

    #[tokio::test]
    async fn source_errors_are_reported_as_such() {
        let enc = encode(Coding::Gzip, b"abc").await;
        let items: Vec<Result<Bytes, BoxError>> =
            vec![Ok(Bytes::from(enc).slice(..4)), Err("connection reset".into())];
        let r = decode_all(Coding::Gzip, stream::iter(items), 1024).await;
        assert!(matches!(r, Err(DecodeError::Source(e)) if e.to_string() == "connection reset"));
    }

    #[tokio::test]
    async fn decoded_bytes_flow_before_the_input_ends() {
        for coding in [Coding::Gzip, Coding::Deflate, Coding::Brotli, Coding::Zstd] {
            let first = b"data: {\"a\":1}\n\n";
            let mut enc = Vec::new();
            match coding {
                Coding::Gzip => {
                    use async_compression::tokio::write::GzipEncoder as W;
                    flushed(W::new(&mut enc), first).await;
                }
                Coding::Deflate => {
                    use async_compression::tokio::write::ZlibEncoder as W;
                    flushed(W::new(&mut enc), first).await;
                }
                Coding::Brotli => {
                    use async_compression::tokio::write::BrotliEncoder as W;
                    flushed(W::new(&mut enc), first).await;
                }
                _ => {
                    use async_compression::tokio::write::ZstdEncoder as W;
                    flushed(W::new(&mut enc), first).await;
                }
            }
            let (tx, rx) = mpsc::channel::<Result<Bytes, BoxError>>(4);
            tx.send(Ok(Bytes::from(enc))).await.unwrap();
            let rx = Box::pin(stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|i| (i, rx))
            }));
            let mut out = decode_stream(coding, rx);
            let got = tokio::time::timeout(Duration::from_secs(2), out.next()).await;
            let got = got.unwrap_or_else(|_| panic!("{coding:?}: no output before the input ended"));
            assert_eq!(&got.unwrap().unwrap()[..], first, "{coding:?}");
            drop(tx);
        }
    }

    async fn flushed<W: tokio::io::AsyncWrite + Unpin>(mut w: W, data: &[u8]) {
        use tokio::io::AsyncWriteExt;
        w.write_all(data).await.unwrap();
        w.flush().await.unwrap();
    }
}
