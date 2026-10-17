//! The config document: strict parsing (JSON and YAML 1.2), validation against the published schema
//! and the semantic rules, secret references and redaction, scalar formats, and typed views of the
//! settings the runtime compiles.

#![forbid(unsafe_code)]

mod access_rules;
pub mod alert_rules;
mod bandit_rules;
mod cache_rules;
mod console_rules;
pub mod console_views;
pub mod cron;
pub mod document;
pub mod emit;
mod error;
mod invalidation_rules;
mod mcp_rules;
mod metrics_rules;
pub mod model;
mod obs_rules;
mod operations_rules;
mod passthrough_rules;
mod path;
mod resilience_rules;
pub mod scalar;
pub mod schema;
pub mod secrets;
pub mod semantic;
mod store_rules;
mod tree_walk;
mod unsupported;
mod validation;
pub mod views;
mod yaml;

pub use access_rules::{IP_SET_PREFIX, auth_references};
pub use document::{Format, parse};
pub use error::{ConfigError, DetailCode};
pub use path::{DocPath, Seg};
pub use validation::{Validation, validate};
