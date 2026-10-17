//! What an upstream response becomes: a classified attempt failure with the client's error, a
//! relayed or synthesized stream, or a checked non-streaming body.

use std::sync::Arc;
use std::time::Duration;

use antirion_core::UtcMicros;
use antirion_egress::headers::upstream_request_id;
use antirion_egress::{BodyError, InboundBody, InboundResponse};
use antirion_error::reason::hygiene;
use antirion_error::{Code, Message, Reason};
use antirion_providers::DeploymentRt;
use antirion_resilience::{AttemptTimers, Class, Failure, ResponsePredicate, TimerKind};
use antirion_router::InFlight;
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use http::header::CONTENT_TYPE;
use http::{HeaderMap, HeaderName, HeaderValue, Response};
use http_body_util::BodyExt;
use serde_json::Value;
use tokio::time::Instant;

use crate::apis;
use crate::attempt::{Done, Plan, decision_headers};
use crate::cell::Cell;
use crate::ctx::{Body, BoxError, Ctx, full, streamed};
use crate::failure;
use crate::relay::{self};
use crate::reqlog::AttemptFact;
use antirion_obs::trace::SpanId;

mod error;
pub(crate) mod json;
mod transcript;

pub(crate) use error::error_response;
use json::json_response;

/// The attempt a response belongs to.
pub(crate) struct Attempt<'a> {
    pub(crate) cx: &'a Ctx,
    pub(crate) plan: &'a Plan<'a>,
    pub(crate) deployment: &'a Arc<DeploymentRt>,
    pub(crate) cell: Cell,
    pub(crate) attempts: u16,
    pub(crate) started: Instant,
    pub(crate) started_at: UtcMicros,
    pub(crate) key: Option<String>,
    pub(crate) wait: Duration,
    pub(crate) via: Option<String>,
    /// The deployment of the request's first attempt.
    pub(crate) first: String,
    pub(crate) timers: AttemptTimers,
    /// The attempt counts as in flight on its deployment while this is held.
    pub(crate) inflight: Option<Arc<InFlight>>,
    /// The credentials obtained for this attempt (tokens, temporary keys).
    pub(crate) acquired: Vec<String>,
    /// The attempt's span, parent of the upstream's.
    pub(crate) span: SpanId,
    /// The upstream's host and port.
    pub(crate) server: Option<(String, u16)>,
    /// What the request translation produced, in a translated cell.
    pub(crate) xlate: Option<crate::xlate::AttemptXlate>,
    /// A refusal before the commit fails the attempt: a class fallback follows.
    pub(crate) refusal_fails: bool,
    /// The client's own provider key this attempt sent.
    pub(crate) byok: Option<String>,
    /// A non-streaming request whose upstream streams (`stream_upstream`): the stream is gathered.
    pub(crate) aggregate: bool,
    /// The route's response predicates: a 2xx answer that matches one is this attempt's failure.
    pub(crate) predicates: &'a [ResponsePredicate],
    /// The attempt takes part in a hedge race.
    pub(crate) race: Option<Race>,
}

/// An attempt of a hedge race: dropped before it recorded an outcome, it was cancelled because
/// another attempt won, and records itself as `hedge_cancelled`.
#[derive(Debug)]
pub(crate) struct Race {
    /// The attempt is a hedge, not the race's first attempt.
    pub(crate) hedge: bool,
    pub(crate) recorded: std::sync::atomic::AtomicBool,
    /// The attempt's upstream exchange, while it runs on a task of its own (a complete response).
    pub(crate) exchange: std::sync::Mutex<Option<crate::hedge_tail::Exchange>>,
}

impl Race {
    pub(crate) fn new(hedge: bool) -> Race {
        Race {
            hedge,
            recorded: std::sync::atomic::AtomicBool::new(false),
            exchange: std::sync::Mutex::new(None),
        }
    }
}

impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        let Some(race) = &self.race else {
            return;
        };
        if race.recorded.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let exchange = race.exchange.lock().ok().and_then(|mut e| e.take());
        self.record(None, Class::HedgeCancelled, None);
        let Some(exchange) = exchange else {
            return;
        };
        let span = self.span;
        self.cx.log.update(|r| {
            if let Some(a) = r.attempts.iter_mut().find(|a| a.span == span) {
                a.tail = true;
            }
        });
        self.cx.log.begin_tail();
        let tail = crate::hedge_tail::Tail {
            rt: Arc::clone(&self.cx.rt),
            snap: Arc::clone(&self.cx.snap),
            draft: self.cx.log.clone(),
            deferred: self.cx.account.deferred(),
            deployment: Arc::clone(self.deployment),
            span,
            purpose: if race.hedge {
                antirion_limits::ledger::Purpose::Hedge
            } else {
                antirion_limits::ledger::Purpose::Primary
            },
            exchange,
        };
        self.cx.rt.tasks.spawn(tail.run());
    }
}

/// The largest upstream error body read.
const MAX_ERROR_BODY: u64 = 1 << 20;

pub(crate) fn data_stream(
    body: InboundBody,
) -> impl futures_util::Stream<Item = Result<Bytes, BoxError>> + Send + Unpin + 'static {
    body.into_data_stream()
        .map(|r| r.map_err(|e| Box::new(e) as BoxError))
}

/// Why a whole upstream body could not be read.
#[derive(Copy, Clone, Debug)]
pub(crate) enum Unread {
    TooLarge,
    /// The body was cut short.
    Truncated,
    /// The body is not valid for its content coding.
    Corrupt,
}

/// Reads a whole (decoded) body, refusing it once it exceeds `cap`.
pub(crate) async fn collect(body: InboundBody, cap: u64) -> Result<Bytes, Unread> {
    let mut chunks = body.into_data_stream();
    let mut out = BytesMut::new();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|e| match e {
            BodyError::Read(_) => Unread::Truncated,
            BodyError::Corrupt => Unread::Corrupt,
        })?;
        if u64::try_from(out.len().saturating_add(chunk.len())).unwrap_or(u64::MAX) > cap {
            return Err(Unread::TooLarge);
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out.freeze())
}

pub(crate) fn is_json(essence: Option<&str>) -> bool {
    essence.is_none_or(|e| e == "application/json" || (e.starts_with("application/") && e.ends_with("+json")))
}

impl Attempt<'_> {
    /// Adds this attempt to the request log (once: a stream records its attempt at commit).
    pub(crate) fn record(&self, status: Option<u16>, class: Class, timer: Option<TimerKind>) {
        self.record_with(status, class, timer, None);
    }

    /// Adds this failed attempt, with what its failure reported.
    pub(crate) fn record_failure(&self, status: Option<u16>, f: &Failure) {
        self.record_with(status, f.class, f.timer, Some(f));
    }

    fn record_with(
        &self,
        status: Option<u16>,
        class: Class,
        timer: Option<TimerKind>,
        fail: Option<&Failure>,
    ) {
        if let Some(r) = &self.race {
            r.recorded.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let reason = if self.race.as_ref().is_some_and(|r| r.hedge) {
            "hedge"
        } else if self.attempts == 1 {
            "primary"
        } else if self.via.is_some() {
            "fallback"
        } else {
            "retry"
        };
        let end = Instant::now().into_std();
        let now = self.cx.rt.now();
        let failed = class != Class::Success;
        self.cx.log.update(|r| {
            if r.attempts.iter().any(|a| a.span == self.span) {
                return;
            }
            let error_body = r.pending_error_body.take().filter(|_| failed);
            let (usage, usage_raw) = r
                .pending_error_usage
                .take()
                .filter(|_| failed)
                .map_or((None, None), |(u, raw)| (Some(u), Some(raw)));
            let start = self.started.into_std();
            let at = r.attempts.partition_point(|a| a.start <= start);
            r.attempts.insert(
                at,
                AttemptFact {
                    reason,
                    provider: self.deployment.provider.name.clone(),
                    provider_type: self.deployment.provider.kind.as_str().to_owned(),
                    deployment: self.deployment.name.clone(),
                    upstream_model: self.deployment.model.clone(),
                    credential: self.key.clone().filter(|k| k != antirion_providers::SINGLE_KEY),
                    key: self.key.clone(),
                    started_at: self.started_at,
                    start: self.started.into_std(),
                    end,
                    ttft: None,
                    status,
                    class: class.as_str(),
                    retry_after_ms: fail
                        .and_then(|f| f.hint)
                        .map(|h| (h.since(now).as_secs_f64() * 1000.0).round() as u64),
                    upstream_request_id: if failed {
                        fail.and_then(|f| f.error.extra_headers.as_ref())
                            .and_then(|h| h.get(antirion_core::headers::UPSTREAM_REQUEST_ID))
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_owned)
                    } else {
                        r.upstream_request_id.clone()
                    },
                    error_message: fail
                        .filter(|_| failed && class != Class::ClientCancelled)
                        .map(|f| antirion_obs::truncate_utf8(f.error.message.as_str(), 2048).to_owned()),
                    error_body,
                    usage,
                    usage_raw,
                    span: self.span,
                    server: self.server.clone(),
                    wait_ms: u64::try_from(self.wait.as_millis()).unwrap_or(u64::MAX),
                    via: self.via.clone(),
                    timeout: timer.map(TimerKind::as_str),
                    tail: false,
                },
            );
        });
    }

    pub(crate) fn fail(
        &self,
        class: Class,
        code: Code,
        reason: Option<Reason>,
        status: Option<u16>,
        message: Message,
    ) -> Failure {
        failure::fail(
            self.deployment,
            self.attempts,
            class,
            code,
            reason,
            status,
            message,
        )
    }

    /// A 2xx answer that matched a response predicate: a `server_error` the loop moves on from
    /// without counting it toward health.
    pub(crate) fn predicate_matched(&self, status: u16) -> Failure {
        self.fail(
            Class::ServerError,
            Code::UpstreamBadResponse,
            Some(antirion_resilience::reasons::RESPONSE_PREDICATE_MATCHED),
            Some(status),
            Message::Fixed("The upstream answer matched a response predicate of the route."),
        )
    }

    /// A malformed answer: 502 `upstream_bad_response` naming the defect.
    pub(crate) fn bad(&self, reason: Reason, status: u16, message: String) -> Failure {
        self.fail(
            Class::MalformedResponse,
            Code::UpstreamBadResponse,
            Some(reason),
            Some(status),
            Message::built(message),
        )
    }

    /// A body cut short after the headers: `upstream_reset` for the loop, a bad response for the client.
    pub(crate) fn truncated(&self, status: u16) -> Failure {
        self.fail(
            Class::UpstreamReset,
            Code::UpstreamBadResponse,
            Some(hygiene::TRUNCATED_BODY),
            Some(status),
            Message::Fixed("The upstream response body was cut short."),
        )
    }

    fn timer(&self) -> Option<(Instant, TimerKind)> {
        if self.plan.stream {
            None
        } else {
            self.timers
                .fire_at(self.timers.timeouts.attempt, self.started, TimerKind::Attempt)
        }
    }

    /// The largest non-streaming upstream body this attempt reads (`max_upstream_response`).
    pub(crate) fn body_cap(&self) -> u64 {
        self.cx.snap.security.max_upstream_response
    }

    /// A body over `max_upstream_response`: 502 `upstream_bad_response`, reason `response_too_large`.
    pub(crate) fn too_large(&self, status: u16) -> Failure {
        self.fail(
            Class::MalformedResponse,
            Code::UpstreamBadResponse,
            Some(crate::reasons::RESPONSE_TOO_LARGE),
            Some(status),
            Message::Fixed("The upstream response body exceeds the gateway's size limit."),
        )
    }

    /// Reads a whole body (decoded), watching the client and the attempt timer.
    pub(crate) async fn read(&self, body: InboundBody, cap: u64) -> Result<Result<Bytes, Unread>, Failure> {
        let far = Instant::now() + Duration::from_secs(86_400 * 365);
        let (at, kind) = self.timer().unwrap_or((far, TimerKind::Attempt));
        tokio::select! {
            () = self.cx.gone.cancelled() => Err(failure::cancelled(self.deployment, self.attempts)),
            () = tokio::time::sleep_until(at) => Err(failure::timeout(self.deployment, self.attempts, kind, &self.timers)),
            r = collect(body, cap) => Ok(r),
        }
    }

    /// The failure of a whole body that could not be read.
    pub(crate) fn unread(&self, why: Unread, status: u16) -> Failure {
        match why {
            Unread::Truncated => self.truncated(status),
            Unread::Corrupt => self.bad(
                hygiene::INVALID_BODY,
                status,
                "upstream response body is not valid for its content-encoding".to_owned(),
            ),
            Unread::TooLarge => self.too_large(status),
        }
    }

    /// The failure of a 2xx body that carries an `error` object, classified by its effective status.
    pub(crate) fn error_object(&self, err: &Value) -> Failure {
        let eff = relay::events::effective_status(err);
        let class = antirion_resilience::classify::status(eff, true);
        let class = match class {
            Class::Success | Class::MalformedResponse => Class::ServerError,
            c => c,
        };
        let message = err
            .get("message")
            .and_then(Value::as_str)
            .map(|m| Message::built(scrub_for(self).apply(m)))
            .unwrap_or(Message::Fixed("The upstream reported an error."));
        self.fail(class, class.final_code(), None, Some(eff), message)
    }
}

/// What this attempt's upstream text is scrubbed of: the provider's credentials and the configured
/// internal hosts.
pub(crate) fn scrub_for(a: &Attempt<'_>) -> crate::scrub::Scrub {
    let hosts = crate::scrub::configured_hosts(
        a.cx.snap
            .providers
            .providers
            .values()
            .filter_map(|p| p.base_url.clone()),
    );
    let secrets: Vec<String> = a
        .deployment
        .provider
        .secrets
        .iter()
        .chain(&a.acquired)
        .cloned()
        .chain(crate::guard::operator_secrets(&a.cx.snap))
        .collect();
    crate::scrub::Scrub::new(&secrets, std::sync::Arc::new(hosts))
}

/// Handles the upstream's response head and body: the response and the attempt's class, or the
/// failure.
pub(crate) async fn handle(a: &Attempt<'_>, up: InboundResponse) -> Result<(Done, Class), Failure> {
    let status = up.status.as_u16();
    let class = a.cx.snap.health.classify_status(status, true);
    if class != Class::Success {
        return Err(error_response(a, up, class, true).await);
    }
    if antirion_resilience::predicate::head_matches(a.predicates, &up.headers) {
        let f = a.predicate_matched(status);
        if let Some(abort) = up.abort {
            drop(up.body);
            Box::pin(abort.abort()).await;
        }
        return Err(f);
    }
    if announced_over(&up.headers, a.body_cap()) {
        let f = a.too_large(status);
        if let Some(abort) = up.abort {
            drop(up.body);
            Box::pin(abort.abort()).await;
        }
        return Err(f);
    }
    let upstream_id = upstream_request_id(&up.headers)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    a.cx.log.update(|r| r.upstream_request_id = upstream_id);
    let api = apis::upstream_api(a.plan.endpoint, a.cell);
    match apis::stream_dialect(api, a.deployment) {
        Some(d) if a.plan.stream => {
            crate::streamed::stream(a, up, d, apis::client_dialect(a.plan.endpoint, a.cell))
                .await
                .map(|done| (done, Class::Success))
        }
        Some(_) if a.aggregate => crate::aggregate::collect(a, up, api).await,
        _ if api == apis::Api::Embeddings
            && let Some(slot) = &a.plan.cache =>
        {
            buffered(a, up, slot).await
        }
        _ if api == apis::Api::Other && (counts_units(a, &up.headers) || transcript::applies(a)) => {
            counted(a, up).await
        }
        _ if matches!(api, apis::Api::Other | apis::Api::Embeddings) => {
            a.record(Some(status), Class::Success, None);
            if a.plan.stream {
                charge_requested_images(a);
            }
            Ok((Done::Response(raw_passthrough(a, up)), Class::Success))
        }
        _ => json_response(a, up, api).await,
    }
}

/// A body that is not a stream announcing, by `Content-Length`, more than `cap` bytes.
fn announced_over(headers: &HeaderMap, cap: u64) -> bool {
    let streams = matches!(
        crate::attempt::essence(headers).as_deref(),
        Some("text/event-stream" | "application/vnd.amazon.eventstream" | "application/x-ndjson")
    );
    !streams
        && headers
            .get(http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .is_some_and(|n| n > cap)
}

pub(crate) fn response_head(a: &Attempt<'_>, resp: &mut Response<Body>, status: u16, upstream: &HeaderMap) {
    crate::egress::inbound(upstream, &a.plan.rules, resp.headers_mut(), || scrub_for(a));
    decision_headers(resp.headers_mut(), a.deployment, a.attempts, status, &a.first);
    let (client, upstream) = apis::dialect_names(a.plan.endpoint, a.cell);
    let h = resp.headers_mut();
    h.insert(
        HeaderName::from_static("x-antirion-client-dialect"),
        HeaderValue::from_static(client),
    );
    h.insert(
        HeaderName::from_static("x-antirion-upstream-dialect"),
        HeaderValue::from_static(upstream),
    );
    a.cx.finish(resp, false);
}

/// A body otherwise relayed as it arrives, read whole because the response may be cached.
async fn buffered(
    a: &Attempt<'_>,
    up: InboundResponse,
    slot: &crate::cache::CacheSlot,
) -> Result<(Done, Class), Failure> {
    let status = up.status;
    let InboundResponse { headers, body, .. } = up;
    let raw = match a.read(body, a.body_cap()).await? {
        Ok(b) => b,
        Err(why) => return Err(a.unread(why, status.as_u16())),
    };
    a.record(Some(status.as_u16()), Class::Success, None);
    let content_type = headers
        .get(CONTENT_TYPE)
        .cloned()
        .unwrap_or(HeaderValue::from_static("application/json"));
    crate::cache::store_response(a, slot, status.as_u16(), &content_type, &raw, &headers).await;
    let mut resp = Response::new(full(raw));
    *resp.status_mut() = status;
    resp.headers_mut().insert(CONTENT_TYPE, content_type);
    response_head(a, &mut resp, status.as_u16(), &headers);
    Ok((Done::Response(resp), Class::Success))
}

/// A JSON image generation or transcription answer, whose unit counts are read from it.
fn counts_units(a: &Attempt<'_>, headers: &HeaderMap) -> bool {
    let unit_endpoint = match a.plan.endpoint.dialect {
        antirion_dialects::Dialect::OpenAiImages => true,
        antirion_dialects::Dialect::OpenAiAudio => matches!(
            a.plan.endpoint.upstream_path,
            Some("/audio/transcriptions" | "/audio/translations")
        ),
        _ => false,
    };
    unit_endpoint && is_json(crate::attempt::essence(headers).as_deref())
}

/// A streamed image generation is charged the images it asked for, which is what was reserved.
fn charge_requested_images(a: &Attempt<'_>) {
    a.cx.log.update(|r| {
        if let Some((key, n)) = r.images_requested.clone() {
            r.units.images.insert(key, n);
        }
    });
}

/// Reads a unit-counted answer whole, records the images it carries or the audio duration it
/// reports, and relays it.
async fn counted(a: &Attempt<'_>, up: InboundResponse) -> Result<(Done, Class), Failure> {
    let status = up.status;
    let InboundResponse { headers, body, .. } = up;
    let raw = match a.read(body, a.body_cap()).await? {
        Ok(b) => b,
        Err(why) => return Err(a.unread(why, status.as_u16())),
    };
    let json = is_json(crate::attempt::essence(&headers).as_deref());
    let raw = transcript::checked(a, status.as_u16(), raw, json).await?;
    a.record(Some(status.as_u16()), Class::Success, None);
    if let Ok(v) = serde_json::from_slice::<Value>(&raw) {
        a.cx.log.update(|r| {
            if let Some((key, n)) = r.images_requested.clone() {
                r.units
                    .images
                    .insert(key, crate::account::units::images_in(&v, n));
            }
            if let Some(ms) = crate::account::units::duration_ms(&v) {
                r.units.audio_ms_in = ms;
            }
        });
    }
    let mut resp = Response::new(full(raw));
    *resp.status_mut() = status;
    if let Some(ct) = headers.get(CONTENT_TYPE).cloned() {
        resp.headers_mut().insert(CONTENT_TYPE, ct);
    }
    response_head(a, &mut resp, status.as_u16(), &headers);
    Ok((Done::Response(resp), Class::Success))
}

/// Binary and text bodies (audio) are relayed as they arrive, decoded when compressed.
fn raw_passthrough(a: &Attempt<'_>, up: InboundResponse) -> Response<Body> {
    let status = up.status;
    let guard = a.inflight.clone();
    let cap = a.body_cap();
    let mut seen = 0u64;
    let mut resp = Response::new(streamed(data_stream(up.body).map(move |chunk| {
        let _held = &guard;
        let chunk = chunk?;
        seen = seen.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        if seen > cap {
            return Err(BoxError::from(
                "the upstream response body exceeds max_upstream_response",
            ));
        }
        Ok(chunk)
    })));
    *resp.status_mut() = status;
    if let Some(ct) = up.headers.get(CONTENT_TYPE).cloned() {
        resp.headers_mut().insert(CONTENT_TYPE, ct);
    }
    response_head(a, &mut resp, status.as_u16(), &up.headers);
    resp
}

/// The `model` a response shows: the requested name, or with `api.response_model: upstream` the
/// value the upstream returned.
pub(crate) fn response_model(a: &Attempt<'_>, upstream: &str) -> String {
    let mode =
        a.cx.snap
            .config
            .doc
            .pointer("/api/response_model")
            .and_then(Value::as_str);
    if mode == Some("upstream") {
        upstream.to_owned()
    } else {
        a.plan.requested.clone()
    }
}

/// Unix seconds on the wall clock: response timestamps never follow the test clock.
pub(crate) fn wall_clock_secs() -> i64 {
    use antirion_core::Clock;
    antirion_core::SystemClock.now().0 / 1_000_000
}

/// The id of an upstream response without one in its body: Bedrock's request id, else a new UUIDv7
/// in lowercase hex.
pub(crate) fn upstream_or_new_id(headers: &HeaderMap) -> String {
    headers
        .get("x-amzn-requestid")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .unwrap_or_else(|| antirion_core::RequestId::generate().as_str().replace('-', ""))
}
