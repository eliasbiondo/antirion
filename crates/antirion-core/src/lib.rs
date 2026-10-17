//! Shared vocabulary of the gateway: time and clocks, identifiers, lifecycle stages, error envelopes,
//! header names, deterministic randomness and the records that cross crate layers.

#![forbid(unsafe_code)]

pub mod audit;
pub mod envelope;
pub mod headers;
pub mod ids;
pub mod net;
pub mod rng;
pub mod secret;
pub mod stage;
pub mod time;

pub use envelope::Envelope;
pub use ids::{RequestId, ResourceUid};
pub use net::{ClientCert, Peer};
pub use rng::SplitMix64;
pub use secret::Secret;
pub use stage::Stage;
pub use time::{Clock, SystemClock, UtcMicros};
