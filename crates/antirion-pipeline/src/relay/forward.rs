//! The relay loop of a committed stream: events pulled from the upstream, shaped, guarded and
//! written to the client until the stream ends.

use std::time::Duration;

use antirion_core::Stage;
use antirion_error::{Code, GatewayError, Message, UpstreamFacts};
use bytes::Bytes;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::costed;
use super::events::{self, Kind, StreamFacts, classify};
use super::guard::{OutGuard, Stop};
use super::pump::{End, Pulled};
use super::writer::{SendEnd, Writer, send_end};
use super::{
    ClientDialect, Committed, Observed, Setup, error_failure, render, settle_now, settled_usage,
    terminator_frames,
};
use crate::account::StreamEnd;
use crate::reasons;
use crate::reqlog::Draft;

/// How a committed stream ended.
pub(super) struct Ending {
    pub(super) reason: &'static str,
    pub(super) error: Option<GatewayError>,
    pub(super) upstream_type: Option<String>,
}

impl Ending {
    pub(super) fn ok(reason: &'static str) -> Ending {
        Ending {
            reason,
            error: None,
            upstream_type: None,
        }
    }

    pub(super) fn fail(
        setup: &Setup,
        stream_end: &'static str,
        code: Code,
        reason: Option<antirion_error::Reason>,
        message: String,
    ) -> Ending {
        let mut e = GatewayError::new(code, Stage::Attempt, Message::built(message));
        e.reason = reason;
        e.attempts = setup.attempts;
        e.upstream = Some(Box::new(UpstreamFacts {
            provider: setup.provider.clone(),
            status: Some(200),
            failure_class: "stream",
            client_status: None,
            error: None,
        }));
        Ending {
            reason: stream_end,
            error: Some(e),
            upstream_type: None,
        }
    }
}

pub(super) async fn relay(
    w: &mut Writer<'_>,
    committed: Committed,
    facts_out: &mut StreamFacts,
    completed: &mut Option<serde_json::Value>,
    obs: &mut Observed,
    log: &Draft,
    cut: &CancellationToken,
) -> Ending {
    let Committed {
        mut pump,
        mut facts,
        initial,
        mut shaper,
        complete,
        content_at,
        assembler,
        mut guard,
        chunked,
        ..
    } = committed;
    obs.content_at = content_at;
    obs.assembler = assembler;
    let setup = w.setup;
    let mut held_usage: Vec<Bytes> = Vec::new();
    let ending = 'relay: {
        let initial = match setup.chunks.as_deref().filter(|_| !chunked) {
            Some(c) => match c.apply(initial).await {
                Ok(frames) => frames,
                Err(stopped) => break 'relay stopped_by_extension(setup, stopped),
            },
            None => initial,
        };
        let initial = if complete {
            settle_now(setup, &facts, log, StreamEnd::Completed).await;
            costed(setup, Vec::new(), initial)
        } else {
            initial
        };
        for bytes in initial {
            if let Err(e) = w.send(bytes).await {
                break 'relay send_end(e);
            }
        }
        if content_at.is_some() {
            obs.first_content = Some(w.last_write);
            obs.last_content = Some(w.last_write);
        }
        let far = Instant::now() + Duration::from_secs(86_400 * 365);
        let total_at = setup.total.map_or(far, |t| Instant::now() + t);
        let mut idle_at = setup.idle.map_or(far, |t| Instant::now() + t);
        loop {
            let buffering = guard.as_ref().and_then(OutGuard::buffer_heartbeat);
            let beat_at = w.last_write + buffering.unwrap_or(setup.policy.heartbeat);
            let pulled = tokio::select! {
                biased;
                () = w.tx.closed(), if !w.detached => {
                    if setup.policy.complete_on_disconnect {
                        w.detach();
                        continue;
                    }
                    break 'relay Ending::ok("client_disconnected");
                }
                () = cut.cancelled() => break 'relay Ending::fail(
                    setup, "drain_deadline", Code::Draining,
                    Some(antirion_runtime::drain::reasons::DRAIN_DEADLINE),
                    "The node stopped before the stream completed.".to_owned()),
                () = tokio::time::sleep_until(total_at) => break 'relay Ending::fail(
                    setup, "total_timeout", Code::UpstreamTimeout, Some(reasons::TOTAL_TIMEOUT),
                    format!("upstream timeout: stream_total {} exceeded", setup.total.map(antirion_resilience::format_duration).unwrap_or_default())),
                () = tokio::time::sleep_until(idle_at) => break 'relay Ending::fail(
                    setup, "upstream_idle_timeout", Code::UpstreamTimeout, Some(reasons::UPSTREAM_IDLE_TIMEOUT),
                    "The upstream stream was idle for too long.".to_owned()),
                () = tokio::time::sleep_until(beat_at), if (setup.heartbeat || buffering.is_some()) && !w.detached => None,
                p = pump.pull() => Some(p),
            };
            let Some(pulled) = pulled else {
                let beat = match buffering {
                    Some(_) => render::buffer_beat(setup.client),
                    None => render::heartbeat(setup.client),
                };
                if let Err(e) = w.send(beat).await {
                    break 'relay send_end(e);
                }
                continue;
            };
            if let Some(t) = setup.idle {
                idle_at = Instant::now() + t;
            }
            let read_at = Instant::now();
            let blocked_before = w.blocked;
            let ev = match pulled {
                Pulled::Activity => continue,
                Pulled::Event(ev) => ev,
                Pulled::End(End::Clean) if complete => {
                    if let Some(g) = guard.as_mut() {
                        let _ = Box::pin(g.finish(Vec::new(), None)).await;
                    }
                    break 'relay Ending::ok("completed");
                }
                Pulled::End(End::Clean) if facts.complete_at_clean_end(setup.dialect) => {
                    let mut tail = shaper.finish(setup, &facts, None);
                    if let Some(g) = guard.as_mut() {
                        match Box::pin(g.finish(tail, None)).await {
                            Ok(frames) => tail = frames,
                            Err(stop) => break 'relay Box::pin(guard_stopped(w, g, &stop, &facts)).await,
                        }
                    }
                    if let Some(c) = &setup.chunks {
                        match c.apply(tail).await {
                            Ok(frames) => tail = frames,
                            Err(stopped) => break 'relay stopped_by_extension(setup, stopped),
                        }
                    }
                    settle_now(setup, &facts, log, StreamEnd::Completed).await;
                    for bytes in costed(setup, std::mem::take(&mut held_usage), tail) {
                        if let Err(e) = w.send(bytes).await {
                            break 'relay send_end(e);
                        }
                    }
                    break 'relay Ending::ok("completed");
                }
                Pulled::End(End::Clean | End::Abnormal) => break 'relay truncated(setup),
                Pulled::End(End::TooLarge) => {
                    break 'relay Ending::fail(
                        setup,
                        "upstream_event_too_large",
                        Code::UpstreamBadResponse,
                        Some(reasons::UPSTREAM_EVENT_TOO_LARGE),
                        "An upstream stream event exceeded the size limit.".to_owned(),
                    );
                }
                Pulled::End(End::Malformed) => break 'relay malformed(setup),
                Pulled::End(End::ResponseTooLarge) => {
                    break 'relay Ending::fail(
                        setup,
                        "response_too_large",
                        Code::UpstreamBadResponse,
                        Some(reasons::RESPONSE_TOO_LARGE),
                        "The upstream stream exceeded the response size limit.".to_owned(),
                    );
                }
            };
            let c = classify(setup.dialect, &ev, setup.client_wants_usage, &mut facts);
            match c.kind {
                Kind::Empty => {}
                Kind::Malformed => break 'relay malformed(setup),
                Kind::Error {
                    status,
                    kind,
                    message,
                } => {
                    let code = error_failure(status, None).code;
                    let text = message
                        .map(|m| setup.scrub.apply(&m))
                        .unwrap_or_else(|| "The upstream reported an error.".to_owned());
                    let mut end = Ending::fail(setup, "upstream_error", code, None, text);
                    end.upstream_type = kind;
                    break 'relay end;
                }
                Kind::Terminator => {
                    pump.close();
                    if let Some(t) = w.tee.as_mut() {
                        t.terminated();
                    }
                    let Ok(mut out) = terminator_frames(setup, &mut shaper, &ev, &c, &mut facts) else {
                        break 'relay malformed(setup);
                    };
                    if let Some(g) = guard.as_mut() {
                        match Box::pin(g.finish(out, None)).await {
                            Ok(frames) => out = frames,
                            Err(stop) => break 'relay Box::pin(guard_stopped(w, g, &stop, &facts)).await,
                        }
                    }
                    if let Some(c) = &setup.chunks {
                        match c.apply(out).await {
                            Ok(frames) => out = frames,
                            Err(stopped) => break 'relay stopped_by_extension(setup, stopped),
                        }
                    }
                    settle_now(setup, &facts, log, StreamEnd::Completed).await;
                    for bytes in costed(setup, std::mem::take(&mut held_usage), out) {
                        if let Err(e) = w.send(bytes).await {
                            break 'relay send_end(e);
                        }
                    }
                    break 'relay Ending::ok("completed");
                }
                Kind::Preamble | Kind::Content => {
                    let is_content = matches!(c.kind, Kind::Content);
                    if let (Some(a), Some(j)) = (obs.assembler.as_mut(), c.json.as_ref()) {
                        a.push(j);
                    }
                    let Ok((mut out, _)) = shaper.event(setup, &ev, &c, &mut facts) else {
                        break 'relay malformed(setup);
                    };
                    if let Some(g) = guard.as_mut() {
                        match g.push(out) {
                            Ok(frames) => out = frames,
                            Err(stop) => break 'relay Box::pin(guard_stopped(w, g, &stop, &facts)).await,
                        }
                    }
                    if let Some(c) = &setup.chunks {
                        match c.apply(out).await {
                            Ok(frames) => out = frames,
                            Err(stopped) => break 'relay stopped_by_extension(setup, stopped),
                        }
                    }
                    let sent_any = !out.is_empty();
                    let reports = setup.report_cost && c.json.as_ref().is_some_and(costed::carries_usage);
                    let mut sending = std::mem::take(&mut held_usage);
                    if reports {
                        held_usage = out;
                    } else {
                        sending.extend(out);
                    }
                    for bytes in sending {
                        if let Err(e) = w.send(bytes).await {
                            break 'relay send_end(e);
                        }
                    }
                    let handed = Instant::now();
                    obs.relay += handed
                        .saturating_duration_since(read_at)
                        .saturating_sub(w.blocked.saturating_sub(blocked_before));
                    if is_content && sent_any && c.json.as_ref().is_some_and(events::carries_output) {
                        if obs.content_at.is_none() {
                            obs.content_at = Some(read_at);
                        }
                        obs.first_content.get_or_insert(handed);
                        obs.last_content = Some(handed);
                    }
                }
            }
        }
    };
    pump.close();
    let ending = if w.detached {
        Ending::ok("client_disconnected")
    } else {
        ending
    };
    let ending = match ending.error {
        Some(err) => {
            for bytes in held_usage {
                if w.send(bytes).await.is_err() {
                    break;
                }
            }
            let seq = guard
                .as_ref()
                .and_then(OutGuard::next_sequence)
                .or_else(|| shaper.next_sequence(&facts));
            let bytes = render::error_event(setup, &err, ending.reason, ending.upstream_type.as_deref(), seq);
            let _ = w.send(bytes).await;
            Ending {
                reason: ending.reason,
                error: Some(err),
                upstream_type: None,
            }
        }
        None => ending,
    };
    *facts_out = facts;
    *completed = shaper.translator.as_ref().and_then(|t| t.completed()).cloned();
    if let Some(t) = &shaper.translator {
        obs.warnings = t.warnings().entries();
        obs.replay = t.replay().to_vec();
    }
    ending
}

/// A guarded stream stopped after commit: the dialect's error event (`on_block: error`, the
/// default, or any guardrail failure), or a content-filter finish and the normal terminator.
async fn guarded_stop_frames(
    w: &mut Writer<'_>,
    g: &mut OutGuard,
    facts: &StreamFacts,
) -> Result<(), SendEnd> {
    let setup = w.setup;
    let mut frames = g.finish_frames();
    if setup.client == ClientDialect::Chat {
        if setup.client_wants_usage {
            let usage = settled_usage(facts, setup.input_estimate);
            frames.push(render::usage_chunk(facts, setup.rewrite_model.as_deref(), usage));
        }
        frames.push(Bytes::from_static(b"data: [DONE]\n\n"));
    }
    for bytes in frames {
        w.send(bytes).await?;
    }
    Ok(())
}

async fn guard_stopped(w: &mut Writer<'_>, g: &mut OutGuard, stop: &Stop, facts: &StreamFacts) -> Ending {
    let blocked = matches!(stop, Stop::Guardrail(e) if e.verdict == antirion_guardrails::Verdict::Block);
    if blocked && g.finishes_on_block() {
        return match guarded_stop_frames(w, g, facts).await {
            Ok(()) => Ending::ok("guardrail_blocked"),
            Err(e) => send_end(e),
        };
    }
    Ending {
        reason: if blocked {
            "guardrail_blocked"
        } else {
            "guardrail_unavailable"
        },
        error: Some(g.error(stop)),
        upstream_type: None,
    }
}

/// A `stream_chunk` hook stopped the stream after its commit: the dialect's error event follows.
fn stopped_by_extension(setup: &Setup, stopped: super::chunks::Stopped) -> Ending {
    let mut error = *stopped.error;
    error.attempts = setup.attempts;
    Ending {
        reason: stopped.reason,
        error: Some(error),
        upstream_type: None,
    }
}

fn truncated(setup: &Setup) -> Ending {
    Ending::fail(
        setup,
        "upstream_truncated",
        Code::UpstreamBadResponse,
        Some(reasons::UPSTREAM_TRUNCATED),
        "The upstream stream ended before completion.".to_owned(),
    )
}

fn malformed(setup: &Setup) -> Ending {
    Ending::fail(
        setup,
        "upstream_malformed",
        Code::UpstreamBadResponse,
        Some(reasons::UPSTREAM_MALFORMED),
        "The upstream sent a malformed stream event.".to_owned(),
    )
}

/// The usage-carrying frames held back, then the closing frames, with the settled cost added to
/// the usage they report (`pricing.report_in_body`).
fn costed(setup: &Setup, held: Vec<Bytes>, closing: Vec<Bytes>) -> Vec<Bytes> {
    let frames = held.into_iter().chain(closing);
    match setup.account.cost_breakdown().filter(|_| setup.report_cost) {
        Some((cost, components)) => frames.map(|f| costed::with_cost(&f, cost, &components)).collect(),
        None => frames.collect(),
    }
}
