//! The tenant-scoped response cache: policy, directives, key builder and backends, the semantic
//! layer and the coalescing of identical concurrent misses.

#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unreachable
)]

pub mod directive;
pub mod entry;
pub mod flights;
pub mod invalidate;
pub mod jcs;
pub mod key;
pub mod listing;
pub mod memory;
pub mod meta;
pub mod policy;
pub mod prompt;
pub mod rules;
pub mod semantic;
pub mod service;
pub mod valkey;
pub mod vectors;

/// Reasons of the cache rules.
pub mod reasons {
    antirion_error::reasons! {
        INVALID_CACHE_DIRECTIVE = "invalid_cache_directive" => InvalidRequest,
        CACHE_TTL_ABOVE_MAX = "cache_ttl_above_max" => InvalidRequest,
        CACHE_UNAVAILABLE = "cache_unavailable" => DependencyUnavailable,
    }
}

pub use directive::{Directives, Refusal};
pub use entry::{Entry, StoredEvent};
pub use meta::{EntryMeta, Mode};
pub use policy::{CachePolicy, PromptCaching, Scope, SemanticPolicy};
pub use service::{Cache, Epochs, Health, Settings};

/// Why a backend call did not complete.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CacheError {
    #[error("the cache backend is unavailable")]
    Unavailable,
    #[error("the cache backend did not answer in time")]
    Timeout,
    #[error("the cache backend answered unexpectedly")]
    Protocol,
}

/// The `x-antirion-cache` value of a request that reached the lookup.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Hit,
    Miss,
    Refresh,
    Bypass,
    Uncacheable,
    Disabled,
    Error,
}

impl Status {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Status::Hit => "hit",
            Status::Miss => "miss",
            Status::Refresh => "refresh",
            Status::Bypass => "bypass",
            Status::Uncacheable => "uncacheable",
            Status::Disabled => "disabled",
            Status::Error => "error",
        }
    }
}
