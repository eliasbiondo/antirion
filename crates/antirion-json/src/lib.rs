//! Strict JSON validation and byte-preserving edits.

#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unreachable
)]

mod skim;
mod splice;

pub use skim::{JsonError, JsonErrorKind, JsonLimits, Member, PathSeg, Skim, skim};
pub use splice::Splice;
