//! The worker of one HTTP sink: batches of at most `batch_max_records`, sent at most `flush_interval`
//! after their first record, retried with backoff until `max_retry_duration`, then dropped and counted.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use antirion_core::SplitMix64;
use antirion_obs::Fam;
use antirion_obs::otlp_logs;
use antirion_obs::settings::{BatchFormat, HttpSinkSettings, TraceProtocol};
use antirion_obs::sink::{LogRecord, backoff, encode_records};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::deliver::post;
use super::{Env, SinkStatus};

/// When the worker stops taking new work: never, or at the end of the shutdown flush.
fn far() -> Instant {
    Instant::now() + Duration::from_secs(86_400 * 365)
}

pub(crate) async fn run(
    s: HttpSinkSettings,
    mut rx: mpsc::Receiver<Arc<dyn LogRecord>>,
    status: Arc<SinkStatus>,
    env: Env,
    stop: CancellationToken,
    shutdown: CancellationToken,
    flush: Duration,
) {
    let mut rng = SplitMix64::from_entropy();
    let mut deadline = far();
    let mut draining = false;
    loop {
        let first = tokio::select! {
            biased;
            () = stop.cancelled() => return,
            () = shutdown.cancelled(), if !draining => {
                if rx.is_empty() {
                    return;
                }
                draining = true;
                deadline = Instant::now() + flush;
                continue;
            }
            () = tokio::time::sleep_until(deadline) => break,
            r = rx.recv() => match r {
                Some(r) => r,
                None => return,
            },
        };
        let mut batch = vec![first];
        let ready_at = if draining {
            Instant::now()
        } else {
            Instant::now() + s.flush_interval
        };
        while batch.len() < s.batch_max_records {
            if draining {
                match rx.try_recv() {
                    Ok(r) => batch.push(r),
                    Err(_) => break,
                }
                continue;
            }
            match rx.try_recv() {
                Ok(r) => {
                    batch.push(r);
                    continue;
                }
                Err(mpsc::error::TryRecvError::Disconnected) => break,
                Err(mpsc::error::TryRecvError::Empty) => {}
            }
            tokio::select! {
                biased;
                () = stop.cancelled() => return,
                () = tokio::time::sleep_until(ready_at) => {
                    while batch.len() < s.batch_max_records {
                        match rx.try_recv() {
                            Ok(r) => batch.push(r),
                            Err(_) => break,
                        }
                    }
                    break;
                }
                () = status.batch_ready.notified() => {}
            }
        }
        let until = if draining { deadline } else { far() };
        deliver(&s, &batch, &status, &env, &stop, &mut rng, until).await;
        if draining && rx.is_empty() {
            return;
        }
    }
    let left = rx.len();
    if left > 0 {
        env.metrics
            .add(Fam::LogRecords, &[&s.name, "dropped_queue_full"], left as f64);
    }
}

/// An OTLP logs export of `batch`: the resource gets `service.version` unless the settings name
/// one; gRPC bodies are one length-prefixed message.
fn otlp_body(
    s: &HttpSinkSettings,
    protocol: TraceProtocol,
    batch: &[Arc<dyn LogRecord>],
    env: &Env,
) -> Vec<u8> {
    let mut resource = s.resource.clone();
    if !resource.iter().any(|(k, _)| k == "service.version") {
        resource.push(("service.version".to_owned(), env.version.clone()));
    }
    let now = u64::try_from(env.clock.now().0).unwrap_or(0).saturating_mul(1000);
    match protocol {
        TraceProtocol::HttpJson => {
            otlp_logs::encode_json(&resource, &env.version, batch, s.include_content, now)
        }
        TraceProtocol::HttpProtobuf => {
            otlp_logs::encode_proto(&resource, &env.version, batch, s.include_content, now)
        }
        TraceProtocol::Grpc => {
            let message = otlp_logs::encode_proto(&resource, &env.version, batch, s.include_content, now);
            let mut framed = Vec::with_capacity(message.len() + 5);
            framed.push(0);
            framed.extend_from_slice(&u32::try_from(message.len()).unwrap_or(u32::MAX).to_be_bytes());
            framed.extend_from_slice(&message);
            framed
        }
    }
}

async fn deliver(
    s: &HttpSinkSettings,
    batch: &[Arc<dyn LogRecord>],
    status: &SinkStatus,
    env: &Env,
    stop: &CancellationToken,
    rng: &mut SplitMix64,
    until: Instant,
) {
    let body = match s.format {
        BatchFormat::TraceIngest => {
            antirion_obs::ingest::encode(batch, s.include_content, s.environment.as_deref())
        }
        BatchFormat::OtlpLogs(protocol) => otlp_body(s, protocol, batch, env),
        format => encode_records(format, batch, s.include_content),
    };
    let n = batch.len() as f64;
    let started = Instant::now();
    let mut tries = 0u32;
    loop {
        let sent = tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep_until(until) => Err(super::deliver::Failed),
            r = post(env, &s.url, &s.headers, s.format.content_type(), body.clone(), s.gzip, s.timeout) => r,
        };
        if sent.is_ok() {
            status.failing.store(false, Ordering::Relaxed);
            env.metrics.add(Fam::LogRecords, &[&s.name, "exported"], n);
            return;
        }
        status.failing.store(true, Ordering::Relaxed);
        env.metrics.add(Fam::LogRecords, &[&s.name, "failed"], n);
        tries += 1;
        let u = (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        let wait = backoff(tries, u);
        let now = Instant::now();
        if now + wait > started + s.max_retry_duration || now + wait > until {
            env.metrics
                .add(Fam::LogRecords, &[&s.name, "dropped_retry_exhausted"], n);
            return;
        }
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
        }
    }
}
