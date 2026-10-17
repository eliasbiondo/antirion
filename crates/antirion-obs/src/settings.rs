//! The `observability` settings of one generation, read from the validated, secret-resolved document
//! with every absent field defaulted.

use std::collections::BTreeMap;
use std::time::Duration;

use antirion_config::scalar::{parse_duration, parse_size};
use serde_json::Value;

use crate::metrics::MetricsConfig;
use crate::trace::Sampler;

/// What `client_ip` the record carries.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ClientIpMode {
    Full,
    /// IPv4 /24, IPv6 /48.
    Truncated,
    None,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BatchFormat {
    Ndjson,
    JsonArray,
    /// A trace-ingestion batch (`sinks.langfuse`): one trace per record, one generation per attempt.
    TraceIngest,
    /// An OTLP logs export (`sinks.otlp`): one LogRecord per record.
    OtlpLogs(TraceProtocol),
}

impl BatchFormat {
    #[must_use]
    pub fn content_type(self) -> &'static str {
        match self {
            BatchFormat::Ndjson => "application/x-ndjson",
            BatchFormat::JsonArray
            | BatchFormat::TraceIngest
            | BatchFormat::OtlpLogs(TraceProtocol::HttpJson) => "application/json",
            BatchFormat::OtlpLogs(TraceProtocol::HttpProtobuf) => "application/x-protobuf",
            BatchFormat::OtlpLogs(TraceProtocol::Grpc) => "application/grpc",
        }
    }
}

/// An `observability.request_logs.sinks.http` entry.
#[derive(Clone, Debug, PartialEq)]
pub struct HttpSinkSettings {
    pub name: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub format: BatchFormat,
    pub include_content: bool,
    pub batch_max_records: usize,
    pub flush_interval: Duration,
    pub timeout: Duration,
    pub queue_capacity: usize,
    pub max_retry_duration: Duration,
    pub gzip: bool,
    /// The `environment` a trace-ingestion sink reports.
    pub environment: Option<String>,
    /// The OTel resource an OTLP sink exports under: the traces' service name and resource
    /// attributes.
    pub resource: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RequestLogSettings {
    pub enabled: bool,
    pub include_unauthenticated: bool,
    pub max_body_bytes: usize,
    pub log_upstream_error_bodies: bool,
    pub upstream_error_body_max: usize,
    /// Lowercase names of the request headers recorded.
    pub request_headers: Vec<String>,
    pub response_headers: Vec<String>,
    pub client_ip: ClientIpMode,
    pub user_agent: bool,
    pub store: bool,
    /// The stdout sink, when enabled: whether it includes content.
    pub stdout: Option<bool>,
    pub http: Vec<HttpSinkSettings>,
    pub shutdown_flush_timeout: Duration,
    /// How long the store keeps records, and their content.
    pub retention: Duration,
    pub content_retention: Duration,
    /// The most records one export returns.
    pub export_max_rows: u64,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TraceProtocol {
    Grpc,
    HttpProtobuf,
    HttpJson,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TraceSettings {
    pub enabled: bool,
    pub service_name: String,
    pub resource_attributes: Vec<(String, String)>,
    pub protocol: TraceProtocol,
    pub endpoint: Option<String>,
    pub headers: Vec<(String, String)>,
    pub timeout: Duration,
    pub gzip: bool,
    pub sampler: Sampler,
    pub propagate_to_upstream: bool,
    /// Request header (lowercase) → root span attribute.
    pub header_attributes: Vec<(String, String)>,
    pub include_end_user: bool,
    pub max_queue_size: usize,
    pub max_export_batch_size: usize,
    pub schedule_delay: Duration,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MetricsSettings {
    pub enabled: bool,
    pub path: String,
    pub auth_required: bool,
    pub config: MetricsConfig,
}

/// Limits on recorded request metadata.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct MetadataLimits {
    pub max_entries: usize,
    pub max_value_bytes: usize,
}

/// Everything under `observability`.
#[derive(Clone, Debug, PartialEq)]
pub struct ObsSettings {
    pub request_logs: RequestLogSettings,
    pub metrics: MetricsSettings,
    pub traces: TraceSettings,
    pub metadata: MetadataLimits,
    pub upstream_request_id_header: bool,
    /// `x-antirion-version` on data-plane responses.
    pub version_header: bool,
    pub feedback: crate::feedback::FeedbackSettings,
    /// The tag key whose values are environments.
    pub environment_tag: String,
    /// The GeoIP database file, when lookups are configured.
    pub geoip_database: Option<String>,
}

struct At<'a>(Option<&'a Value>);

impl<'a> At<'a> {
    fn get(&self, path: &str) -> At<'a> {
        At(self
            .0
            .and_then(|root| path.split('.').try_fold(root, |v, k| v.get(k)))
            .filter(|v| !v.is_null()))
    }

    fn bool(&self, path: &str, default: bool) -> bool {
        self.get(path).0.and_then(Value::as_bool).unwrap_or(default)
    }

    fn str(&self, path: &str) -> Option<&'a str> {
        self.get(path).0.and_then(Value::as_str)
    }

    fn usize(&self, path: &str, default: usize) -> usize {
        self.get(path)
            .0
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(default)
    }

    fn duration(&self, path: &str, default: Duration) -> Duration {
        self.str(path).and_then(parse_duration).unwrap_or(default)
    }

    fn size(&self, path: &str, default: usize) -> usize {
        self.str(path)
            .and_then(parse_size)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(default)
    }

    fn names(&self, path: &str) -> Vec<String> {
        self.get(path)
            .0
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_ascii_lowercase)
            .collect()
    }

    fn pairs(&self, path: &str) -> Vec<(String, String)> {
        self.get(path)
            .0
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
            .collect()
    }
}

fn http_sink(name: &str, v: &Value) -> Option<HttpSinkSettings> {
    let at = At(Some(v));
    Some(HttpSinkSettings {
        name: name.to_owned(),
        url: at.str("url")?.to_owned(),
        headers: at.pairs("headers"),
        format: match at.str("format") {
            Some("json_array") => BatchFormat::JsonArray,
            _ => BatchFormat::Ndjson,
        },
        include_content: at.bool("include_content", false),
        batch_max_records: at.usize("batch_max_records", 500).max(1),
        flush_interval: at.duration("flush_interval", Duration::from_secs(1)),
        timeout: at.duration("timeout", Duration::from_secs(10)),
        queue_capacity: at.usize("queue_capacity", 10_000).max(1),
        max_retry_duration: at.duration("max_retry_duration", Duration::from_secs(300)),
        gzip: at.str("compression") != Some("none"),
        environment: None,
        resource: Vec::new(),
    })
}

/// A `sinks.langfuse` entry: an HTTP sink that posts trace-ingestion batches with basic auth.
fn ingest_sink(name: &str, v: &Value) -> Option<HttpSinkSettings> {
    use base64::Engine as _;
    let at = At(Some(v));
    let base = at.str("url")?.trim_end_matches('/');
    let auth = base64::engine::general_purpose::STANDARD.encode(format!(
        "{}:{}",
        at.str("public_key")?,
        at.str("secret_key")?
    ));
    Some(HttpSinkSettings {
        name: name.to_owned(),
        url: format!("{base}{}", crate::ingest::PATH),
        headers: vec![("authorization".to_owned(), format!("Basic {auth}"))],
        format: BatchFormat::TraceIngest,
        include_content: at.bool("include_content", false),
        batch_max_records: at.usize("batch_max_records", 100).max(1),
        flush_interval: at.duration("flush_interval", Duration::from_secs(1)),
        timeout: at.duration("timeout", Duration::from_secs(10)),
        queue_capacity: at.usize("queue_capacity", 10_000).max(1),
        max_retry_duration: at.duration("max_retry_duration", Duration::from_secs(300)),
        gzip: false,
        environment: at.str("environment").map(str::to_owned),
        resource: Vec::new(),
    })
}

/// The path of the gRPC method that receives logs.
pub const OTLP_LOGS_GRPC_PATH: &str = "/opentelemetry.proto.collector.logs.v1.LogsService/Export";

/// A `sinks.otlp` entry: an HTTP sink whose batches are OTLP logs exports.
fn otlp_sink(name: &str, v: &Value, resource: &[(String, String)]) -> Option<HttpSinkSettings> {
    let at = At(Some(v));
    let endpoint = at.str("endpoint")?.trim_end_matches('/');
    let protocol = match at.str("protocol") {
        Some("grpc") => TraceProtocol::Grpc,
        Some("http/json") => TraceProtocol::HttpJson,
        _ => TraceProtocol::HttpProtobuf,
    };
    let url = match protocol {
        TraceProtocol::Grpc => format!("{endpoint}{OTLP_LOGS_GRPC_PATH}"),
        _ => format!("{endpoint}/v1/logs"),
    };
    Some(HttpSinkSettings {
        name: name.to_owned(),
        url,
        headers: at.pairs("headers"),
        format: BatchFormat::OtlpLogs(protocol),
        include_content: at.bool("include_content", false),
        batch_max_records: at.usize("batch_max_records", 512).max(1),
        flush_interval: at.duration("flush_interval", Duration::from_secs(1)),
        timeout: at.duration("timeout", Duration::from_secs(10)),
        queue_capacity: at.usize("queue_capacity", 10_000).max(1),
        max_retry_duration: at.duration("max_retry_duration", Duration::from_secs(300)),
        gzip: protocol != TraceProtocol::Grpc && at.str("compression") != Some("none"),
        environment: None,
        resource: resource.to_vec(),
    })
}

/// The OTel resource of exported logs: the traces' `service_name` as `service.name`, then their
/// `resource_attributes`, which win.
fn resource(traces: &At<'_>) -> Vec<(String, String)> {
    let mut out = vec![(
        "service.name".to_owned(),
        traces.str("service_name").unwrap_or("antirion").to_owned(),
    )];
    for (k, v) in traces.pairs("resource_attributes") {
        out.retain(|(name, _)| *name != k);
        out.push((k, v));
    }
    out
}

fn sampler(at: &At<'_>) -> Sampler {
    let ratio = at
        .get("sampler.ratio")
        .0
        .and_then(Value::as_f64)
        .unwrap_or(1.0)
        .clamp(0.0, 1.0);
    match at.str("sampler.type") {
        Some("ratio") => Sampler::Ratio(ratio),
        Some("always_on") => Sampler::AlwaysOn,
        Some("always_off") => Sampler::AlwaysOff,
        _ => Sampler::ParentBasedRatio(ratio),
    }
}

impl ObsSettings {
    /// The settings of a resolved document.
    #[must_use]
    pub fn compile(doc: &Value) -> ObsSettings {
        let o = At(doc.get("observability"));
        let logs = o.get("request_logs");
        let otel_resource = resource(&o.get("traces"));
        let http = logs
            .get("sinks.http")
            .0
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(name, v)| http_sink(name, v))
            .chain(
                logs.get("sinks.langfuse")
                    .0
                    .and_then(Value::as_object)
                    .into_iter()
                    .flatten()
                    .filter_map(|(name, v)| ingest_sink(name, v)),
            )
            .chain(
                logs.get("sinks.otlp")
                    .0
                    .and_then(Value::as_object)
                    .into_iter()
                    .flatten()
                    .filter_map(|(name, v)| otlp_sink(name, v, &otel_resource)),
            )
            .collect();
        let request_logs = RequestLogSettings {
            enabled: logs.bool("enabled", true),
            include_unauthenticated: logs.bool("include_unauthenticated", false),
            max_body_bytes: logs.size("content.max_body_bytes", 1 << 20),
            log_upstream_error_bodies: logs.bool("content.log_upstream_error_bodies", false),
            upstream_error_body_max: logs.size("content.upstream_error_body_max", 16 << 10),
            request_headers: logs.names("headers.request"),
            response_headers: logs.names("headers.response"),
            client_ip: match logs.str("client_ip") {
                Some("truncated") => ClientIpMode::Truncated,
                Some("none") => ClientIpMode::None,
                _ => ClientIpMode::Full,
            },
            user_agent: logs.bool("user_agent", true),
            store: logs.bool("sinks.store.enabled", true),
            stdout: logs
                .bool("sinks.stdout.enabled", false)
                .then(|| logs.bool("sinks.stdout.include_content", false)),
            http,
            shutdown_flush_timeout: logs.duration("shutdown_flush_timeout", Duration::from_secs(10)),
            retention: logs.duration("sinks.store.retention", Duration::from_secs(30 * 86_400)),
            content_retention: logs
                .duration("sinks.store.content_retention", Duration::from_secs(7 * 86_400)),
            export_max_rows: logs
                .get("export_max_rows")
                .0
                .and_then(Value::as_u64)
                .unwrap_or(100_000),
        };
        let m = o.get("metrics");
        let buckets: BTreeMap<String, Vec<f64>> = m
            .get("buckets")
            .0
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .map(|(k, v)| {
                let list = v
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_f64)
                    .collect();
                (k.clone(), list)
            })
            .collect();
        let metrics = MetricsSettings {
            enabled: m.bool("enabled", true),
            path: m.str("path").unwrap_or("/metrics").to_owned(),
            auth_required: m.str("auth") != Some("none"),
            config: MetricsConfig {
                max_label_value_bytes: m.usize("max_label_value_bytes", 128),
                max_series_per_family: m.usize("max_series_per_family", 10_000),
                buckets,
                exclude: m.names("exclude_families"),
                tenant_labels: m.names("tenant_labels"),
                custom_labels: custom_labels(m.get("custom_labels").0),
            },
        };
        let t = o.get("traces");
        let traces = TraceSettings {
            enabled: t.bool("enabled", false),
            service_name: t.str("service_name").unwrap_or("antirion").to_owned(),
            resource_attributes: t.pairs("resource_attributes"),
            protocol: match t.str("exporter.protocol") {
                Some("http/protobuf") => TraceProtocol::HttpProtobuf,
                Some("http/json") => TraceProtocol::HttpJson,
                _ => TraceProtocol::Grpc,
            },
            endpoint: t.str("exporter.endpoint").map(str::to_owned),
            headers: t.pairs("exporter.headers"),
            timeout: t.duration("exporter.timeout", Duration::from_secs(10)),
            gzip: t.str("exporter.compression") != Some("none"),
            sampler: sampler(&t),
            propagate_to_upstream: t.bool("propagate_to_upstream", true),
            header_attributes: t
                .pairs("header_attributes")
                .into_iter()
                .map(|(h, a)| (h.to_ascii_lowercase(), a))
                .collect(),
            include_end_user: t.bool("include_end_user", false),
            max_queue_size: t.usize("batch.max_queue_size", 2048).max(1),
            max_export_batch_size: t.usize("batch.max_export_batch_size", 512).max(1),
            schedule_delay: t.duration("batch.schedule_delay", Duration::from_secs(1)),
        };
        ObsSettings {
            request_logs,
            metrics,
            traces,
            metadata: MetadataLimits {
                max_entries: o.usize("metadata.max_entries", 20),
                max_value_bytes: o.usize("metadata.max_value_bytes", 256),
            },
            upstream_request_id_header: o.bool("response_headers.upstream_request_id", true),
            version_header: o.bool("response_headers.version", false),
            feedback: crate::feedback::FeedbackSettings::compile(o.get("feedback").0),
            environment_tag: o.str("environment_tag").unwrap_or("env").to_owned(),
            geoip_database: o.str("geoip.database").map(str::to_owned),
        }
    }
}

/// `observability.metrics.custom_labels`, by name.
fn custom_labels(v: Option<&Value>) -> Vec<std::sync::Arc<crate::metrics::CustomLabel>> {
    let strings = |v: Option<&Value>| -> Vec<String> {
        v.and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    };
    v.and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(name, def)| {
            Some(std::sync::Arc::new(crate::metrics::CustomLabel {
                name: name.clone(),
                from_metadata: def.get("from_metadata")?.as_str()?.to_owned(),
                allowed_values: strings(def.get("allowed_values")),
                other_value: def
                    .get("other_value")
                    .and_then(Value::as_str)
                    .unwrap_or("_other")
                    .to_owned(),
                families: strings(def.get("families")),
            }))
        })
        .collect()
}

impl Default for ObsSettings {
    fn default() -> ObsSettings {
        ObsSettings::compile(&Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn defaults() {
        let s = ObsSettings::default();
        assert!(s.request_logs.enabled && s.request_logs.store);
        assert_eq!(s.request_logs.max_body_bytes, 1 << 20);
        assert!(s.metrics.enabled && s.metrics.auth_required);
        assert_eq!(s.metrics.path, "/metrics");
        assert!(!s.traces.enabled);
        assert_eq!(s.traces.sampler, Sampler::ParentBasedRatio(1.0));
        assert_eq!(s.metadata.max_entries, 20);
    }

    #[test]
    fn reads_sinks_and_traces() {
        let s = ObsSettings::compile(&json!({"observability": {
            "request_logs": {"headers": {"request": ["X-App"]}, "sinks": {"http": {"w": {
                "url": "http://h/ingest", "format": "json_array", "compression": "none",
                "flush_interval": "10ms", "batch_max_records": 2}}}},
            "metrics": {"auth": "none", "buckets": {"antirion_overhead_seconds": [0.001, 0.01]}},
            "traces": {"enabled": true, "exporter": {"protocol": "http/json", "endpoint": "http://c"},
                       "sampler": {"type": "ratio", "ratio": 0.0}, "batch": {"schedule_delay": "50ms"}},
        }}));
        assert_eq!(s.request_logs.request_headers, ["x-app"]);
        let h = &s.request_logs.http[0];
        assert_eq!(
            (h.format, h.gzip, h.batch_max_records),
            (BatchFormat::JsonArray, false, 2)
        );
        assert_eq!(h.flush_interval, Duration::from_millis(10));
        assert!(!s.metrics.auth_required);
        assert_eq!(
            s.metrics.config.buckets["antirion_overhead_seconds"],
            [0.001, 0.01]
        );
        assert_eq!(s.traces.protocol, TraceProtocol::HttpJson);
        assert_eq!(s.traces.sampler, Sampler::Ratio(0.0));
        assert_eq!(s.traces.schedule_delay, Duration::from_millis(50));
    }
}
