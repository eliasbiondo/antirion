//! Guardrail definitions, checks, transforms, redaction and streaming modes.

#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unreachable
)]

pub mod attach;
mod compile;
pub mod def;
pub mod endpoints;
pub mod length;
pub mod limiter;
pub mod moderation;
pub mod partners;
mod pattern;
pub mod pii;
pub mod preserve;
pub mod redact;
pub mod repair;
pub mod rewrite;
pub mod runner;
pub mod scrub;
pub mod secrets;
pub mod segments;
pub mod services;
pub mod structured;
pub mod template;
pub mod tokens;
pub mod tools;
pub mod topic;
pub mod transform;
pub mod webhook;
pub mod window;

pub use compile::{check_document, compile};
pub use def::{Action, Category, Check, Def, GuardStage, Guardrails, SegKind};
pub use pattern::{Span, Validate};
pub use runner::{Evaluation, Net, StageCx, StageOutcome, Verdict, run_stage};
