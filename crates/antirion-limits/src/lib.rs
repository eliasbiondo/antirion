//! Limits, budgets and cost: declarations from every policy level, the applicable counters of a
//! request, the token estimator, exact counter arithmetic, the reservation protocol over a
//! counter store (in memory or Valkey), degraded mode, pricing and the cost formula, ledger lines
//! and the rate-limit and cost headers.

#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unreachable
)]

pub mod allocation;
pub mod applicable;
pub mod book;
pub mod bypass;
pub mod catalog;
pub mod check;
pub mod cost;
pub mod counter;
pub mod decl;
pub mod degraded;
pub mod engine;
pub mod estimate;
pub mod forecast;
pub mod headers;
pub mod imagetokens;
pub mod ledger;
pub mod level;
pub mod math;
pub mod money;
pub mod overlay;
pub mod period;
pub mod plan;
pub mod policy;
pub mod price;
pub mod priority;
pub mod recost;
pub mod refusal;
pub mod reservation;
pub mod settings;
pub mod shadow;
pub mod spend;
pub mod store;
pub mod usage;
pub mod weights;

/// Reasons of the limits, budgets and pricing rules.
pub mod reasons {
    antirion_error::reasons! {
        LIMITS_UNAVAILABLE = "limits_unavailable" => DependencyUnavailable,
        MODEL_UNPRICED = "model_unpriced" => Forbidden,
        MISSING_PARTITION_ATTRIBUTE = "missing_partition_attribute" => InvalidRequest,
        OUTPUT_CAP_OUT_OF_RANGE = "output_cap_out_of_range" => InvalidRequest,
        DEPLOYMENT_LIMITS = "deployment_limits" => RateLimited,
    }
}

pub use level::Level;
