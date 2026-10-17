//! Streamed responses: events are held until the commit point, then relayed to the client through a
//! bounded queue by a tracked task that owns the upstream body, so a disconnect cancels the upstream
//! while the request is still accounted.

mod array;
pub(crate) mod assemble;
mod body;
pub(crate) mod chunks;
mod costed;
mod encode;
pub(crate) mod events;
mod forward;
mod guard;
mod invoke;
pub(crate) mod native;
mod pump;
mod render;
mod shape;
pub(crate) mod synth;
mod writer;

use std::sync::Arc;
use std::time::Duration;

use antirion_core::RequestId;
use antirion_error::{Code, GatewayError};
use antirion_resilience::{Class, TimerKind};
use antirion_router::InFlight;
use antirion_runtime::Runtime;
use bytes::Bytes;
use http_body_util::BodyExt;
use serde_json::Value;
use tokio::sync::{Semaphore, mpsc};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use self::assemble::Assembler;
pub(crate) use self::encode::ClientDialect;
pub(crate) use self::events::StreamDialect;
pub(crate) use self::events::StreamFacts;
use self::events::{Kind, classify};
use self::forward::relay;
pub(crate) use self::guard::OutGuard;
pub(crate) use self::pump::{End, Framing, Pulled, Pump};
pub(crate) use self::render::synthesize;
pub(crate) use self::shape::Shaper;
use self::writer::Writer;
use crate::account::{End as Ended, StreamEnd};
use crate::ctx::Body;
use crate::policy::StreamPolicy;
use crate::reasons;
use crate::reqlog::{Draft, StreamFact, StreamTimes, Usage};

/// The trailer that names how a committed stream ended.
const STREAM_END: &str = "x-antirion-stream-end";

/// How a stream is relayed and timed.
#[derive(Clone, Debug)]
pub(crate) struct Setup {
    /// The stream is assembled for content logging.
    pub(crate) assemble: bool,
    /// The upstream's event dialect.
    pub(crate) dialect: StreamDialect,
    /// The dialect the client reads.
    pub(crate) client: ClientDialect,
    pub(crate) client_wants_usage: bool,
    /// The model name chunks must carry, when it differs from the upstream's.
    pub(crate) rewrite_model: Option<String>,
    pub(crate) policy: StreamPolicy,
    pub(crate) heartbeat: bool,
    pub(crate) idle: Option<Duration>,
    pub(crate) total: Option<Duration>,
    /// When the first committable event must have arrived, and which timer that is.
    pub(crate) ttft_at: Option<(Instant, TimerKind)>,
    /// The request deadline, which bounds the stream until commit.
    pub(crate) deadline_at: Option<Instant>,
    pub(crate) input_estimate: u64,
    pub(crate) provider: String,
    pub(crate) attempts: u16,
    pub(crate) request_id: RequestId,
    /// What upstream text is scrubbed of.
    pub(crate) scrub: crate::scrub::Scrub,
    /// A translated Responses stream to store when it completes, with its id.
    pub(crate) store: Option<Box<(crate::respstate::Turn, String)>>,
    /// The project and settings of the reasoning replay store, for a translated stream.
    pub(crate) replay: Option<(String, antirion_runtime::replay::ReplaySettings)>,
    /// Holds the attempt in flight on its deployment until the stream ends.
    pub(crate) inflight: Option<Arc<InFlight>>,
    /// The request's accounting, settled before the stream's terminator is released.
    pub(crate) account: crate::account::Slot,
    /// The client accepts trailers: the cost follows the stream as one.
    pub(crate) trailer: bool,
    /// The output guardrails' verdicts follow the stream as a trailer too.
    pub(crate) guard_trailer: bool,
    /// The copy of the stream for the response cache, when the response may be stored.
    pub(crate) tee: Option<Box<crate::cache::Tee>>,
    /// An InvokeModel client of an Anthropic Messages upstream: the events go out as AWS
    /// event-stream messages.
    pub(crate) invoke_frames: bool,
    /// A refusal before the commit fails the attempt.
    pub(crate) refusal_fails: bool,
    /// The client's read timeout shortened `idle`: its expiry is not the upstream's failure.
    pub(crate) idle_shortened: bool,
    /// The stream's health verdict after its commit.
    pub(crate) post_commit: crate::postcommit::PostCommit,
    /// A Gemini client that asked for a JSON array instead of server-sent events.
    pub(crate) json_array: bool,
    /// The upstream frames its events in a way the client does not read (NDJSON, a JSON array):
    /// forwarded events are written again in the client's framing.
    pub(crate) reframe: bool,
    /// `pricing.report_in_body`: the events carrying usage are held until the stream is settled
    /// and sent with its cost.
    pub(crate) report_cost: bool,
    /// The request's `stream_chunk` hooks, when an attached extension has one.
    pub(crate) chunks: Option<Arc<chunks::ChunkHooks>>,
}

/// A failure before commit: an attempt failure for the retry loop.
#[derive(Debug)]
pub(crate) struct Failed {
    pub(crate) code: Code,
    pub(crate) reason: Option<antirion_error::Reason>,
    pub(crate) status: u16,
    pub(crate) class: &'static str,
    pub(crate) message: Option<String>,
    /// The timer that ended the attempt.
    pub(crate) timer: Option<TimerKind>,
    /// The upstream's own error type, from an error event.
    pub(crate) kind: Option<String>,
}

fn bad(reason: antirion_error::Reason, class: &'static str) -> Failed {
    Failed {
        code: Code::UpstreamBadResponse,
        reason: Some(reason),
        status: 502,
        class,
        message: None,
        timer: None,
        kind: None,
    }
}

/// The outcome of the holding phase.
pub(crate) enum Held {
    Committed(Box<Committed>),
    Failed(Failed),
    /// An output guardrail stopped the stream before anything was sent; with the usage of the
    /// answer when the upstream had finished it, so the refused answer is charged.
    Guarded(Box<GatewayError>, Option<Box<Answered>>),
    /// The client went away.
    Cancelled,
}

/// The usage of a complete stream an output guardrail refused, completed by an estimate as a
/// committed stream's is, and the usage object the upstream reported.
pub(crate) struct Answered {
    pub(crate) usage: Usage,
    pub(crate) raw: Option<Value>,
}

/// A stream at its commit point.
pub(crate) struct Committed {
    pump: Pump,
    facts: StreamFacts,
    initial: Vec<Bytes>,
    transform: bool,
    shaper: Shaper,
    /// The initial events already end the stream (a synthesized stream with its own terminator).
    complete: bool,
    /// When the first upstream content was read.
    content_at: Option<Instant>,
    assembler: Option<Assembler>,
    /// The output guardrails still watching the stream.
    guard: Option<OutGuard>,
    /// The initial events went through the `stream_chunk` hooks already.
    chunked: bool,
}

impl Committed {
    /// A stream rebuilt from a complete response.
    pub(crate) fn synthesized(initial: Vec<Bytes>, facts: StreamFacts, complete: bool) -> Committed {
        Committed {
            pump: Pump::finished(),
            facts,
            initial,
            transform: true,
            shaper: Shaper::default(),
            complete,
            content_at: Some(Instant::now()),
            assembler: None,
            guard: None,
            chunked: false,
        }
    }

    /// The id of a native Responses stream, from its `response.created` event.
    pub(crate) fn response_id(&self) -> Option<&str> {
        self.facts.response_id.as_deref()
    }

    /// `x-antirion-stream-mode`.
    pub(crate) fn mode(&self) -> &'static str {
        if self.transform {
            "transform"
        } else {
            "passthrough"
        }
    }
}

/// The frames that end a stream at the upstream's terminator: the translator's last events, the
/// terminator itself and what the shaper adds.
fn terminator_frames(
    setup: &Setup,
    shaper: &mut Shaper,
    ev: &pump::UpEvent,
    c: &events::Classified,
    facts: &mut StreamFacts,
) -> Result<Vec<Bytes>, ()> {
    let mut out = Vec::new();
    if shaper.translator.is_some() {
        out.extend(shaper.event(setup, ev, c, facts).map_err(|_| ())?.0);
    }
    let passthrough = shaper.translator.is_none();
    let terminator = if passthrough && setup.dialect != StreamDialect::Chat {
        shaper.event(setup, ev, c, facts).map_err(|_| ())?.0.pop()
    } else if setup.reframe {
        None
    } else {
        Some(ev.raw.clone())
    };
    out.extend(shaper.finish(setup, facts, terminator));
    Ok(out)
}

/// Holds events until the commit point (or a failure), watching the client and the timers. An
/// output guard in window or buffer mode moves the commit point to its first release.
pub(crate) async fn hold(
    setup: &Setup,
    mut pump: Pump,
    mut shaper: Shaper,
    gone: &CancellationToken,
    mut guard: Option<OutGuard>,
    trail: &crate::guard::Trail,
) -> Held {
    let holding = guard.as_ref().is_some_and(|g| !g.passes());
    let mut facts = StreamFacts::default();
    let mut held: Vec<Bytes> = Vec::new();
    let mut held_events = 0usize;
    let mut held_bytes = 0usize;
    let mut transform = shaper.transforms(setup);
    let mut assembler = (setup.assemble && setup.dialect == StreamDialect::Chat).then(Assembler::default);
    let headers_at = Instant::now();
    let hold_until = headers_at + setup.policy.max_hold;
    let far = headers_at + Duration::from_secs(86_400 * 365);
    let (mut ttft_at, ttft_kind) = setup.ttft_at.unwrap_or((far, TimerKind::Ttft));
    let idle_fire = |from: Instant| -> (Instant, TimerKind) {
        let own = setup.idle.map_or(far, |t| from + t);
        match setup.deadline_at {
            Some(d) if d <= own => (d, TimerKind::Deadline),
            _ => (own, TimerKind::StreamIdle),
        }
    };
    let (mut idle_at, mut idle_kind) = idle_fire(Instant::now());
    let beat_at = guard
        .as_ref()
        .and_then(OutGuard::buffer_heartbeat)
        .map(|d| headers_at + d);
    let mut beat_now = false;
    loop {
        let pulled = tokio::select! {
            () = gone.cancelled() => return Held::Cancelled,
            () = tokio::time::sleep_until(hold_until), if held_events > 0 && !holding => None,
            () = tokio::time::sleep_until(beat_at.unwrap_or(far)), if beat_at.is_some() => {
                beat_now = true;
                None
            }
            () = tokio::time::sleep_until(ttft_at) => return Held::Failed(timeout_failure(ttft_kind)),
            () = tokio::time::sleep_until(idle_at) => return Held::Failed(timeout_failure(idle_kind)),
            p = pump.pull() => Some(p),
        };
        let Some(pulled) = pulled else {
            if beat_now {
                held.push(render::buffer_beat(setup.client));
            }
            return commit(pump, facts, held, transform, shaper, None, assembler, guard);
        };
        (idle_at, idle_kind) = idle_fire(Instant::now());
        let ev = match pulled {
            Pulled::Activity => continue,
            Pulled::End(End::Clean)
                if holding && held_events > 0 && facts.complete_at_clean_end(setup.dialect) =>
            {
                let tail = shaper.finish(setup, &facts, None);
                return Box::pin(guarded_end(
                    guard,
                    tail,
                    trail,
                    facts,
                    held,
                    shaper,
                    assembler,
                    setup.chunks.as_deref(),
                    setup.input_estimate,
                ))
                .await;
            }
            Pulled::End(End::Clean) => return Held::Failed(bad(reasons::EMPTY_STREAM, "upstream_reset")),
            Pulled::End(End::Abnormal) => {
                return Held::Failed(bad(reasons::UPSTREAM_TRUNCATED, "upstream_reset"));
            }
            Pulled::End(End::TooLarge) => {
                return Held::Failed(bad(reasons::UPSTREAM_EVENT_TOO_LARGE, "malformed_response"));
            }
            Pulled::End(End::ResponseTooLarge) => {
                return Held::Failed(bad(reasons::RESPONSE_TOO_LARGE, "malformed_response"));
            }
            Pulled::End(End::Malformed) => {
                return Held::Failed(bad(reasons::UPSTREAM_MALFORMED, "malformed_response"));
            }
            Pulled::Event(ev) => ev,
        };
        let c = classify(setup.dialect, &ev, setup.client_wants_usage, &mut facts);
        match c.kind {
            Kind::Empty => {}
            Kind::Terminator if holding && held_events > 0 => {
                drop(pump);
                let Ok(tail) = terminator_frames(setup, &mut shaper, &ev, &c, &mut facts) else {
                    return Held::Failed(bad(reasons::UPSTREAM_MALFORMED, "malformed_response"));
                };
                return Box::pin(guarded_end(
                    guard,
                    tail,
                    trail,
                    facts,
                    held,
                    shaper,
                    assembler,
                    setup.chunks.as_deref(),
                    setup.input_estimate,
                ))
                .await;
            }
            Kind::Terminator if setup.dialect == StreamDialect::Responses && shaper.translator.is_some() => {
                let Ok((out, _)) = shaper.event(setup, &ev, &c, &mut facts) else {
                    return Held::Failed(bad(reasons::UPSTREAM_MALFORMED, "malformed_response"));
                };
                held.extend(out);
                held.extend(shaper.finish(setup, &facts, None));
                drop(pump);
                let mut c = commit(
                    Pump::finished(),
                    facts,
                    held,
                    true,
                    shaper,
                    None,
                    assembler,
                    guard,
                );
                if let Held::Committed(done) = &mut c {
                    done.complete = true;
                }
                return c;
            }
            Kind::Terminator => return Held::Failed(bad(reasons::EMPTY_STREAM, "upstream_reset")),
            Kind::Malformed => return Held::Failed(bad(reasons::UPSTREAM_MALFORMED, "malformed_response")),
            Kind::Error {
                status,
                message,
                kind,
            } => {
                let mut f = error_failure(status, message);
                if kind.as_deref() == Some(native::MISSING_THOUGHT_SIGNATURE) {
                    f.class = Class::ReasoningReplayRejected.as_str();
                    f.code = Class::ReasoningReplayRejected.final_code();
                }
                f.kind = kind;
                return Held::Failed(f);
            }
            Kind::Content if setup.refusal_fails && c.json.as_ref().is_some_and(events::refusal) => {
                return Held::Failed(Failed {
                    code: Code::UpstreamRejected,
                    reason: Some(reasons::CONTENT_POLICY_VIOLATION),
                    status: 400,
                    class: "content_policy",
                    message: Some("The upstream refused the request.".to_owned()),
                    timer: None,
                    kind: None,
                });
            }
            Kind::Preamble | Kind::Content => {
                let is_content = matches!(c.kind, Kind::Content);
                if let (Some(a), Some(j)) = (assembler.as_mut(), c.json.as_ref()) {
                    a.push(j);
                }
                let Ok((out, rewritten)) = shaper.event(setup, &ev, &c, &mut facts) else {
                    return Held::Failed(bad(reasons::UPSTREAM_MALFORMED, "malformed_response"));
                };
                transform |= rewritten;
                held_events += 1;
                held_bytes += ev.raw.len();
                let out = match guard.as_mut() {
                    Some(g) => match g.push(out) {
                        Ok(released) => released,
                        Err(stop) => return Held::Guarded(Box::new(g.error(&stop)), None),
                    },
                    None => out,
                };
                let out = match &setup.chunks {
                    Some(c) => match c.apply(out).await {
                        Ok(frames) => frames,
                        Err(stopped) => return Held::Guarded(stopped.error, None),
                    },
                    None => out,
                };
                if holding {
                    if is_content {
                        ttft_at = far;
                    }
                    held.extend(out);
                    if guard.as_ref().is_some_and(OutGuard::committed) {
                        let content_at = Some(Instant::now());
                        return commit(pump, facts, held, true, shaper, content_at, assembler, guard);
                    }
                    continue;
                }
                held.extend(out);
                if is_content
                    || held_events > setup.policy.max_hold_events
                    || held_bytes > setup.policy.max_hold_bytes
                {
                    let content_at = c
                        .json
                        .as_ref()
                        .is_some_and(events::carries_output)
                        .then(Instant::now);
                    return commit(pump, facts, held, transform, shaper, content_at, assembler, guard);
                }
            }
        }
    }
}

/// The upstream ended while the guard held everything: the final check, then the whole stream
/// commits at once, or the stage's error.
#[allow(clippy::too_many_arguments)]
async fn guarded_end(
    guard: Option<OutGuard>,
    tail: Vec<Bytes>,
    trail: &crate::guard::Trail,
    facts: StreamFacts,
    mut held: Vec<Bytes>,
    shaper: Shaper,
    assembler: Option<Assembler>,
    chunks: Option<&chunks::ChunkHooks>,
    input_estimate: u64,
) -> Held {
    let Some(mut g) = guard else {
        let tail = match chunks {
            Some(c) => match c.apply(tail).await {
                Ok(frames) => frames,
                Err(stopped) => return Held::Guarded(stopped.error, None),
            },
            None => tail,
        };
        held.extend(tail);
        return commit(Pump::finished(), facts, held, true, shaper, None, assembler, None);
    };
    let buffered = matches!(g.mode(), crate::guard::OutMode::Buffer);
    match g.finish(tail, buffered.then_some(trail)).await {
        Ok(frames) => {
            let frames = match chunks {
                Some(c) => match c.apply(frames).await {
                    Ok(frames) => frames,
                    Err(stopped) => return Held::Guarded(stopped.error, None),
                },
                None => frames,
            };
            held.extend(frames);
            let content_at = Some(Instant::now());
            let mut h = commit(
                Pump::finished(),
                facts,
                held,
                true,
                shaper,
                content_at,
                assembler,
                None,
            );
            if let Held::Committed(done) = &mut h {
                done.complete = true;
            }
            h
        }
        Err(stop) => {
            let answered = Answered {
                usage: settled_usage(&facts, input_estimate),
                raw: facts.usage_raw.clone(),
            };
            Held::Guarded(Box::new(g.error(&stop)), Some(Box::new(answered)))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn commit(
    pump: Pump,
    facts: StreamFacts,
    initial: Vec<Bytes>,
    transform: bool,
    shaper: Shaper,
    content_at: Option<Instant>,
    assembler: Option<Assembler>,
    guard: Option<OutGuard>,
) -> Held {
    Held::Committed(Box::new(Committed {
        pump,
        facts,
        initial,
        transform,
        shaper,
        complete: false,
        content_at,
        assembler,
        guard,
        chunked: true,
    }))
}

fn timeout_failure(kind: TimerKind) -> Failed {
    Failed {
        code: Code::UpstreamTimeout,
        reason: None,
        status: 504,
        class: "timeout",
        message: None,
        timer: Some(kind),
        kind: None,
    }
}

/// The attempt failure of an upstream error event, by its effective status.
fn error_failure(status: u16, message: Option<String>) -> Failed {
    let class = match antirion_resilience::classify::status(status, true) {
        Class::Success | Class::MalformedResponse => Class::ServerError,
        c => c,
    };
    Failed {
        code: class.final_code(),
        reason: None,
        status,
        class: class.as_str(),
        message,
        timer: None,
        kind: None,
    }
}

/// Starts relaying a committed stream; returns the client response body. The relay task writes the
/// request-log record when the stream ends.
pub(crate) fn spawn(
    rt: &Arc<Runtime>,
    setup: Setup,
    committed: Committed,
    log: Draft,
    synthesized: bool,
) -> Body {
    let (tx, rx) = mpsc::channel(256);
    let sem = Arc::new(Semaphore::new(setup.policy.send_buffer.max(1)));
    let body = body::ChannelBody::new(rx);
    let rt2 = Arc::clone(rt);
    rt.tasks.spawn(async move {
        let mut setup = setup;
        let mut tee = setup.tee.take();
        if synthesized && let Some(t) = tee.as_mut() {
            t.terminated();
        }
        let mode = if synthesized {
            "synthesized"
        } else if committed.transform {
            "translated"
        } else {
            "passthrough"
        };
        let mut w = Writer {
            tx,
            sem,
            setup: &setup,
            last_write: Instant::now(),
            first_sent: None,
            blocked: Duration::ZERO,
            tee,
            frames: if setup.invoke_frames {
                Some(writer::Reframe::Invoke(invoke::InvokeFrames::default()))
            } else if setup.json_array {
                Some(writer::Reframe::Array(array::ArrayFrames::default()))
            } else {
                None
            },
            detached: false,
        };
        let mut facts = StreamFacts::default();
        let mut completed = None;
        let mut obs = Observed::default();
        let cut = rt2.drain.cut.clone();
        let ending = relay(
            &mut w,
            committed,
            &mut facts,
            &mut completed,
            &mut obs,
            &log,
            &cut,
        )
        .await;
        if let Some(c) = &setup.chunks {
            c.finish();
        }
        let outcome = match ending.reason {
            "completed" => StreamEnd::Completed,
            "client_disconnected" | "client_write_timeout" => StreamEnd::ClientGone,
            _ => StreamEnd::UpstreamFailed,
        };
        let unhealthy = match ending.reason {
            "upstream_truncated" | "upstream_malformed" | "upstream_event_too_large" => true,
            "upstream_idle_timeout" => !setup.idle_shortened,
            _ => false,
        };
        if unhealthy {
            setup.post_commit.failed(&rt2);
        }
        settle_now(&setup, &facts, &log, outcome).await;
        if outcome != StreamEnd::ClientGone {
            let _ = w.close_array().await;
        }
        let first_sent = w.first_sent;
        let ended = std::time::Instant::now();
        let usage = settled_usage(&facts, setup.input_estimate);
        log.update(|r| {
            r.stream = Some(StreamFact {
                end_reason: ending.reason,
                committed: true,
                relay_mode: mode,
            });
            r.client_disconnected = matches!(ending.reason, "client_disconnected" | "client_write_timeout");
            r.usage = usage;
            r.finish_reasons = std::mem::take(&mut facts.finish_reasons);
            if let Some(t) = facts.service_tier.take() {
                r.service_tier = Some(t);
            }
            r.tool_call_count = Some(facts.tool_calls);
            if r.response_model.is_none() {
                r.response_model.clone_from(&facts.model);
            }
            if r.response_id.is_none() {
                r.response_id.clone_from(&facts.id);
            }
            if let Some(e) = &ending.error {
                r.fail(e);
            }
            for w in std::mem::take(&mut obs.warnings) {
                if !r.translation_warnings.contains(&w) {
                    r.translation_warnings.push(w);
                }
            }
            r.stream_times = StreamTimes {
                first_byte_sent: first_sent.map(Instant::into_std),
                first_upstream_content: obs.content_at.map(Instant::into_std),
                first_content_sent: obs.first_content.map(Instant::into_std),
                last_content_sent: obs.last_content.map(Instant::into_std),
                relay: obs.relay,
            };
            if let Some(a) = r.attempts.last_mut() {
                a.end = ended;
                a.ttft = obs
                    .content_at
                    .map(|c| c.into_std().saturating_duration_since(a.start));
            }
        });
        if setup.trailer && outcome != StreamEnd::ClientGone {
            let mut t = http::HeaderMap::new();
            if outcome == StreamEnd::Completed
                && let Some(cost) = setup.account.cost()
                && let Ok(v) = http::HeaderValue::from_str(&antirion_limits::headers::cost(cost))
            {
                t.insert(http::HeaderName::from_static(antirion_limits::headers::COST), v);
                if let Some(d) = setup
                    .account
                    .cost_details()
                    .and_then(|d| http::HeaderValue::from_str(&d).ok())
                {
                    t.insert(
                        http::HeaderName::from_static(antirion_limits::headers::COST_DETAILS),
                        d,
                    );
                }
            }
            if setup.guard_trailer
                && let Some(v) = crate::guard::output_items(&log)
            {
                t.insert(http::HeaderName::from_static("x-antirion-guardrails"), v);
            }
            if let Ok(v) = http::HeaderValue::from_str(ending.reason) {
                t.insert(http::HeaderName::from_static(STREAM_END), v);
            }
            if let Some(v) = crate::reqlog::stream_overhead(&log)
                .and_then(|ms| http::HeaderValue::from_str(&format!("{ms:.3}")).ok())
            {
                t.insert(
                    http::HeaderName::from_static(antirion_core::headers::OVERHEAD_MS),
                    v,
                );
            }
            let _ = w.send_trailers(t).await;
        }
        let tee = w.tee.take();
        drop(w);
        if let Some(tee) = tee.filter(|_| outcome == StreamEnd::Completed) {
            tee.finish(&rt2, &log, facts.usage_raw.as_ref()).await;
        }
        drop(setup.inflight.take());
        if let (Some(store), Some(object)) = (&setup.store, &completed) {
            crate::respstate::keep(&rt2, &store.0, &store.1, object).await;
        }
        if let Some((project, settings)) = &setup.replay
            && !obs.replay.is_empty()
        {
            rt2.state.replay.keep(project, &obs.replay, rt2.now(), *settings);
        }
        if let Some(a) = obs.assembler.take() {
            log.response_value(a.finish());
        } else if let Some(object) = completed {
            log.response_value(object);
        }
        crate::reqlog::settle_stream(&rt2, &log);
    });
    body.boxed()
}

/// What the relay observed for the request's timing and content.
#[derive(Default)]
struct Observed {
    content_at: Option<Instant>,
    first_content: Option<Instant>,
    last_content: Option<Instant>,
    /// Time from reading an event to handing its bytes over, without client back-pressure.
    relay: Duration,
    assembler: Option<Assembler>,
    /// Response-side translation entries, for the request log only.
    warnings: Vec<String>,
    /// What the reasoning replay store keeps of a translated stream.
    replay: Vec<(String, antirion_dialects::xlate::replay::Saved)>,
}

/// Settles the request from what the stream reported or what was counted; a no-op once settled.
async fn settle_now(setup: &Setup, facts: &StreamFacts, log: &Draft, outcome: StreamEnd) {
    if let Some(t) = &facts.service_tier {
        log.update(|r| r.service_tier = Some(t.clone()));
    }
    let end = Ended::Stream {
        usage: settled_usage(facts, setup.input_estimate),
        raw: facts.usage_raw.clone().map(Box::new),
        outcome,
    };
    crate::account::settle(&setup.account, log, end).await;
}

/// Usage for accounting: what the upstream reported, completed by an estimate.
pub(crate) fn settled_usage(facts: &StreamFacts, input_estimate: u64) -> Usage {
    let mut u = facts.usage;
    if let (None, None, Some(total)) = (u.input, u.output, u.total) {
        u.input = Some(input_estimate.min(total));
        u.output = Some(total.saturating_sub(input_estimate));
        u.estimated = true;
        return u;
    }
    if u.output.is_none() {
        u.output = Some(facts.output_chars.div_ceil(4));
        u.estimated = true;
    }
    if u.input.is_none() {
        u.input = Some(input_estimate);
        u.estimated = true;
    }
    u
}
