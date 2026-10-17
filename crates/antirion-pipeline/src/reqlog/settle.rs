//! Settling a request: the headers that depend on its record, then (once the head is built and any
//! committed stream has ended) the timing, the cost, the record itself, the metrics and the trace.

use std::sync::Arc;
use std::time::Instant;

use antirion_core::Stage;
use antirion_core::headers as h;
use antirion_limits::cost::CostComponents;
use antirion_limits::money::Nusd;
use antirion_obs::sink::LogRecord;
use antirion_obs::timing::{Intervals, StreamInstants, Timing};
use antirion_obs::{Fam, Metrics};
use antirion_runtime::Runtime;
use http::{HeaderMap, HeaderName, HeaderValue, Response};
use serde_json::{Map, Value, json};

use super::{AttemptFact, Draft, HeadFacts, Record, emit, json};
use crate::controls::{ATTEMPT_TRAIL, ROUTE_DEBUG};
use crate::ctx::{Body, Ctx};

/// A request counted in `antirion_requests_in_flight` until dropped, whatever ends it.
#[derive(Debug)]
pub(crate) struct InFlight {
    metrics: Metrics,
    operation: &'static str,
}

impl InFlight {
    pub(crate) fn new(metrics: &Metrics, operation: &'static str) -> InFlight {
        metrics.add(Fam::InFlight, &[operation], 1.0);
        InFlight {
            metrics: metrics.clone(),
            operation,
        }
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.metrics.add(Fam::InFlight, &[self.operation], -1.0);
    }
}

/// The cost of a request, as its record and metrics report it.
#[derive(Clone, Debug)]
pub(crate) struct CostFact {
    pub(crate) status: &'static str,
    /// `None` when unpriced.
    pub(crate) total: Option<Nusd>,
    pub(crate) components: CostComponents,
    /// What a cache hit saved.
    pub(crate) saved: Option<Nusd>,
}

fn trail(attempts: &[AttemptFact]) -> String {
    attempts
        .iter()
        .map(|a| {
            let mut item = format!("\"{}\"", sf_escape(&a.deployment));
            if let Some(k) = &a.key {
                item.push_str(&format!(";key=\"{}\"", sf_escape(k)));
            }
            item.push_str(&format!(";class={}", a.class));
            if let Some(s) = a.status {
                item.push_str(&format!(";status={s}"));
            }
            if let Some(t) = a.timeout {
                item.push_str(&format!(";timeout={t}"));
            }
            let ms = a.end.saturating_duration_since(a.start).as_millis();
            item.push_str(&format!(";wait={};ms={ms}", a.wait_ms));
            if let Some(v) = &a.via {
                item.push_str(&format!(";via=\"{}\"", sf_escape(v)));
            }
            item
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn sf_escape(s: &str) -> String {
    s.chars()
        .filter(|c| (' '..='~').contains(c))
        .flat_map(|c| match c {
            '"' | '\\' => vec!['\\', c],
            _ => vec![c],
        })
        .collect()
}

fn ms(from: Instant, to: Instant) -> f64 {
    to.saturating_duration_since(from).as_secs_f64() * 1000.0
}

/// The intervals the overhead formula reads, relative to the request's start.
pub(crate) fn intervals(rec: &Record, t_end: Instant) -> Intervals {
    let off = |i: Instant| ms(rec.started, i);
    let streamed = rec.stream.as_ref().is_some_and(|s| s.committed);
    let serving = rec.serving_attempt().filter(|a| !a.failed());
    Intervals {
        total_ms: off(t_end),
        upstream: rec.attempts.iter().map(|a| (off(a.start), off(a.end))).collect(),
        waits: rec.waits.iter().map(|(a, b)| (off(*a), off(*b))).collect(),
        external: Vec::new(),
        upstream_ttft_ms: serving
            .filter(|_| streamed)
            .and_then(|a| a.ttft)
            .map(|d| d.as_secs_f64() * 1000.0),
        stream: streamed.then(|| {
            let st = &rec.stream_times;
            StreamInstants {
                first_byte_sent_ms: st.first_byte_sent.map(off),
                first_upstream_content_ms: st.first_upstream_content.map(off),
                relay_ms: st.relay.as_secs_f64() * 1000.0,
                first_content_sent_ms: st.first_content_sent.map(off),
                last_content_sent_ms: st.last_content_sent.map(off),
                output_tokens: rec.usage.output.unwrap_or(0),
            }
        }),
    }
}

fn header_value(map: &HeaderMap, name: &str) -> Option<String> {
    let values: Vec<String> = map
        .get_all(name)
        .iter()
        .map(|v| antirion_obs::record::header_text(v.as_bytes()))
        .collect();
    (!values.is_empty()).then(|| values.join(", "))
}

fn set(headers: &mut HeaderMap, name: &'static str, value: &str) {
    if let Ok(v) = HeaderValue::from_str(value) {
        headers.insert(HeaderName::from_static(name), v);
    }
}

/// The response path of a response, from its header or its status.
fn response_path(headers: &HeaderMap, status: u16, failed: bool) -> String {
    headers
        .get(h::RESPONSE_PATH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            if failed || status >= 400 {
                "error".to_owned()
            } else {
                "local".to_owned()
            }
        })
}

/// Adds the headers that depend on the record (or, without one, the overhead), and settles the
/// request unless a committed stream still runs.
pub(crate) fn finish(cx: &Ctx, mut resp: Response<Body>) -> Response<Body> {
    let t_end = Instant::now();
    let status = resp.status().as_u16();
    let streaming = cx.log.is_streaming();
    let headers = resp.headers_mut();
    let Some((logged, failed)) = cx
        .log
        .read(|record| record_headers(record, headers, streaming, t_end))
    else {
        let overhead = antirion_obs::timing::ms3(ms(cx.started, t_end));
        set(resp.headers_mut(), h::OVERHEAD_MS, &format!("{overhead:.3}"));
        let path = response_path(resp.headers(), status, true);
        emit::unlogged(cx, status, &path, cx.log.error_code(), overhead);
        cx.log.end_in_flight();
        return resp;
    };
    let headers = resp.headers_mut();
    let degraded = logged
        .then(|| {
            cx.rt
                .state
                .telemetry
                .degraded()
                .or_else(|| cx.rt.state.request_logs.degraded())
        })
        .flatten();
    if degraded.is_some() {
        let value = match headers.get(h::DEGRADED).and_then(|v| v.to_str().ok()) {
            Some(existing) if !existing.is_empty() => format!("{existing}, observability"),
            _ => "observability".to_owned(),
        };
        set(headers, h::DEGRADED, &value);
    }
    let path = response_path(headers, status, failed);
    let wanted = &cx.snap.obs.request_logs.response_headers;
    let response_headers: Map<String, Value> = wanted
        .iter()
        .filter_map(|n| header_value(headers, n).map(|v| (n.clone(), Value::String(v))))
        .collect();
    cx.log.update(|r| {
        if let Some(cause) = degraded {
            r.degraded.push(("observability", Some(cause)));
        }
        r.response_headers = response_headers;
    });
    if let Some((rec, head)) = cx.log.head_built(HeadFacts {
        status,
        response_path: path,
    }) {
        settle(&cx.rt, rec, head, t_end);
        cx.log.end_in_flight();
    }
    resp
}

/// The headers `record` decides; returns whether it is logged and whether it failed.
fn record_headers(record: &Record, headers: &mut HeaderMap, streaming: bool, t_end: Instant) -> (bool, bool) {
    if record.logged {
        set(headers, h::LOG_ID, &record.id);
    }
    if record.attempt_trail {
        set(headers, ATTEMPT_TRAIL, &trail(&record.attempts));
    }
    if record.route_debug {
        let classifier = record.classes.first().map_or(
            Value::Null,
            |(node, class)| json!({"node": node, "class": class, "score": record.class_score}),
        );
        let causes: Vec<&str> = record
            .degraded
            .iter()
            .filter(|(c, _)| *c == "routing" || *c == "affinity")
            .filter_map(|(_, cause)| *cause)
            .collect();
        let debug = json!({"plan": record.plan, "classifier": classifier,
                           "cost_quality": record.cost_quality, "degraded": causes});
        set(headers, ROUTE_DEBUG, &debug.to_string());
    }
    routing_headers(headers, record);
    if !streaming {
        let timing = intervals(record, t_end).timing();
        set(headers, h::OVERHEAD_MS, &format!("{:.3}", timing.overhead_ms));
    }
    (record.logged, record.error.is_some())
}

/// The routing headers of a request that reached stage `route`.
fn routing_headers(headers: &mut HeaderMap, record: &Record) {
    if let Some(route) = &record.route {
        set(headers, h::ROUTE, route);
    }
    if record.show_route_path
        && let Some(path) = &record.route_path
    {
        set(headers, h::ROUTE_PATH, path);
    }
    if let Some(a) = record.affinity {
        set(headers, crate::controls::AFFINITY, a);
    }
    if let Some((name, arm, true)) = &record.experiment {
        set(headers, h::EXPERIMENT, &format!("{name}={arm}"));
    }
    if record.routed {
        if !headers.contains_key(h::ATTEMPTS) {
            set(headers, h::ATTEMPTS, &record.attempts.len().to_string());
        }
        if !headers.contains_key(h::DEPLOYMENT)
            && !record.conceal_identity
            && let Some(last) = record.serving_attempt()
        {
            set(headers, h::DEPLOYMENT, &last.deployment);
        }
    }
}

/// The overhead of a committed stream at its end, as its summary trailer reports it.
pub(crate) fn stream_overhead(draft: &Draft) -> Option<f64> {
    let now = Instant::now();
    draft.read(|r| antirion_obs::timing::ms3(intervals(r, now).timing().overhead_ms))
}

/// Called by the relay when a committed stream ends.
pub(crate) fn settle_stream(rt: &Runtime, draft: &Draft) {
    if let Some((rec, head)) = draft.stream_ended() {
        settle(rt, rec, head, Instant::now());
        draft.end_in_flight();
    }
}

/// Called when a lost hedge attempt's tail ends: the last one settles the record once its head
/// is built (and any stream ended).
pub(crate) fn tail_ended(rt: &Runtime, draft: &Draft) {
    if let Some((rec, head, at)) = draft.tail_ended() {
        let streamed = draft.is_streaming();
        let t_end = at.filter(|_| !streamed).unwrap_or_else(Instant::now);
        settle(rt, rec, head, t_end);
        draft.end_in_flight();
    }
}

/// The settled cost of a request; free when it was never settled (it reached no upstream).
fn cost_of(rec: &Record) -> CostFact {
    rec.cost.clone().unwrap_or(CostFact {
        status: "exact",
        total: Some(Nusd::ZERO),
        components: CostComponents::default(),
        saved: None,
    })
}

/// The time of each lifecycle stage that ran, upstream time and waits excluded, adjusted so the
/// stages sum to the overhead: what the marks cannot place (a stream's relay) goes to `respond`.
pub(crate) fn stage_ms(rec: &Record, iv: &Intervals, overhead_ms: f64) -> Map<String, Value> {
    let off = |i: Instant| ms(rec.started, i);
    let mut marks: Vec<(&'static str, f64)> = vec![(Stage::Accept.as_str(), 0.0)];
    if let Some((auth, _)) = rec.auth_span {
        marks.push((Stage::Authenticate.as_str(), off(auth)));
    }
    marks.extend(rec.stage_marks.iter().map(|(s, at)| (s.as_str(), off(*at))));
    marks.sort_by(|a, b| a.1.total_cmp(&b.1));
    let mut excluded = iv.upstream.clone();
    excluded.extend(iv.waits.iter().copied());
    let mut stages: Vec<(&'static str, f64)> = Vec::new();
    for (i, (name, from)) in marks.iter().enumerate() {
        let to = marks.get(i + 1).map_or(iv.total_ms, |m| m.1).min(iv.total_ms);
        if to <= *from {
            continue;
        }
        let own = antirion_obs::timing::minus(&[(*from, to)], &excluded);
        match stages.iter_mut().find(|(n, _)| n == name) {
            Some((_, d)) => *d += own,
            None => stages.push((name, own)),
        }
    }
    let sum: f64 = stages.iter().map(|(_, d)| d).sum();
    let gap = overhead_ms - sum;
    let respond = Stage::Respond.as_str();
    match stages.iter_mut().find(|(n, _)| *n == respond) {
        Some((_, d)) if *d + gap >= 0.0 => *d += gap,
        None if gap >= 0.0 => stages.push((respond, gap)),
        _ if sum > 0.0 => {
            let scale = (overhead_ms / sum).max(0.0);
            for (_, d) in &mut stages {
                *d *= scale;
            }
        }
        _ => {}
    }
    stages
        .into_iter()
        .map(|(n, d)| (n.to_owned(), json!(antirion_obs::timing::ms3(d))))
        .collect()
}

/// Writes the record and observes the request.
fn settle(rt: &Runtime, rec: Record, head: HeadFacts, t_end: Instant) {
    let iv = intervals(&rec, t_end);
    let timing: Timing = iv.timing();
    let stages = stage_ms(&rec, &iv, timing.overhead_ms);
    let cost = cost_of(&rec);
    emit::metrics(&rt.metrics, &rec, &head, &timing, &cost);
    if rec.trace.sampled {
        let spans = emit::spans(&rec, &head, &timing, &cost, t_end);
        rt.state.telemetry.export(spans, &rt.metrics);
    }
    if !rec.comparisons.is_empty() {
        let (finish, text) = rec.answer.clone().unwrap_or_default();
        #[allow(clippy::cast_precision_loss)]
        let served = crate::compare::Side {
            finish: finish.or_else(|| {
                rec.finish_reasons
                    .first()
                    .map(|f| crate::compare::normalised_finish(f))
            }),
            text,
            latency_ms: timing.total_ms,
            cost_usd: cost.total.map_or(0.0, |n| n.0 as f64 / 1e9),
        };
        for pair in &rec.comparisons {
            pair.served(served.clone());
        }
    }
    if !rec.logged {
        return;
    }
    let snap = Arc::clone(&rec.snap);
    let mut rec = rec;
    rec.content.scrub(snap.guardrails.hash_key.as_deref());
    let marks = std::mem::take(&mut rec.safety);
    let rec_ip = rec.client_ip.clone();
    let settled = json::Settled {
        rec,
        head,
        timing,
        cost,
        ended: rt.now(),
        node_id: rt.node.id.clone(),
        gateway_version: rt.node.version.clone(),
        stage_ms: stages,
        geo: snap
            .geoip
            .as_ref()
            .zip(rec_ip.as_deref().and_then(|ip| ip.parse().ok()))
            .and_then(|(db, ip)| db.locate(ip)),
    };
    let guarded = !settled.rec.guardrails.is_empty();
    let value = (snap.obs.request_logs.store || guarded).then(|| Arc::new(settled.to_value()));
    if guarded && let Some(v) = &value {
        rt.keep_safety(v, &marks, None);
    }
    if snap.obs.request_logs.store
        && let Some(value) = value
    {
        let extras = antirion_runtime::reqlog::Extras {
            user: settled.rec.owner.clone(),
            route_node: settled.rec.route_path.clone(),
        };
        rt.keep_log(value, extras);
        rt.metrics.add(Fam::LogRecords, &["store", "exported"], 1.0);
    }
    let record: Arc<dyn LogRecord> = Arc::new(settled);
    for (sink, dropped) in rt.state.telemetry.publish(&snap, &record, &rt.metrics) {
        rt.send_alert(
            &snap,
            None,
            "log_sink.dropping",
            serde_json::json!({"sink": sink, "dropped": dropped}),
        );
    }
}
