//! Server-sent events, AWS event-stream framing, streams of JSON values, multipart streaming and
//! bounded decompression.

#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unreachable
)]

pub mod decode;
pub mod eventstream;
pub mod jsonstream;
pub mod multipart;
pub mod sse;
