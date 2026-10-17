//! Stage `attempt`: one attempt sent to one position of the plan with one key, under its timers:
//! the upstream body for the position (route params, then identity or translation), the request
//! with its credential, and what came back, classified for the attempt loop.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use antirion_core::{Stage, headers as h};
use antirion_dialects::Endpoint;
use antirion_egress::Payload;
use antirion_egress::headers::HeaderRules;
use antirion_error::{GatewayError, Message};
use antirion_json::JsonLimits;
use antirion_obs::trace::SpanId;
use antirion_providers::{Authorized, DeploymentRt, UpstreamCall};
use antirion_resilience::{Attempt, Class, Failure, Sender, Sent, TimerKind};
use antirion_router::body_edit::BodyEdit;
use antirion_router::{Params, Slot};
use bytes::Bytes;
use http::header::CONTENT_TYPE;
use http::{HeaderMap, HeaderName, HeaderValue, Response};
use serde_json::Value;
use tokio::time::Instant;

use crate::cell::{Cell, cell, upstream_op};
use crate::ctx::{Body, Ctx};
use crate::outcome;
use crate::parse::Parsed;
use crate::policy::StreamPolicy;
use crate::route::{Position, Positions};
use crate::upbody::{client_anthropic, upstream_body};

/// Everything the attempts of one request share.
pub(crate) struct Plan<'a> {
    pub(crate) endpoint: &'a Endpoint,
    pub(crate) parsed: &'a Parsed,
    /// The model string the client sent (or the default it got), for response bodies.
    pub(crate) requested: String,
    /// The body had no `model` member (a default model was used).
    pub(crate) model_absent: bool,
    pub(crate) stream: bool,
    pub(crate) client_wants_usage: bool,
    pub(crate) stream_policy: StreamPolicy,
    pub(crate) client_content_type: Option<HeaderValue>,
    /// The client's headers, some of which the header rules forward.
    pub(crate) client_headers: &'a HeaderMap,
    pub(crate) rules: HeaderRules,
    pub(crate) input_estimate: u64,
    pub(crate) cache: Option<crate::cache::CacheSlot>,
    /// The prompt-cache breakpoints attempts on supporting deployments get.
    pub(crate) prompt_cache: Option<antirion_cache::PromptCaching>,
    /// The `prompt_cache_key` attempts to OpenAI deployments carry.
    pub(crate) openai_cache_key: Option<String>,
    /// The Responses state of a Responses request.
    pub(crate) turn: Option<crate::respstate::Turn>,
    /// The decoded request, when some candidate is a translated cell.
    pub(crate) xlate: Option<Arc<crate::xlate::Xlate>>,
    /// The request's guardrails, when any applies.
    pub(crate) guard: Option<Arc<crate::guard::RequestGuard>>,
    /// The request's extensions, when any is attached.
    pub(crate) hooks: Option<Arc<crate::extend::Hooks>>,
    /// Upstream headers extensions set, sent on every attempt.
    pub(crate) extra_headers: Vec<(HeaderName, HeaderValue)>,
    /// The health verdict of the stream after its commit.
    pub(crate) post_commit: crate::postcommit::PostCommit,
    /// The provider credential the client brought, when its key may use one.
    pub(crate) client_keys: Option<crate::byok::ClientKeys>,
    /// A Gemini stream request without `alt=sse`: the client reads a JSON array.
    pub(crate) json_array: bool,
    /// The `n` attempts on deployments that cannot honour it fan out to.
    pub(crate) fan_out: Option<u32>,
}

/// What a finished request produced.
pub(crate) enum Done {
    Response(Response<Body>),
    /// Replayed from the cache, without an attempt.
    Cached(Response<Body>),
    /// The client went away; nothing is sent.
    Cancelled,
}

/// Sends attempts upstream for the attempt loop.
pub(crate) struct Upstream<'a> {
    pub(crate) cx: &'a Ctx,
    pub(crate) plan: &'a Plan<'a>,
    pub(crate) positions: Positions,
    pub(crate) slot: Slot,
    /// The decay of latency estimates.
    pub(crate) decay: Duration,
    /// Deployments whose last attempt was refused before processing: the retry takes a new
    /// connection.
    pub(crate) refused: std::sync::Mutex<HashSet<String>>,
    /// A hedge was sent: the request's attempts ran in a race.
    pub(crate) raced: std::sync::atomic::AtomicBool,
    /// The request's planned copies, started with its first attempt.
    pub(crate) mirrors: crate::mirror::Jobs,
    /// The Models the plan may move to; a refusal becomes a failure while a class fallback is
    /// still to come.
    pub(crate) fallbacks: &'a crate::fallbacks::Fallbacks,
    /// The route's tree, for the canaries above an attempted position.
    pub(crate) tree: &'a antirion_router::CompiledRoute,
    /// Some weighted node of the route has a canary.
    pub(crate) canaries: bool,
}

impl Sender for Upstream<'_> {
    type Done = Done;

    async fn send(&self, a: Attempt<'_>) -> Sent<Done> {
        let position = self
            .positions
            .lock()
            .ok()
            .and_then(|m| m.get(&a.selected.deployment).cloned());
        let Some(position) = position else {
            return Sent::Failed(Failure {
                class: Class::EgressDenied,
                status: None,
                timer: None,
                hint: None,
                error: Box::new(GatewayError::new(
                    antirion_error::Code::Internal,
                    Stage::Attempt,
                    Message::Fixed("The attempt has no plan position."),
                )),
            });
        };
        if a.index == 0 {
            crate::mirror::launch(self, &a.timers).await;
        }
        if a.hedge {
            self.raced.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let started = Instant::now();
        let fresh = self
            .refused
            .lock()
            .is_ok_and(|r| r.contains(&position.deployment.name));
        let refusal_fails = a
            .selected
            .policy
            .retry
            .class_fallback(Class::ContentPolicy)
            .is_some_and(|m| !self.fallbacks.taken() && self.fallbacks.class(m).is_some());
        let result = one(self, &a, &position, fresh, refusal_fails).await;
        if let Ok(mut refused) = self.refused.lock() {
            match &result {
                Sent::Failed(f) if f.class == Class::RefusedStream => {
                    refused.insert(position.deployment.name.clone());
                }
                _ => {
                    refused.remove(&position.deployment.name);
                }
            }
        }
        self.observe(&position.deployment.name, &result, started);
        if self.canaries && position.segment == 0 {
            self.canary_outcome(position.node, &result);
        }
        result
    }

    fn committed(&mut self, c: antirion_resilience::Committed) {
        self.plan.post_commit.committed(&self.cx.rt, c);
    }
}

impl Upstream<'_> {
    /// Counts an attempt on a canary's candidate against its error budget, and reports a budget
    /// that tripped.
    fn canary_outcome(&self, node: usize, result: &Sent<Done>) {
        let ok = match result {
            Sent::Success(_) => true,
            Sent::Failed(f) if attributable(f) => false,
            Sent::Failed(_) => return,
        };
        let chain = self.tree.ancestry(node);
        for pair in chain.windows(2) {
            let [parent, child] = pair else { continue };
            let Some(n) = self.tree.node(*parent) else {
                continue;
            };
            let antirion_router::NodeKind::Weighted { canary: Some(c), .. } = &n.kind else {
                continue;
            };
            if n.children.get(c.child) != Some(child) {
                continue;
            }
            let rt = &self.cx.rt;
            if let Some(t) = rt.state.routing.canaries.record(c, ok, rt.now()) {
                let kind = if t.rolled_back {
                    "routing.canary_rolled_back"
                } else {
                    "routing.canary_held"
                };
                let candidate = self.tree.node(*child).and_then(|x| x.segment.clone());
                rt.send_alert(
                    &self.cx.snap,
                    None,
                    kind,
                    serde_json::json!({
                        "route": self.tree.name,
                        "node": n.path,
                        "child": candidate,
                        "weight": t.weight,
                        "error_rate": t.error_rate,
                    }),
                );
            }
        }
    }

    /// Latency and success samples for the routing statistics.
    fn observe(&self, d: &str, result: &Sent<Done>, started: Instant) {
        let state = &self.cx.rt.state.routing;
        let now = std::time::Instant::now();
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        match result {
            Sent::Success(_) => {
                let sample = match self.slot {
                    Slot::PerToken => self
                        .cx
                        .log
                        .read(|r| r.usage.output)
                        .flatten()
                        .filter(|o| *o > 0)
                        .map(|o| elapsed_ms / o as f64),
                    _ => Some(elapsed_ms),
                };
                if let Some(ms) = sample {
                    state.observe(d, self.slot, ms, self.decay, now);
                }
                state.outcome(d, true, now);
            }
            Sent::Failed(f) => {
                if f.class == Class::Timeout && f.timer != Some(TimerKind::Deadline) {
                    state.observe(d, self.slot, elapsed_ms, self.decay, now);
                }
                if attributable(f) {
                    state.outcome(d, false, now);
                }
            }
        }
    }
}

/// A failed attempt the deployment is to blame for (not the caller, not the request's deadline).
fn attributable(f: &Failure) -> bool {
    matches!(
        f.error.code,
        antirion_error::Code::UpstreamUnavailable
            | antirion_error::Code::UpstreamTimeout
            | antirion_error::Code::UpstreamRateLimited
            | antirion_error::Code::UpstreamAuthFailed
            | antirion_error::Code::UpstreamBadResponse
    ) && f.timer != Some(TimerKind::Deadline)
}

/// The final upstream body with a leaf's `upstream_body` edits applied; a body that is not JSON is
/// sent as it is.
pub(crate) fn edited_body(body: Payload, edit: &BodyEdit) -> Payload {
    match serde_json::from_slice::<Value>(&body.contiguous()) {
        Ok(mut v) => {
            edit.apply(&mut v);
            serde_json::to_vec(&v).map_or(body, Payload::from)
        }
        Err(_) => body,
    }
}

/// The client's JSON body with the position's route params applied, when any applies.
pub(crate) fn with_params(parsed: &Parsed, params: &[Params]) -> Option<Parsed> {
    if params.is_empty() {
        return None;
    }
    let mut v: Value = parsed.doc()?.clone();
    for p in params {
        p.apply(&mut v);
    }
    let raw = Bytes::from(serde_json::to_vec(&v).ok()?);
    let skim = antirion_json::skim(&raw, &JsonLimits::default()).ok()?;
    Some(Parsed::json(raw, skim, Some(v)))
}

/// One attempt on `d`: its upstream body (route params applied, then identity or a translation
/// encoded again for this deployment), then the request.
async fn one(
    up: &Upstream<'_>,
    a: &Attempt<'_>,
    position: &Position,
    fresh: bool,
    refusal_fails: bool,
) -> Sent<Done> {
    let d = &position.deployment;
    let upload = match up.plan.parsed {
        Parsed::Multipart { rest: Some(r), .. } => Some(r),
        _ => None,
    };
    if let Some(r) = upload.filter(|r| r.spent()) {
        return Sent::Failed(Failure {
            class: Class::CallerError,
            status: None,
            timer: None,
            hint: None,
            error: Box::new(r.earlier_error()),
        });
    }
    let mut edited = with_params(up.plan.parsed, &position.params);
    if a.repair
        && let Some(r) = crate::reasoning::repaired(edited.as_ref().unwrap_or(up.plan.parsed))
    {
        edited = Some(r);
    }
    let parsed = edited.as_ref().unwrap_or(up.plan.parsed);
    let cell = cell(up.plan.endpoint, parsed, d).unwrap_or(Cell::Identity);
    let mut result = if matches!(cell, Cell::Embed(_) | Cell::Rerank(_)) {
        crate::embed::send(up.cx, up.plan, a, position, parsed, cell).await
    } else {
        send(
            up,
            a,
            position,
            parsed,
            cell,
            edited.is_some(),
            fresh,
            refusal_fails,
        )
        .await
    };
    if let (Sent::Failed(f), Some(r)) = (&mut result, upload) {
        if let Some(e) = r.broken() {
            f.class = Class::CallerError;
            *f.error = e;
        }
        if r.spent() {
            r.sent_with(&f.error);
        }
    }
    if let Sent::Failed(f) = &mut result
        && f.class != Class::ClientCancelled
    {
        let (_, upstream) = crate::apis::dialect_names(up.plan.endpoint, cell);
        if let Ok(v) = HeaderValue::from_str(upstream) {
            f.error
                .extra_headers
                .get_or_insert_with(Box::default)
                .insert(HeaderName::from_static("x-antirion-upstream-dialect"), v);
        }
    }
    result
}

/// Sends one attempt's request and handles what comes back. A token credential the upstream
/// rejects with 401 is refreshed and the request sent once more with the new token. With
/// `refusal_fails`, a refusal before the commit is the attempt's failure (a class fallback follows).
#[allow(clippy::too_many_arguments)]
async fn send(
    up: &Upstream<'_>,
    a: &Attempt<'_>,
    position: &Position,
    parsed: &Parsed,
    cell: Cell,
    edited: bool,
    fresh: bool,
    refusal_fails: bool,
) -> Sent<Done> {
    let cx = up.cx;
    let plan = up.plan;
    let d = &position.deployment;
    let fan = crate::fanout::calls(plan, d, cell);
    let fanned = fan.and_then(|_| crate::fanout::without_n(parsed));
    let (parsed, edited) = match &fanned {
        Some(p) => (p, true),
        None => (parsed, edited),
    };
    let attempts = u16::try_from(a.index + 1).unwrap_or(u16::MAX);
    let started_at = cx.rt.now();
    let started = Instant::now();
    let mut att = outcome::Attempt {
        cx,
        plan,
        deployment: d,
        cell,
        attempts,
        started,
        started_at,
        key: a.key.map(str::to_owned),
        wait: a.wait,
        via: a.via.map(str::to_owned),
        first: a.first.to_owned(),
        timers: a.timers.clone(),
        inflight: Some(Arc::new(match &position.pool {
            Some(p) => cx.rt.state.routing.begin_in_pool(&d.name, p),
            None => cx.rt.state.routing.begin(&d.name),
        })),
        acquired: Vec::new(),
        span: SpanId::random(),
        server: None,
        xlate: None,
        refusal_fails,
        byok: None,
        aggregate: crate::aggregate::applies(plan, cell),
        predicates: &a.selected.policy.retry.on_response,
        race: a.hedged.then(|| crate::outcome::Race::new(a.hedge)),
    };
    let aggregate = att.aggregate;
    if !a.wait.is_zero() {
        let now = std::time::Instant::now();
        cx.log.update(|r| {
            let from = r
                .attempts
                .last()
                .map_or_else(|| now.checked_sub(a.wait).unwrap_or(now), |l| l.end);
            r.waits.push((from, now));
        });
    }
    let (body, content_type) = match upstream_body(cx, plan, parsed, d, cell, edited, a.repair).await {
        Ok((body, content_type, xlate)) => {
            att.xlate = xlate;
            let body = if aggregate {
                crate::aggregate::stream_body(body, crate::apis::upstream_api(plan.endpoint, cell), d)
            } else {
                body
            };
            let body = crate::cache::prompt::breakpoints(plan, d, body);
            let body = crate::cache::prompt::openai_cache_key(plan, d, body);
            match &position.body_edit {
                Some(edit) => (edited_body(body, edit), content_type),
                None => (body, content_type),
            }
        }
        Err(e) => {
            let mut f = crate::failure::of_class(d, attempts, Class::CallerError, None, e.message.clone());
            *f.error = e;
            att.record(None, Class::CallerError, None);
            return Sent::Failed(f);
        }
    };
    let (body, hook_headers) = match plan.hooks.as_deref().filter(|h| crate::extend::attempt::any(h)) {
        None => (body, Vec::new()),
        Some(h) => match crate::extend::attempt::run(cx, h, a.index, d, body, content_type.as_ref()).await {
            Ok(edited) => (edited.body, edited.headers),
            Err(mut e) => {
                e.attempts = attempts;
                return Sent::Failed(crate::streamed::guarded(e.status(), e));
            }
        },
    };
    let timers = &a.timers;
    let connect = match timers.deadline_at {
        Some(at) => timers.connect.min(at.saturating_duration_since(started)),
        None => timers.connect,
    };
    let head_timer = if plan.stream || aggregate {
        let headers = timers.fire_at(
            timers.timeouts.response_headers,
            started,
            TimerKind::ResponseHeaders,
        );
        let ttft = timers.fire_at(
            timers.timeouts.ttft_with_hint(d.capabilities.ttft_hint()),
            started,
            TimerKind::Ttft,
        );
        match (headers, ttft) {
            (Some(h), Some(t)) if t.0 < h.0 => Some(t),
            (h, t) => h.or(t),
        }
    } else {
        timers.fire_at(timers.timeouts.attempt, started, TimerKind::Attempt)
    };
    let far = started + Duration::from_secs(86_400 * 365);
    let (fire_at, fire_kind) = head_timer.unwrap_or((far, TimerKind::Attempt));
    let anthropic = client_anthropic(plan);
    let byok = plan
        .client_keys
        .as_ref()
        .and_then(|k| k.for_deployment(d))
        .filter(|_| d.byok.accepts());
    att.byok = byok.clone();
    let mut refreshed = false;
    loop {
        att.span = SpanId::random();
        let prepared = d.build_request(UpstreamCall {
            op: upstream_op(plan.endpoint, cell),
            stream: plan.stream || aggregate,
            body: body.clone(),
            content_type: content_type.clone(),
            request_id: &cx.request_id,
            response_timeout: Duration::from_secs(86_400 * 365),
            connect_timeout: Some(connect.max(Duration::from_millis(1))),
            anthropic: anthropic.as_ref(),
            key: a.key,
            byok: byok.as_deref(),
        });
        let mut request = match prepared {
            Ok(p) => {
                if !p.applied.is_empty() {
                    let applied = p.applied.clone();
                    cx.log.update(|r| r.params_applied = Some(applied));
                }
                let mut r = p.request;
                att.server = r.uri.host().map(|h| {
                    let port = r
                        .uri
                        .port_u16()
                        .unwrap_or(if r.uri.scheme_str() == Some("https") {
                            443
                        } else {
                            80
                        });
                    (h.trim_matches(['[', ']']).to_owned(), port)
                });
                r.fresh_connection = fresh;
                r
            }
            Err(_) => {
                att.record(None, Class::ConnectError, None);
                return Sent::Failed(crate::failure::of_class(
                    d,
                    attempts,
                    Class::ConnectError,
                    None,
                    Message::Fixed("The upstream request could not be built."),
                ));
            }
        };
        crate::egress::outbound(
            cx,
            plan.client_headers,
            &plan.rules,
            plan.stream || aggregate,
            &mut request.headers,
        );
        for (n, v) in plan.extra_headers.iter().chain(&hook_headers) {
            request.headers.insert(n.clone(), v.clone());
        }
        cx.trace.propagate(&mut request.headers, &att.span);
        let cred_tls = cx
            .snap
            .security
            .egress_tls
            .clone()
            .unwrap_or_else(|| d.provider.tls.clone());
        let authorized = tokio::select! {
            () = cx.gone.cancelled() => {
                att.record(None, Class::ClientCancelled, None);
                return Sent::Failed(crate::failure::cancelled(d, attempts));
            }
            () = tokio::time::sleep_until(fire_at) => {
                att.record(None, Class::Timeout, Some(fire_kind));
                return Sent::Failed(crate::failure::timeout(d, attempts, fire_kind, timers));
            }
            r = d.authorize(&mut request, &cx.rt.creds, &cred_tls) => r,
        };
        let authorized = match authorized {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!(provider = %d.provider.name, error = %e, "upstream credential acquisition failed");
                att.record(None, Class::CredentialRejected, None);
                return Sent::Failed(crate::failure::fail(
                    d,
                    attempts,
                    Class::CredentialRejected,
                    antirion_error::Code::UpstreamAuthFailed,
                    Some(crate::reasons::CREDENTIAL_ACQUISITION_FAILED),
                    None,
                    Message::built(format!(
                        "The gateway could not obtain a credential for provider `{}`.",
                        d.provider.name
                    )),
                ));
            }
        };
        att.acquired = authorized.secrets();
        att.acquired.extend(byok.iter().cloned());
        if let Some(n) = fan {
            return Box::pin(crate::fanout::exchange(att, request, n, fire_at, fire_kind)).await;
        }
        let detached = att.race.is_some() && !(plan.stream || aggregate);
        let sent = if detached {
            detached_exchange(&att, request, fire_at).await
        } else {
            tokio::select! {
                () = cx.gone.cancelled() => Err(Gone::Client),
                () = tokio::time::sleep_until(fire_at) => Err(Gone::Timer),
                r = cx.rt.egress.send(request) => r.map_err(|t| Gone::Failed(crate::failure::transport(d, attempts, &t, timers))),
            }
        };
        let upstream = match sent {
            Ok(u) => u,
            Err(Gone::Client) => {
                att.record(None, Class::ClientCancelled, None);
                return Sent::Failed(crate::failure::cancelled(d, attempts));
            }
            Err(Gone::Timer) => {
                att.record(None, Class::Timeout, Some(fire_kind));
                return Sent::Failed(crate::failure::timeout(d, attempts, fire_kind, timers));
            }
            Err(Gone::Failed(f)) => {
                att.record_failure(None, &f);
                return Sent::Failed(f);
            }
        };
        let status = upstream.status.as_u16();
        cx.rt.state.headroom.observe(
            &d.provider.name,
            a.key.unwrap_or(antirion_providers::SINGLE_KEY),
            &upstream.headers,
            cx.rt.now(),
        );
        if status == 401
            && let Authorized::Token { source, token } = &authorized
        {
            if refreshed {
                cx.rt.creds.mark_dead(source);
            } else {
                cx.rt.creds.invalidate(source, token);
                refreshed = true;
                att.record(Some(status), Class::CredentialRejected, None);
                continue;
            }
        }
        let mut result = outcome::handle(&att, upstream).await;
        if let (Err(f), Some(x)) = (&mut result, &att.xlate) {
            let entries = x.warnings.request_entries();
            if let Some(v) = antirion_dialects::xlate::warn::header_value(&entries)
                .and_then(|v| HeaderValue::from_str(&v).ok())
            {
                f.error
                    .extra_headers
                    .get_or_insert_with(Box::default)
                    .insert(HeaderName::from_static("x-antirion-translation-warnings"), v);
            }
            cx.log.update(|r| r.translation_warnings = entries);
        }
        return match result {
            Ok((done, _)) => Sent::Success(done),
            Err(mut f) => {
                let restored = plan.xlate.as_ref().is_some_and(|x| x.restored);
                if f.class == Class::ReasoningReplayRejected
                    && !restored
                    && !crate::reasoning::carries(parsed)
                {
                    f.class = Class::CallerError;
                    if let Some(u) = f.error.upstream.as_mut() {
                        u.failure_class = Class::CallerError.as_str();
                    }
                }
                att.record_failure(f.status.or(Some(status)), &f);
                Sent::Failed(f)
            }
        };
    }
}

/// Why an attempt has no upstream response to read.
enum Gone {
    /// The client went away.
    Client,
    /// The attempt's head timer fired.
    Timer,
    Failed(Failure),
}

/// One raced attempt's exchange on a task of its own (a complete response), its answer read whole:
/// an attempt that loses the race is dropped while it waits here, and its exchange goes on for
/// the grace of a tail.
async fn detached_exchange(
    att: &outcome::Attempt<'_>,
    request: antirion_egress::OutboundRequest,
    fire_at: Instant,
) -> Result<antirion_egress::InboundResponse, Gone> {
    let exchange = crate::hedge_tail::Exchange::start(&att.cx.rt, request, att.body_cap());
    if let Some(race) = &att.race
        && let Ok(mut slot) = race.exchange.lock()
    {
        *slot = Some(exchange.clone());
    }
    let answer = tokio::select! {
        () = att.cx.gone.cancelled() => {
            exchange.abort();
            return Err(Gone::Client);
        }
        () = tokio::time::sleep_until(fire_at) => {
            exchange.abort();
            return Err(Gone::Timer);
        }
        a = exchange.answer() => a,
    };
    match answer.as_deref() {
        Some(Ok((status, headers, Ok(body)))) => Ok(crate::hedge_tail::response(*status, headers, body)),
        Some(Ok((status, _, Err(why)))) => Err(Gone::Failed(att.unread(*why, status.as_u16()))),
        Some(Err(t)) => Err(Gone::Failed(crate::failure::transport(
            att.deployment,
            att.attempts,
            t,
            &att.timers,
        ))),
        None => Err(Gone::Failed(crate::failure::of_class(
            att.deployment,
            att.attempts,
            Class::UpstreamReset,
            None,
            Message::Fixed("The upstream exchange ended without an answer."),
        ))),
    }
}

/// A refusal that moves the request to its content-policy fallback. It reaches the client only when
/// that Model has no position to try.
pub(crate) fn refused(d: &DeploymentRt, attempts: u16) -> antirion_resilience::Failure {
    crate::failure::fail(
        d,
        attempts,
        Class::ContentPolicy,
        antirion_error::Code::UpstreamRejected,
        Some(crate::reasons::CONTENT_POLICY_VIOLATION),
        Some(400),
        Message::Fixed("The upstream refused the request."),
    )
}

/// The decision headers of a response with an upstream.
pub(crate) fn decision_headers(
    headers: &mut HeaderMap,
    deployment: &DeploymentRt,
    attempts: u16,
    status: u16,
    first: &str,
) {
    let mut set = |name: &'static str, value: &str| {
        if let Ok(v) = HeaderValue::from_str(value) {
            headers.insert(HeaderName::from_static(name), v);
        }
    };
    set(
        h::RESPONSE_PATH,
        if deployment.name == first {
            "upstream"
        } else {
            "fallback"
        },
    );
    set(h::PROVIDER, &deployment.provider.name);
    set(h::MODEL, &deployment.model);
    set(h::DEPLOYMENT, &deployment.name);
    set(h::ATTEMPTS, &attempts.to_string());
    set(h::UPSTREAM_STATUS, &status.to_string());
}

/// The content type of an upstream response, lowercased without parameters.
pub(crate) fn essence(headers: &HeaderMap) -> Option<String> {
    headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(|ct| {
        ct.split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
    })
}
