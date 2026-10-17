//! The extension hook model and its carriers: webhooks, extensions compiled into the binary
//! (through the stable [`api`]) and Wasm components.

#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unreachable
)]

pub mod api;
pub mod builtins;
pub mod hook;
pub mod patch;
pub mod plugin;
mod registry;
pub mod wasm;

pub use hook::{HookAction, HookFacts, Reply, parse_answer, payload, signature};
pub use plugin::{Carrier, Hook, Plugin, Plugins, check_document, compile, resolve};
