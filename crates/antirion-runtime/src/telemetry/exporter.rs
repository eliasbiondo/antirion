//! The span exporter: batches of at most `max_export_batch_size` spans, sent `schedule_delay` after
//! their first span over OTLP/HTTP (JSON or protobuf) or gRPC. A failed export drops its spans and
//! counts them; it never retries into the next batch's way.

use std::time::Duration;

use antirion_obs::Fam;
use antirion_obs::settings::{TraceProtocol, TraceSettings};
use antirion_obs::trace::otlp::{encode_json, encode_proto};
use antirion_obs::trace::{SpanData, Value};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::Env;
use super::deliver::post;

const GRPC_PATH: &str = "/opentelemetry.proto.collector.trace.v1.TraceService/Export";

pub(crate) async fn run(
    s: TraceSettings,
    mut rx: mpsc::Receiver<SpanData>,
    env: Env,
    stop: CancellationToken,
    shutdown: CancellationToken,
    flush: Duration,
) {
    let mut resource: Vec<(String, Value)> = vec![
        ("service.name".to_owned(), Value::Str(s.service_name.clone())),
        ("service.version".to_owned(), Value::Str(env.version.clone())),
    ];
    for (k, v) in &s.resource_attributes {
        resource.retain(|(name, _)| name != k);
        resource.push((k.clone(), Value::Str(v.clone())));
    }
    let mut draining: Option<Instant> = None;
    loop {
        let first = tokio::select! {
            biased;
            () = stop.cancelled() => return,
            () = shutdown.cancelled(), if draining.is_none() => {
                if rx.is_empty() {
                    return;
                }
                draining = Some(Instant::now() + flush);
                continue;
            }
            r = rx.recv() => match r {
                Some(r) => r,
                None => return,
            },
        };
        let mut batch = vec![first];
        let ready_at = Instant::now()
            + if draining.is_some() {
                Duration::ZERO
            } else {
                s.schedule_delay
            };
        while batch.len() < s.max_export_batch_size {
            tokio::select! {
                biased;
                () = stop.cancelled() => return,
                () = tokio::time::sleep_until(ready_at) => break,
                r = rx.recv() => match r {
                    Some(r) => batch.push(r),
                    None => break,
                },
            }
        }
        let timeout = match draining {
            Some(d) => s.timeout.min(d.saturating_duration_since(Instant::now())),
            None => s.timeout,
        };
        let sent = tokio::select! {
            () = stop.cancelled() => return,
            r = export(&s, &resource, &batch, &env, timeout) => r,
        };
        if sent.is_err() {
            env.metrics
                .add(Fam::SpansDropped, &["export_failed"], batch.len() as f64);
        }
        if draining.is_some_and(|d| rx.is_empty() || Instant::now() >= d) {
            return;
        }
    }
}

async fn export(
    s: &TraceSettings,
    resource: &[(String, Value)],
    spans: &[SpanData],
    env: &Env,
    timeout: Duration,
) -> Result<(), super::deliver::Failed> {
    let endpoint = s.endpoint.as_deref().unwrap_or_default().trim_end_matches('/');
    let (url, content_type, body) = match s.protocol {
        TraceProtocol::HttpJson => (
            format!("{endpoint}/v1/traces"),
            "application/json",
            encode_json(resource, &env.version, spans),
        ),
        TraceProtocol::HttpProtobuf => (
            format!("{endpoint}/v1/traces"),
            "application/x-protobuf",
            encode_proto(resource, &env.version, spans),
        ),
        TraceProtocol::Grpc => {
            let message = encode_proto(resource, &env.version, spans);
            let mut framed = Vec::with_capacity(message.len() + 5);
            framed.push(0);
            framed.extend_from_slice(&u32::try_from(message.len()).unwrap_or(u32::MAX).to_be_bytes());
            framed.extend_from_slice(&message);
            (format!("{endpoint}{GRPC_PATH}"), "application/grpc", framed)
        }
    };
    let gzip = s.gzip && s.protocol != TraceProtocol::Grpc;
    post(env, &url, &s.headers, content_type, body, gzip, timeout).await
}
