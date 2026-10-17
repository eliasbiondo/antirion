//! Client dialects: the endpoint catalog, envelope selection by path, and the request facts read at
//! `parse`.

#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unreachable
)]

pub mod embed;
pub mod endpoints;
pub mod facts;
pub mod rerank;
pub mod validate;
pub mod xlate;

pub use endpoints::{
    Dialect, Endpoint, Kind, Matched, envelope_for_path, legacy_path, match_endpoint, normalise, passthrough,
    responses_socket,
};
pub use facts::{BodyFacts, ModelField, body_facts};
pub use validate::{ValidateOpts, Violation, ViolationKind, openai_code, validate_json};
