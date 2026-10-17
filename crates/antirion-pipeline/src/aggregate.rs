//! A streaming upstream for a non-streaming client (`policies.streaming.stream_upstream`): the
//! attempt asks the upstream for a stream, reads it whole under the stream timers (time to first
//! token, idle, total), and gathers it into the one complete response the client asked for.

use antirion_dialects::xlate::ir::Client;
use antirion_dialects::xlate::resp;
use antirion_dialects::xlate::stream::{Collector, StreamCx};
use antirion_dialects::xlate::target::Upstream as XUp;
use antirion_egress::{InboundResponse, Payload};
use antirion_error::Code;
use antirion_providers::DeploymentRt;
use antirion_resilience::{Class, Failure, TimerKind};
use bytes::Bytes;
use serde_json::{Value, json};
use tokio::time::Instant;

use crate::apis::{self, Api};
use crate::attempt::{Done, Plan, essence};
use crate::cell::Cell;
use crate::outcome::json::{Answer, deliver};
use crate::outcome::{Attempt, is_json, response_model, upstream_or_new_id, wall_clock_secs};
use crate::reasons;
use crate::relay::events::{Kind, StreamFacts, classify};
use crate::relay::{End, Framing, Pulled, Pump};

/// The upstream API a request of `plan` in `cell` is gathered from, when it is: a non-streaming
/// Chat, Messages or Responses client under `stream_upstream`, on an API that streams.
pub(crate) fn applies(plan: &Plan<'_>, cell: Cell) -> bool {
    if plan.stream || !plan.stream_policy.stream_upstream || cell == Cell::InvokeAsMessages {
        return false;
    }
    let client = matches!(plan.endpoint.id, "chat_completions" | "messages" | "responses")
        && plan.endpoint.action.is_none();
    let api = apis::upstream_api(plan.endpoint, cell);
    client && xlate_upstream(api).is_some()
}

fn xlate_upstream(api: Api) -> Option<XUp> {
    Some(match api {
        Api::Chat => XUp::Chat,
        Api::Responses => XUp::Responses,
        Api::Anthropic => XUp::Anthropic,
        Api::Gemini => XUp::Gemini,
        Api::Converse => XUp::Converse,
        Api::Cohere => XUp::Cohere,
        _ => return None,
    })
}

/// The upstream body asking for a stream: `stream: true` in the body for the APIs that take it
/// there (with usage requested from a Chat upstream that honours `stream_options`); the others
/// choose a stream by their path.
pub(crate) fn stream_body(body: Payload, api: Api, d: &DeploymentRt) -> Payload {
    if !matches!(api, Api::Chat | Api::Responses | Api::Anthropic | Api::Cohere) {
        return body;
    }
    let Ok(mut v) = serde_json::from_slice::<Value>(&body.contiguous()) else {
        return body;
    };
    let Some(o) = v.as_object_mut() else {
        return body;
    };
    o.insert("stream".into(), Value::Bool(true));
    if api == Api::Chat && d.capabilities.flag("stream_usage") {
        o.insert("stream_options".into(), json!({"include_usage": true}));
    }
    Payload::from(serde_json::to_vec(&v).unwrap_or_default())
}

fn bad(a: &Attempt<'_>, reason: antirion_error::Reason, class: Class, status: u16, message: &str) -> Failure {
    a.fail(
        class,
        Code::UpstreamBadResponse,
        Some(reason),
        Some(status),
        antirion_error::Message::built(message.to_owned()),
    )
}

/// Reads the upstream's stream whole and answers the client with the complete response.
pub(crate) async fn collect(
    a: &Attempt<'_>,
    up: InboundResponse,
    api: Api,
) -> Result<(Done, Class), Failure> {
    let status = up.status.as_u16();
    let Some((dialect, upstream)) = apis::stream_dialect(api, a.deployment).zip(xlate_upstream(api)) else {
        return crate::outcome::json::json_response(a, up, api).await;
    };
    let InboundResponse {
        headers, body, abort, ..
    } = up;
    let ct = essence(&headers);
    let t = &a.timers.timeouts;
    let ttft = a.timers.fire_at(
        t.ttft_with_hint(a.deployment.capabilities.ttft_hint()),
        a.started,
        TimerKind::Ttft,
    );
    let mut framing = match ct.as_deref() {
        Some("text/event-stream") if !dialect.event_stream() => Some(Framing::Sse),
        Some("application/vnd.amazon.eventstream") if dialect.event_stream() => Some(Framing::EventStream),
        Some("application/x-ndjson" | "application/jsonl" | "application/ndjson") => Some(Framing::Ndjson),
        _ => None,
    };
    let mut body = body;
    if framing.is_none() && !dialect.event_stream() && is_json(ct.as_deref()) {
        let (array, rejoined) = crate::streamed::peek(a, ttft, body).await?;
        body = rejoined;
        framing = array.then_some(Framing::JsonArray);
    }
    let Some(framing) = framing else {
        let whole = InboundResponse {
            status: up.status,
            headers,
            body,
            abort,
        };
        return crate::outcome::json::json_response(a, whole, api).await;
    };
    let policy = &a.plan.stream_policy;
    let mut pump = Pump::new(
        body,
        policy.max_event_bytes,
        dialect,
        framing,
        policy.max_response_bytes,
    );
    let shown = response_model(a, &a.deployment.model);
    let client = match apis::client_dialect(a.plan.endpoint, a.cell) {
        crate::relay::ClientDialect::Anthropic => Client::Anthropic,
        crate::relay::ClientDialect::Responses => Client::Responses,
        _ => Client::Chat,
    };
    let restore = a.xlate.as_ref().map(|x| x.restore.clone()).unwrap_or_default();
    let request = a
        .plan
        .xlate
        .as_ref()
        .map(|x| x.body.clone())
        .or_else(|| a.plan.parsed.doc().cloned())
        .unwrap_or(Value::Null);
    let fallback = upstream_or_new_id(&headers);
    let mut collector = Collector::new(StreamCx {
        client,
        upstream,
        restore: restore.clone(),
        provider: a.deployment.provider.name.clone(),
        model: shown.clone(),
        created: wall_clock_secs(),
        fallback_id: fallback.clone(),
        request: request.clone(),
        include_usage: false,
        response_id: None,
    });
    let mut facts = StreamFacts::default();
    let far = Instant::now() + std::time::Duration::from_secs(86_400 * 365);
    let deadline_at = a.timers.deadline_at.unwrap_or(far);
    let mut content = false;
    let mut idle_from = Instant::now();
    loop {
        let (first_at, first_kind) = ttft.filter(|_| !content).unwrap_or((far, TimerKind::Ttft));
        let idle_at = t.stream_idle.map_or(far, |d| idle_from + d);
        let (at, kind) = [
            (first_at, first_kind),
            (idle_at, TimerKind::StreamIdle),
            (deadline_at, TimerKind::Deadline),
        ]
        .into_iter()
        .min_by_key(|(at, _)| *at)
        .unwrap_or((far, TimerKind::Deadline));
        let pulled = tokio::select! {
            () = a.cx.gone.cancelled() => return Err(crate::failure::cancelled(a.deployment, a.attempts)),
            () = tokio::time::sleep_until(at) => {
                a.record(Some(status), Class::Timeout, Some(kind));
                return Err(crate::failure::timeout(a.deployment, a.attempts, kind, &a.timers));
            }
            p = pump.pull() => p,
        };
        idle_from = Instant::now();
        let ev = match pulled {
            Pulled::Activity => continue,
            Pulled::Event(ev) => ev,
            Pulled::End(End::Clean) if facts.native_finished || facts.complete_at_clean_end(dialect) => break,
            Pulled::End(End::Clean) if !content => {
                return Err(bad(
                    a,
                    reasons::EMPTY_STREAM,
                    Class::UpstreamReset,
                    status,
                    "empty upstream stream",
                ));
            }
            Pulled::End(End::Clean | End::Abnormal) => {
                return Err(bad(
                    a,
                    reasons::UPSTREAM_TRUNCATED,
                    Class::UpstreamReset,
                    status,
                    "the upstream stream ended before completion",
                ));
            }
            Pulled::End(End::TooLarge) => {
                return Err(bad(
                    a,
                    reasons::UPSTREAM_EVENT_TOO_LARGE,
                    Class::MalformedResponse,
                    status,
                    "an upstream stream event exceeded the size limit",
                ));
            }
            Pulled::End(End::ResponseTooLarge) => return Err(a.too_large(status)),
            Pulled::End(End::Malformed) => {
                return Err(bad(
                    a,
                    reasons::UPSTREAM_MALFORMED,
                    Class::MalformedResponse,
                    status,
                    "the upstream sent a malformed stream event",
                ));
            }
        };
        let c = classify(dialect, &ev, false, &mut facts);
        match c.kind {
            Kind::Empty => {}
            Kind::Malformed => {
                return Err(bad(
                    a,
                    reasons::UPSTREAM_MALFORMED,
                    Class::MalformedResponse,
                    status,
                    "the upstream sent a malformed stream event",
                ));
            }
            Kind::Error {
                status: eff, message, ..
            } => {
                let class = match antirion_resilience::classify::status(eff, true) {
                    Class::Success | Class::MalformedResponse => Class::ServerError,
                    c => c,
                };
                let text = message.map_or(
                    antirion_error::Message::Fixed("The upstream reported an error."),
                    |m| antirion_error::Message::built(crate::outcome::scrub_for(a).apply(&m)),
                );
                return Err(a.fail(class, class.final_code(), None, Some(eff), text));
            }
            Kind::Terminator => {
                facts.native_finished = true;
                if let Some(j) = &c.json {
                    let _ = collector.push(ev.event.as_deref(), j);
                }
            }
            Kind::Preamble | Kind::Content => {
                content |= matches!(c.kind, Kind::Content);
                if let Some(j) = &c.json
                    && collector.push(ev.event.as_deref(), j).is_err()
                {
                    return Err(bad(
                        a,
                        reasons::UPSTREAM_MALFORMED,
                        Class::MalformedResponse,
                        status,
                        "the upstream sent a malformed stream event",
                    ));
                }
            }
        }
    }
    drop(pump);
    let (response, _) = collector.finish();
    let mut warnings = a.xlate.as_ref().map(|x| x.warnings.clone()).unwrap_or_default();
    let cx = resp::Cx {
        upstream,
        client,
        restore: &restore,
        provider: &a.deployment.provider.name,
        model: &shown,
        created: wall_clock_secs(),
        fallback_id: &fallback,
        request: &request,
    };
    let client_body = resp::encode(&response, &cx, &mut warnings);
    let translated_cell = matches!(a.cell, Cell::Translate { .. });
    deliver(
        a,
        Answer {
            status,
            raw: Bytes::new(),
            facts: client_body.clone(),
            shown,
            translated: Ok(Some(client_body)),
            warnings: translated_cell.then_some(warnings),
            content_type: None,
            up_headers: headers,
        },
    )
    .await
}
