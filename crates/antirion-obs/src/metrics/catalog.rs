//! The metric families the gateway exposes: name, type, help, label keys and default buckets.

/// The type of a family.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Counter,
    Gauge,
    Histogram,
}

/// One family of the catalogue.
#[derive(Debug)]
pub struct FamilyDef {
    pub name: &'static str,
    pub help: &'static str,
    pub kind: Kind,
    pub labels: &'static [&'static str],
    pub buckets: &'static [f64],
}

pub const DUR_LONG: &[f64] = &[
    0.01, 0.02, 0.04, 0.08, 0.16, 0.32, 0.64, 1.28, 2.56, 5.12, 10.24, 20.48, 40.96, 81.92, 163.84, 327.68,
    655.36,
];
pub const TTFT: &[f64] = &[
    0.001, 0.005, 0.01, 0.02, 0.04, 0.06, 0.08, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0, 20.0, 40.0,
    80.0,
];
pub const TPOT: &[f64] = &[
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.15, 0.2, 0.3, 0.4, 0.5, 0.75, 1.0, 2.5,
];
pub const OVERHEAD: &[f64] = &[
    0.00005, 0.0001, 0.00025, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0,
];
pub const DUR_SHORT: &[f64] = &[
    0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];
pub const TOKENS: &[f64] = &[
    1.0,
    4.0,
    16.0,
    64.0,
    256.0,
    1024.0,
    4096.0,
    16384.0,
    65536.0,
    262_144.0,
    1_048_576.0,
    4_194_304.0,
];

/// Labels kept with their value when a sample is redirected to the `_overflow` series.
pub const KEPT_ON_OVERFLOW: &[&str] = &[
    "operation",
    "stream",
    "status_class",
    "outcome",
    "token_type",
    "result",
    "verdict",
    "stage",
    "layer",
];

macro_rules! catalogue {
    ($($fam:ident = $name:literal, $kind:ident, $help:literal, [$($label:literal),*], $buckets:expr;)*) => {
        /// A family of the catalogue.
        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
        pub enum Fam { $($fam),* }

        impl Fam {
            /// Every family, in exposition order.
            pub const ALL: &'static [Fam] = &[$(Fam::$fam),*];

            #[must_use]
            pub fn def(self) -> &'static FamilyDef {
                match self {
                    $(Fam::$fam => &FamilyDef {
                        name: $name, help: $help, kind: Kind::$kind, labels: &[$($label),*], buckets: $buckets,
                    }),*
                }
            }

            /// The position of the family in [`Fam::ALL`].
            #[must_use]
            pub fn index(self) -> usize {
                self as usize
            }
        }
    };
}

const NONE: &[f64] = &[];

catalogue! {
    BuildInfo = "antirion_build_info", Gauge, "Build identity of this node",
        ["version", "commit", "rustc", "semconv", "test_mode"], NONE;
    StartTime = "antirion_start_time_seconds", Gauge, "Unix time the process started", [], NONE;
    ConfigGeneration = "antirion_config_generation", Gauge, "Config generation this node is serving", [], NONE;
    ConfigApplies = "antirion_config_applies_total", Counter, "Config changes attempted on this node",
        ["source", "result"], NONE;
    ConfigLastApplied = "antirion_config_last_applied_timestamp_seconds", Gauge,
        "Unix time of the last successful config apply", [], NONE;
    Requests = "antirion_requests_total", Counter, "Data-plane requests",
        ["operation", "dialect", "model", "stream", "status_code", "code", "response_path"], NONE;
    RequestDuration = "antirion_request_duration_seconds", Histogram, "Total time of data-plane requests",
        ["operation", "model", "stream", "status_class"], DUR_LONG;
    InFlight = "antirion_requests_in_flight", Gauge, "Data-plane requests in flight", ["operation"], NONE;
    Overhead = "antirion_overhead_seconds", Histogram, "Time the gateway added to a request",
        ["operation", "stream"], OVERHEAD;
    Ttft = "antirion_time_to_first_token_seconds", Histogram, "Time to the first content sent to the client",
        ["operation", "model", "provider", "deployment"], TTFT;
    Tpot = "antirion_time_per_output_token_seconds", Histogram, "Time per output token of streamed responses",
        ["operation", "model", "provider", "deployment"], TPOT;
    UpstreamAttempts = "antirion_upstream_attempts_total", Counter, "Upstream attempts",
        ["provider", "deployment", "upstream_model", "outcome", "status_code"], NONE;
    UpstreamAttemptDuration = "antirion_upstream_attempt_duration_seconds", Histogram,
        "Duration of upstream attempts", ["provider", "deployment", "outcome"], DUR_LONG;
    UpstreamTtft = "antirion_upstream_time_to_first_token_seconds", Histogram,
        "Time from an attempt's start to its first upstream content", ["provider", "deployment"], TTFT;
    Retries = "antirion_retries_total", Counter, "Attempts that retried the same deployment",
        ["provider", "deployment", "failure_class"], NONE;
    Fallbacks = "antirion_fallbacks_total", Counter, "Attempts that moved to another deployment",
        ["model", "from_deployment", "to_deployment"], NONE;
    RoutingDecisions = "antirion_routing_decisions_total", Counter, "Deployments picked by routes",
        ["model", "deployment", "strategy"], NONE;
    Tokens = "antirion_tokens_total", Counter, "Tokens of upstream attempts, by usage field",
        ["model", "provider", "deployment", "upstream_model", "token_type"], NONE;
    TokenUsage = "antirion_token_usage", Histogram, "Tokens per request",
        ["model", "provider", "token_type"], TOKENS;
    Cost = "antirion_cost_usd_total", Counter, "Billed cost in USD", ["model", "provider", "deployment"], NONE;
    UsageMissing = "antirion_usage_missing_total", Counter, "Successful attempts that reported no usage",
        ["provider", "deployment"], NONE;
    Unpriced = "antirion_unpriced_requests_total", Counter, "Requests whose cost could not be computed",
        ["model", "deployment"], NONE;
    CacheLookups = "antirion_cache_lookups_total", Counter, "Response cache lookups",
        ["layer", "result", "model"], NONE;
    CacheWrites = "antirion_cache_writes_total", Counter, "Response cache writes", ["layer", "result"], NONE;
    CacheLookupDuration = "antirion_cache_lookup_duration_seconds", Histogram, "Duration of cache lookups",
        ["layer"], DUR_SHORT;
    GuardrailChecks = "antirion_guardrail_checks_total", Counter, "Guardrail checks",
        ["guardrail", "stage", "verdict"], NONE;
    GuardrailDuration = "antirion_guardrail_duration_seconds", Histogram, "Duration of guardrail checks",
        ["guardrail", "stage"], DUR_SHORT;
    ExtensionCalls = "antirion_extension_calls_total", Counter, "Extension hook calls",
        ["extension", "hook", "result"], NONE;
    ExtensionDuration = "antirion_extension_duration_seconds", Histogram, "Duration of extension hook calls",
        ["extension", "hook"], DUR_SHORT;
    RateLimited = "antirion_rate_limited_total", Counter, "Requests refused by a rate limit",
        ["scope_level", "limit"], NONE;
    AuthFailures = "antirion_auth_failures_total", Counter, "Authentication failures", ["plane", "reason"], NONE;
    LogRecords = "antirion_log_records_total", Counter, "Request-log records per sink", ["sink", "result"], NONE;
    LogQueueDepth = "antirion_log_queue_depth", Gauge, "Request-log records waiting per sink", ["sink"], NONE;
    SpansDropped = "antirion_trace_spans_dropped_total", Counter, "Trace spans that were not exported",
        ["reason"], NONE;
    AlertDeliveries = "antirion_alert_deliveries_total", Counter, "Alert event deliveries per webhook",
        ["webhook", "result"], NONE;
    EmailSent = "antirion_email_sent_total", Counter, "Outgoing mail messages by result", ["result"], NONE;
    PushSent = "antirion_push_sent_total", Counter, "Browser push messages by result", ["result"], NONE;
    SeriesOverflow = "antirion_metrics_series_overflow_total", Counter,
        "Samples redirected to an _overflow series", ["family"], NONE;
    AdmissionInFlight = "antirion_admission_in_flight", Gauge, "Inference requests holding an admission slot",
        [], NONE;
    AdmissionQueueDepth = "antirion_admission_queue_depth", Gauge, "Inference requests waiting for a slot",
        [], NONE;
    AdmissionRejected = "antirion_admission_rejected_total", Counter, "Inference requests refused by admission",
        ["reason"], NONE;
    DependencyUp = "antirion_dependency_up", Gauge, "Whether a dependency of this node is in use (1) or degraded (0)",
        ["dependency"], NONE;
    LedgerRowsDropped = "antirion_ledger_rows_dropped_total", Counter,
        "Ledger rows written neither to the store nor to the spool", [], NONE;
    SpoolBytes = "antirion_spool_bytes", Gauge, "Bytes of ledger rows waiting in the spool", [], NONE;
    ClockSkew = "antirion_clock_skew_seconds", Gauge, "This node's clock minus the store's", [], NONE;
    ShadowRequests = "antirion_shadow_requests_total", Counter, "Shadow copies sent, or dropped beyond max_in_flight",
        ["route", "result"], NONE;
    Feedback = "antirion_feedback_total", Counter, "Feedback accepted, per metric", ["metric"], NONE;
    McpRequests = "antirion_mcp_requests_total", Counter, "MCP requests, per upstream server and method",
        ["server", "method", "outcome"], NONE;
    McpRequestDuration = "antirion_mcp_request_duration_seconds", Histogram, "Duration of MCP requests",
        ["server", "method"], DUR_LONG;
    McpToolDefinitionChanges = "antirion_mcp_tool_definition_changes_total", Counter,
        "MCP tool definitions added, removed or changed upstream", ["server", "change"], NONE;
}

/// The family named `name`, if the catalogue has it.
#[must_use]
pub fn by_name(name: &str) -> Option<Fam> {
    Fam::ALL.iter().copied().find(|f| f.def().name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_match_the_enum() {
        assert_eq!(Fam::BuildInfo.def().name, "antirion_build_info");
        assert_eq!(
            Fam::SeriesOverflow.def().name,
            "antirion_metrics_series_overflow_total"
        );
        assert_eq!(Fam::Requests.def().labels.len(), 7);
        for (i, f) in Fam::ALL.iter().enumerate() {
            assert_eq!(f.index(), i);
        }
        assert_eq!(by_name("antirion_overhead_seconds"), Some(Fam::Overhead));
    }

    #[test]
    fn default_buckets_are_strictly_increasing() {
        for f in Fam::ALL
            .iter()
            .map(|f| f.def())
            .filter(|f| f.kind == Kind::Histogram)
        {
            assert!(!f.buckets.is_empty(), "{}", f.name);
            assert!(f.buckets.windows(2).all(|w| w[0] < w[1]), "{}", f.name);
        }
    }
}
