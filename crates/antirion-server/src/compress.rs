//! Response compression: a complete (non-streaming) response of at least `min_size` bytes is
//! compressed with the client's preferred codec among those enabled. Event streams are never
//! compressed.

use antirion_config::scalar::parse_size;
use bytes::Bytes;
use http::header::{ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, VARY};
use http::{HeaderMap, HeaderValue, Response};
use http_body::Body as _;
use http_body_util::BodyExt;
use serde_json::Value;
use tokio::io::AsyncReadExt;

use antirion_pipeline::Body;

/// A codec the gateway can compress responses with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Codec {
    Zstd,
    Br,
    Gzip,
}

impl Codec {
    fn parse(name: &str) -> Option<Codec> {
        match name {
            "zstd" => Some(Codec::Zstd),
            "br" => Some(Codec::Br),
            "gzip" => Some(Codec::Gzip),
            _ => None,
        }
    }

    fn token(self) -> &'static str {
        match self {
            Codec::Zstd => "zstd",
            Codec::Br => "br",
            Codec::Gzip => "gzip",
        }
    }
}

/// `server.compression`.
#[derive(Clone, Debug)]
pub(crate) struct Settings {
    enabled: bool,
    codecs: Vec<Codec>,
    min_size: u64,
}

impl Settings {
    pub(crate) fn from_statics(statics: &Value) -> Settings {
        let block = statics.pointer("/server/compression");
        let get = |key: &str| block.and_then(|b| b.get(key));
        let codecs = match get("algorithms").and_then(Value::as_array) {
            Some(list) => list
                .iter()
                .filter_map(Value::as_str)
                .filter_map(Codec::parse)
                .collect(),
            None => vec![Codec::Zstd, Codec::Br, Codec::Gzip],
        };
        Settings {
            enabled: get("enabled").and_then(Value::as_bool).unwrap_or(true),
            codecs,
            min_size: get("min_size")
                .and_then(Value::as_str)
                .and_then(parse_size)
                .unwrap_or(1024),
        }
    }

    /// The codec a request's `Accept-Encoding` selects, if compression may apply to its response.
    pub(crate) fn choose(&self, headers: &HeaderMap) -> Option<Codec> {
        if !self.enabled {
            return None;
        }
        let accept = headers
            .get_all(ACCEPT_ENCODING)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect::<Vec<_>>()
            .join(",");
        negotiate(&accept, &self.codecs)
    }

    /// Compresses a complete response with `codec` when it is large enough and not an event stream.
    pub(crate) async fn apply(&self, resp: Response<Body>, codec: Codec) -> Response<Body> {
        let streamed = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| {
                ct.trim_start()
                    .to_ascii_lowercase()
                    .starts_with("text/event-stream")
            });
        let size = resp.body().size_hint().exact();
        let encoded = resp.headers().contains_key(CONTENT_ENCODING);
        let bodiless = resp.status().is_informational() || resp.status() == 204 || resp.status() == 304;
        if streamed || encoded || bodiless || size.is_none_or(|n| n < self.min_size) {
            return resp;
        }
        let (mut parts, body) = resp.into_parts();
        let bytes = match body.collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(_) => return Response::from_parts(parts, empty()),
        };
        let Ok(compressed) = encode(codec, &bytes).await else {
            return Response::from_parts(parts, full(bytes));
        };
        parts.headers.remove(CONTENT_LENGTH);
        parts
            .headers
            .insert(CONTENT_ENCODING, HeaderValue::from_static(codec.token()));
        parts
            .headers
            .append(VARY, HeaderValue::from_static("Accept-Encoding"));
        let body = Unsized(Some(Bytes::from(compressed)));
        Response::from_parts(parts, body.map_err(|never| match never {}).boxed())
    }
}

/// One chunk of bytes whose length is not announced: a compressed response carries no
/// `Content-Length`.
struct Unsized(Option<Bytes>);

impl http_body::Body for Unsized {
    type Data = Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        std::task::Poll::Ready(self.0.take().map(|b| Ok(http_body::Frame::data(b))))
    }

    fn is_end_stream(&self) -> bool {
        self.0.is_none()
    }
}

fn full(bytes: Bytes) -> Body {
    http_body_util::Full::new(bytes)
        .map_err(|never| match never {})
        .boxed()
}

fn empty() -> Body {
    full(Bytes::new())
}

async fn encode(codec: Codec, input: &[u8]) -> std::io::Result<Vec<u8>> {
    use async_compression::tokio::bufread::{BrotliEncoder, GzipEncoder, ZstdEncoder};
    let mut out = Vec::new();
    match codec {
        Codec::Zstd => ZstdEncoder::new(input).read_to_end(&mut out).await?,
        Codec::Br => BrotliEncoder::new(input).read_to_end(&mut out).await?,
        Codec::Gzip => GzipEncoder::new(input).read_to_end(&mut out).await?,
    };
    Ok(out)
}

/// The enabled codec with the highest q-value in `accept`, ties broken by the order of the header;
/// a `*` entry stands for every codec the header does not name.
fn negotiate(accept: &str, enabled: &[Codec]) -> Option<Codec> {
    let mut named: Vec<(Codec, f32, usize)> = Vec::new();
    let mut wildcard: Option<(f32, usize)> = None;
    let mut seen = Vec::new();
    for (pos, item) in accept.split(',').enumerate() {
        let mut fields = item.split(';');
        let name = fields.next().unwrap_or("").trim().to_ascii_lowercase();
        let q = fields
            .filter_map(|p| {
                p.trim()
                    .strip_prefix("q=")
                    .or_else(|| p.trim().strip_prefix("Q="))
            })
            .find_map(|v| v.trim().parse::<f32>().ok())
            .unwrap_or(1.0);
        if name == "*" {
            wildcard = Some((q, pos));
            continue;
        }
        seen.push(name.clone());
        if let Some(c) = Codec::parse(&name).filter(|c| enabled.contains(c)) {
            named.push((c, q, pos));
        }
    }
    if let Some((q, pos)) = wildcard {
        for c in enabled {
            if !seen.iter().any(|s| s == c.token()) {
                named.push((*c, q, pos));
            }
        }
    }
    let best =
        named
            .iter()
            .filter(|(_, q, _)| *q > 0.0)
            .fold(None::<(Codec, f32, usize)>, |best, &(c, q, pos)| match best {
                Some((_, bq, bpos)) if bq > q || (bq == q && bpos <= pos) => best,
                _ => Some((c, q, pos)),
            });
    best.map(|(c, _, _)| c)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: &[Codec] = &[Codec::Zstd, Codec::Br, Codec::Gzip];

    #[test]
    fn the_highest_q_wins_and_ties_follow_the_client_order() {
        assert_eq!(negotiate("gzip, br", ALL), Some(Codec::Gzip));
        assert_eq!(negotiate("br, gzip", ALL), Some(Codec::Br));
        assert_eq!(negotiate("br;q=1, gzip;q=0.5", ALL), Some(Codec::Br));
        assert_eq!(negotiate("gzip;q=0.9, zstd;q=0.8", ALL), Some(Codec::Gzip));
        assert_eq!(negotiate("zstd, gzip", ALL), Some(Codec::Zstd));
        assert_eq!(negotiate("compress, gzip;q=0.1", ALL), Some(Codec::Gzip));
    }

    #[test]
    fn nothing_acceptable_means_no_compression() {
        for accept in ["", "identity", "compress", "gzip;q=0", "*;q=0, identity"] {
            assert_eq!(negotiate(accept, ALL), None, "{accept}");
        }
        assert_eq!(negotiate("br", &[Codec::Gzip]), None);
        assert_eq!(negotiate("*", &[Codec::Gzip]), Some(Codec::Gzip));
    }

    #[tokio::test]
    async fn small_and_streamed_responses_stay_as_they_are() {
        let s = Settings::from_statics(&Value::Null);
        let small = s
            .apply(Response::new(full(Bytes::from_static(b"{}"))), Codec::Gzip)
            .await;
        assert!(!small.headers().contains_key(CONTENT_ENCODING));
        let mut sse = Response::new(full(Bytes::from(vec![b'a'; 4096])));
        sse.headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
        assert!(
            !s.apply(sse, Codec::Gzip)
                .await
                .headers()
                .contains_key(CONTENT_ENCODING)
        );
        let big = s
            .apply(Response::new(full(Bytes::from(vec![b'a'; 4096]))), Codec::Gzip)
            .await;
        assert_eq!(big.headers()[CONTENT_ENCODING], "gzip");
        assert_eq!(big.headers()[VARY], "Accept-Encoding");
        let body = big.into_body().collect().await.unwrap().to_bytes();
        assert!(body.len() < 4096 && body.starts_with(&[0x1f, 0x8b]));
    }
}
