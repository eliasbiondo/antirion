//! A streaming request's upstream response: relayed through the holding phase and the relay task,
//! or synthesized from a complete reply.

use antirion_dialects::xlate::stream::{StreamCx, Translator};
use antirion_egress::{InboundBody, InboundResponse};
use antirion_error::Message;
use antirion_error::reason::hygiene;
use antirion_resilience::{Class, Failure};
use futures_util::StreamExt;
use http::header::{CACHE_CONTROL, CONTENT_TYPE};
use http::{HeaderMap, HeaderName, HeaderValue, Response};
use http_body_util::BodyExt;
use serde_json::Value;

use crate::apis;
use crate::attempt::{Done, essence};
use crate::cell::Cell;
use crate::ctx::Body;
use crate::failure;
use crate::outcome::{Attempt, is_json, response_head, scrub_for, upstream_or_new_id, wall_clock_secs};
use crate::relay::{
    self, ClientDialect, Committed, Framing, Held, OutGuard, Pump, Setup, Shaper, StreamDialect, StreamFacts,
};
use crate::reqlog::StreamFact;

fn setup(a: &Attempt<'_>, dialect: StreamDialect, client: ClientDialect) -> Setup {
    let plan = a.plan;
    let name = match (client, plan.endpoint.id) {
        (ClientDialect::Anthropic, _) => "anthropic_messages",
        (ClientDialect::Responses, _) => "openai_responses",
        (ClientDialect::Gemini, _) => "gemini",
        (ClientDialect::Bedrock, _) => "bedrock",
        (_, "completions") => "openai_completions",
        _ => "openai_chat",
    };
    let upstream_mode =
        a.cx.snap
            .config
            .doc
            .pointer("/api/response_model")
            .and_then(Value::as_str)
            == Some("upstream");
    let rewrite = !upstream_mode && plan.requested != a.deployment.model;
    let t = &a.timers.timeouts;
    Setup {
        dialect,
        client,
        client_wants_usage: plan.client_wants_usage,
        rewrite_model: rewrite.then(|| plan.requested.clone()),
        policy: plan.stream_policy.clone(),
        heartbeat: !matches!(client, ClientDialect::Gemini | ClientDialect::Bedrock)
            && a.cell != Cell::InvokeAsMessages
            && plan.stream_policy.heartbeat_dialects.iter().any(|d| d == name),
        idle: t.stream_idle,
        total: t.stream_total,
        ttft_at: a.timers.fire_at(
            t.ttft_with_hint(a.deployment.capabilities.ttft_hint()),
            a.started,
            antirion_resilience::TimerKind::Ttft,
        ),
        deadline_at: a.timers.deadline_at,
        input_estimate: plan.input_estimate,
        provider: a.deployment.provider.name.clone(),
        attempts: a.attempts,
        request_id: a.cx.request_id.clone(),
        scrub: scrub_for(a),
        store: None,
        replay: match a.cell {
            Cell::Translate { .. } => plan.xlate.as_ref().and_then(|x| x.replay.clone()),
            _ => None,
        },
        inflight: a.inflight.clone(),
        account: a.cx.account.clone(),
        trailer: crate::account::accepts_trailers(plan.client_headers),
        guard_trailer: crate::guard::trailer_applies(plan.guard.as_deref(), plan.client_headers),
        tee: None,
        assemble: a
            .cx
            .log
            .read(|r| r.content.enabled || !r.comparisons.is_empty())
            .unwrap_or(false),
        invoke_frames: a.cell == Cell::InvokeAsMessages,
        refusal_fails: a.refusal_fails,
        idle_shortened: a.timers.shortened(antirion_resilience::TimerKind::StreamIdle),
        post_commit: a.plan.post_commit.clone(),
        json_array: plan.json_array && client == ClientDialect::Gemini,
        reframe: false,
        report_cost: a.cx.snap.limits.pricing.report_in_body
            && matches!(
                client,
                ClientDialect::Chat | ClientDialect::Anthropic | ClientDialect::Responses
            )
            && a.cell != Cell::InvokeAsMessages,
        chunks: plan
            .hooks
            .as_deref()
            .and_then(|h| relay::chunks::ChunkHooks::new(h, client, &a.cx.rt, &a.cx.log, &scrub_for(a))),
    }
}

/// A streaming request: a relayed stream, or one synthesized from a complete JSON reply.
pub(crate) async fn stream(
    a: &Attempt<'_>,
    up: InboundResponse,
    dialect: StreamDialect,
    client: ClientDialect,
) -> Result<Done, Failure> {
    let status = up.status.as_u16();
    let mut setup = setup(a, dialect, client);
    let out_mode = a.plan.guard.as_ref().and_then(|g| g.out_mode());
    if matches!(out_mode, Some(crate::guard::OutMode::Observe)) {
        setup.rewrite_model = None;
    }
    let InboundResponse { headers, body, .. } = up;
    setup.tee = crate::cache::Tee::new(a, status, &headers);
    let ct = essence(&headers);
    let mut framing = match ct.as_deref() {
        Some("application/vnd.amazon.eventstream") if dialect.event_stream() => Some(Framing::EventStream),
        Some("text/event-stream") if !dialect.event_stream() => Some(Framing::Sse),
        Some(NDJSON | "application/jsonl" | "application/ndjson") if !dialect.event_stream() => {
            Some(Framing::Ndjson)
        }
        _ => None,
    };
    let mut body = body;
    if framing.is_none() && !dialect.event_stream() && is_json(ct.as_deref()) {
        let (array, rejoined) = match peek(a, setup.ttft_at, body).await {
            Ok(p) => p,
            Err(f) => return Err(f),
        };
        body = rejoined;
        if array {
            framing = Some(Framing::JsonArray);
        }
    }
    if framing.is_none() {
        if !is_json(ct.as_deref()) {
            return Err(a.bad(
                hygiene::UNEXPECTED_CONTENT_TYPE,
                status,
                format!(
                    "upstream answered a stream request with content-type {}",
                    ct.as_deref().unwrap_or("none")
                ),
            ));
        }
        let raw = match a.read(body, a.body_cap()).await? {
            Ok(b) => b,
            Err(why) => return Err(a.unread(why, status)),
        };
        let Ok(v) = serde_json::from_slice::<Value>(&raw) else {
            return Err(a.bad(
                hygiene::INVALID_BODY,
                status,
                "upstream response is not valid json".to_owned(),
            ));
        };
        if let Some(err) = v.get("error").filter(|e| e.is_object()) {
            return Err(a.error_object(err));
        }
        if dialect == StreamDialect::Gemini && relay::native::missing_thought_signature(&v) {
            return Err(crate::outcome::json::missing_signature(a));
        }
        let mut facts = StreamFacts::default();
        let api = apis::upstream_api(a.plan.endpoint, a.cell);
        let (translated, warnings) = crate::xlate::response(a, api, &v, &a.plan.requested, &headers, status)?;
        if let Some(w) = warnings {
            let entries = w.entries();
            a.cx.log.update(|r| r.translation_warnings = entries);
        }
        let mut body = match translated {
            Ok(Some(t)) => t,
            Ok(None) => v.clone(),
            Err(()) => Value::Null,
        };
        if let Some(g) = &a.plan.guard
            && let Err(e) = Box::pin(crate::guard::output(a.cx, g, &mut body, a.attempts)).await
        {
            a.record(Some(status), Class::Success, None);
            return Err(guarded(status, e));
        }
        if let (Some(m), Some(o)) = (&setup.rewrite_model, body.as_object_mut())
            && o.contains_key("model")
        {
            o.insert("model".into(), Value::from(m.as_str()));
        }
        let synthesized = match client {
            ClientDialect::Chat => relay::synthesize(
                &body,
                setup.rewrite_model.as_deref(),
                setup.client_wants_usage,
                &mut facts,
            ),
            ClientDialect::Anthropic => relay::synth::anthropic(&body, &mut facts),
            ClientDialect::Responses => relay::synth::responses(&body, &mut facts),
            ClientDialect::Gemini => relay::synth::gemini(&body, &mut facts),
            ClientDialect::Bedrock
                if dialect == StreamDialect::Converse || matches!(a.cell, Cell::Translate { .. }) =>
            {
                relay::synth::converse(&body, &mut facts)
            }
            ClientDialect::Bedrock => None,
        };
        let Some(initial) = synthesized else {
            return Err(a.bad(
                hygiene::INVALID_BODY,
                status,
                "upstream response cannot be used".to_owned(),
            ));
        };
        return Ok(Done::Response(committed(
            a,
            setup,
            Committed::synthesized(initial, facts, client != ClientDialect::Chat),
            status,
            &headers,
            true,
        )));
    }
    setup.reframe = matches!(framing, Some(Framing::Ndjson | Framing::JsonArray));
    let pump = Pump::new(
        body,
        setup.policy.max_event_bytes,
        dialect,
        framing.unwrap_or(Framing::Sse),
        setup.policy.max_response_bytes,
    );
    let store_id = match (a.cell, &a.plan.turn) {
        (
            Cell::Translate {
                client: ClientDialect::Responses,
                ..
            },
            Some(_),
        ) => Some(crate::respstate::new_id()),
        _ => None,
    };
    let shaper = shaper(a, &headers, store_id.as_deref());
    let out_guard = a.plan.guard.as_ref().zip(out_mode).map(|(g, mode)| {
        a.cx.guard.stream_mode(mode.name());
        OutGuard::new(
            mode,
            client,
            std::sync::Arc::clone(g),
            std::sync::Arc::clone(&a.cx.rt),
            a.cx.log.clone(),
            a.attempts,
        )
    });
    match relay::hold(&setup, pump, shaper, &a.cx.gone, out_guard, &a.cx.guard).await {
        Held::Guarded(e, answered) => {
            if let Some(answered) = answered {
                let relay::Answered { usage, raw } = *answered;
                a.cx.log.update(|r| {
                    r.usage = usage;
                    r.usage_raw = raw;
                });
            }
            a.record(Some(status), Class::Success, None);
            Err(guarded(status, *e))
        }
        Held::Cancelled => {
            a.cx.log.update(|r| {
                r.stream = Some(StreamFact {
                    end_reason: "client_disconnected",
                    committed: false,
                    relay_mode: "passthrough",
                });
                r.client_disconnected = true;
            });
            Err(failure::cancelled(a.deployment, a.attempts))
        }
        Held::Failed(f) => {
            if let Some(kind) = f.timer {
                return Err(failure::timeout(a.deployment, a.attempts, kind, &a.timers));
            }
            let message = match f.message {
                Some(m) => Message::built(setup.scrub.apply(&m)),
                None => Message::Fixed("The upstream stream cannot be used."),
            };
            let class = Class::parse(f.class).unwrap_or(Class::ServerError);
            let mut fail = a.fail(class, f.code, f.reason, Some(f.status), message);
            if let (Some(kind), Some(u)) = (f.kind, fail.error.upstream.as_mut()) {
                u.error = Some(Box::new(antirion_error::UpstreamError {
                    kind: Some(kind),
                    ..antirion_error::UpstreamError::default()
                }));
            }
            Err(fail)
        }
        Held::Committed(c) => {
            let mut setup = setup;
            if let Some(turn) = &a.plan.turn {
                match (&store_id, c.response_id()) {
                    (Some(id), _) => setup.store = Some(Box::new((turn.clone(), id.clone()))),
                    (None, Some(native)) => {
                        Box::pin(crate::respstate::link(a.cx, turn, native, &a.deployment.name)).await
                    }
                    (None, None) => {}
                }
            }
            Ok(Done::Response(committed(a, setup, *c, status, &headers, false)))
        }
    }
}

/// The NDJSON media type.
const NDJSON: &str = "application/x-ndjson";

/// Whether a JSON answer to a stream request is a streamed JSON array (its first byte that is not
/// whitespace is `[`), and the body with the bytes read put back. Waits under the stream's first
/// timers and the client.
pub(crate) async fn peek(
    a: &Attempt<'_>,
    first_at: Option<(tokio::time::Instant, antirion_resilience::TimerKind)>,
    body: InboundBody,
) -> Result<(bool, InboundBody), Failure> {
    let mut stream = body.into_data_stream();
    let mut seen: Vec<Result<bytes::Bytes, antirion_egress::BodyError>> = Vec::new();
    let far = tokio::time::Instant::now() + std::time::Duration::from_secs(86_400 * 365);
    let (fire_at, kind) = first_at.unwrap_or((far, antirion_resilience::TimerKind::Ttft));
    let first = loop {
        let next = tokio::select! {
            () = a.cx.gone.cancelled() => return Err(failure::cancelled(a.deployment, a.attempts)),
            () = tokio::time::sleep_until(fire_at) => {
                return Err(failure::timeout(a.deployment, a.attempts, kind, &a.timers));
            }
            n = stream.next() => n,
        };
        match next {
            Some(Ok(chunk)) => {
                let lead = chunk.iter().find(|b| !b.is_ascii_whitespace()).copied();
                seen.push(Ok(chunk));
                if lead.is_some() {
                    break lead;
                }
            }
            Some(Err(e)) => {
                seen.push(Err(e));
                break None;
            }
            None => break None,
        }
    };
    let rejoined = futures_util::stream::iter(seen)
        .chain(stream)
        .map(|r| r.map(http_body::Frame::data));
    Ok((
        first == Some(b'['),
        http_body_util::BodyExt::boxed_unsync(http_body_util::StreamBody::new(rejoined)),
    ))
}

/// A guardrail outcome that ends the request: final, never retried.
pub(crate) fn guarded(status: u16, e: antirion_error::GatewayError) -> Failure {
    Failure {
        class: Class::GuardrailBlocked,
        status: Some(status),
        timer: None,
        hint: None,
        error: Box::new(e),
    }
}

/// The stream transformation of the attempt's cell.
fn shaper(a: &Attempt<'_>, headers: &HeaderMap, store_id: Option<&str>) -> Shaper {
    let (Cell::Translate { client, .. }, Some(x), Some(px)) = (a.cell, &a.xlate, &a.plan.xlate) else {
        return Shaper::default();
    };
    let Some(client) = crate::xlate::client_of(client) else {
        return Shaper::default();
    };
    let include_usage = px
        .body
        .pointer("/stream_options/include_usage")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let cx = StreamCx {
        client,
        upstream: x.upstream,
        restore: x.restore.clone(),
        provider: a.deployment.provider.name.clone(),
        model: shown_model(a),
        created: wall_clock_secs(),
        fallback_id: upstream_or_new_id(headers),
        request: px.body.clone(),
        include_usage,
        response_id: store_id.map(str::to_owned),
    };
    Shaper {
        translator: Some(Translator::new(cx)),
    }
}

/// The model name a stream's events carry.
fn shown_model(a: &Attempt<'_>) -> String {
    crate::outcome::response_model(a, &a.deployment.model)
}

fn committed(
    a: &Attempt<'_>,
    setup: Setup,
    c: Committed,
    status: u16,
    upstream: &HeaderMap,
    synthesized: bool,
) -> Response<Body> {
    let mode = c.mode();
    let event_stream = setup.client == ClientDialect::Bedrock || setup.invoke_frames;
    let json_array = setup.json_array;
    a.record(Some(status), Class::Success, None);
    let mut entries = a
        .xlate
        .as_ref()
        .map(|x| x.warnings.request_entries())
        .unwrap_or_default();
    if synthesized
        && let Some(logged) =
            a.cx.log
                .read(|r| r.translation_warnings.clone())
                .filter(|l| !l.is_empty())
    {
        entries = logged;
    }
    let warnings = antirion_dialects::xlate::warn::header_value(&entries);
    a.cx.log.update(|r| r.translation_warnings = entries);
    let body = relay::spawn(&a.cx.rt, setup, c, a.cx.log.hand_off(), synthesized);
    let mut resp = Response::new(body);
    let h = resp.headers_mut();
    let content_type = if event_stream {
        "application/vnd.amazon.eventstream"
    } else if json_array {
        "application/json; charset=utf-8"
    } else {
        "text/event-stream; charset=utf-8"
    };
    h.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    h.insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    h.insert(
        HeaderName::from_static(antirion_core::headers::STREAM_MODE),
        HeaderValue::from_static(mode),
    );
    if let Some(v) = warnings.and_then(|v| HeaderValue::from_str(&v).ok()) {
        h.insert(HeaderName::from_static("x-antirion-translation-warnings"), v);
    }
    response_head(a, &mut resp, status, upstream);
    resp
}
