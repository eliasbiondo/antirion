//! OTLP metric export: with `observability.metrics.otlp.enabled`, the registry's cumulative state is
//! pushed every `interval` over OTLP/HTTP (JSON or protobuf) or gRPC, in addition to the scrape
//! endpoint. A failed push is logged and the next one carries the same cumulative values.

use std::sync::Arc;
use std::time::Duration;

use antirion_core::Clock;
use antirion_obs::metrics::otlp::{Export, encode_json, encode_proto};

use crate::Runtime;
use crate::alerting::config::OtlpMetrics;

const GRPC_PATH: &str = "/opentelemetry.proto.collector.metrics.v1.MetricsService/Export";
const TIMEOUT: Duration = Duration::from_secs(10);
const IDLE: Duration = Duration::from_secs(1);

fn unix_nanos() -> u64 {
    u64::try_from(antirion_core::SystemClock.now().0)
        .unwrap_or(0)
        .saturating_mul(1000)
}

async fn push(rt: &Runtime, s: &OtlpMetrics, start: u64) {
    let (url, content_type, body) = {
        let snap = rt.gens.load();
        let provider_type = |name: &str| {
            snap.providers
                .providers
                .get(name)
                .map(|p| p.kind.as_str().to_owned())
        };
        let service = snap.obs.traces.service_name.clone();
        let resource = vec![
            ("service.name".to_owned(), service),
            ("service.version".to_owned(), rt.node.version.clone()),
            ("service.instance.id".to_owned(), rt.node.id.clone()),
        ];
        let x = Export {
            resource: &resource,
            version: &rt.node.version,
            start_unix_nano: start,
            now_unix_nano: unix_nanos(),
            provider_type: &provider_type,
        };
        let families = rt.metrics.families();
        let endpoint = s.endpoint.trim_end_matches('/');
        let (url, content_type, body) = match s.protocol.as_str() {
            "http/json" => (
                format!("{endpoint}/v1/metrics"),
                "application/json",
                encode_json(&families, &x),
            ),
            "grpc" => {
                let message = encode_proto(&families, &x);
                let mut framed = Vec::with_capacity(message.len() + 5);
                framed.push(0);
                framed.extend_from_slice(&u32::try_from(message.len()).unwrap_or(u32::MAX).to_be_bytes());
                framed.extend_from_slice(&message);
                (format!("{endpoint}{GRPC_PATH}"), "application/grpc", framed)
            }
            _ => (
                format!("{endpoint}/v1/metrics"),
                "application/x-protobuf",
                encode_proto(&families, &x),
            ),
        };
        (url, content_type, body)
    };
    let env = crate::alerting::deliver_env(rt);
    if crate::telemetry::deliver::post(&env, &url, &s.headers, content_type, body, false, TIMEOUT)
        .await
        .is_err()
    {
        tracing::debug!("an OTLP metric export failed");
    }
}

/// Pushes metrics while OTLP export is enabled, until the node stops.
pub(crate) async fn run(rt: Arc<Runtime>) {
    let start = unix_nanos();
    let mut last: Option<tokio::time::Instant> = None;
    loop {
        let settings = rt.alerting().otlp_metrics.clone();
        let wait = match (&settings, last) {
            (Some(s), Some(t)) => s.interval.saturating_sub(t.elapsed()).min(IDLE),
            (Some(_), None) => Duration::ZERO,
            (None, _) => IDLE,
        };
        tokio::select! {
            () = rt.shutdown.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
        }
        let Some(s) = settings else {
            last = None;
            continue;
        };
        if last.is_some_and(|t| t.elapsed() < s.interval) {
            continue;
        }
        last = Some(tokio::time::Instant::now());
        push(&rt, &s, start).await;
    }
}
