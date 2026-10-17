//! Observability: the metrics registry and its expositions, the request-log record's schema and
//! bounded fields, the overhead formula, traces and their OTLP encodings, and the settings of the
//! `observability` section.

#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unreachable
)]

pub mod feedback;
pub mod ingest;
pub mod metrics;
pub mod otlp_logs;
pub mod record;
pub mod settings;
pub mod sink;
pub mod sketch;
pub mod timing;
pub mod trace;

pub use metrics::{BuildInfo, CustomLabel, Fam, Format, LabelSource, Metrics, MetricsConfig, truncate_utf8};
pub use settings::ObsSettings;

use std::sync::LazyLock;

/// The pinned OpenTelemetry semantic-conventions version the spans and metrics follow.
pub const SEMCONV: &str = "core-1.41.1";
/// The GenAI repository's development line, selected with
/// `OTEL_SEMCONV_STABILITY_OPT_IN=gen_ai_latest_experimental`.
pub const SEMCONV_LATEST: &str = "genai-2026-06-12";

static LATEST: LazyLock<bool> = LazyLock::new(|| {
    std::env::var("OTEL_SEMCONV_STABILITY_OPT_IN")
        .is_ok_and(|v| v.split(',').any(|o| o.trim() == "gen_ai_latest_experimental"))
});

/// Whether spans use the latest experimental GenAI conventions instead of the pinned release.
#[must_use]
pub fn latest_semconv() -> bool {
    *LATEST
}

/// The semantic-conventions version in effect, reported by `antirion_build_info`.
#[must_use]
pub fn semconv() -> &'static str {
    if latest_semconv() { SEMCONV_LATEST } else { SEMCONV }
}
