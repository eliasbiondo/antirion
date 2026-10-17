//! The data-plane error model: the closed code table, registered reasons, the one error type and its
//! rendering in every client dialect's envelope.

#![forbid(unsafe_code)]

mod code;
mod error;
pub mod reason;
mod render;

pub use code::{ALL_CODES, Code};
pub use error::{GatewayError, GuardrailFacts, Message, UpstreamError, UpstreamFacts};
pub use reason::Reason;
pub use render::{Rendered, antirion_object, render, retry_after_seconds};
