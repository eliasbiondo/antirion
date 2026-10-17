//! A call on a provider-scoped object (a file or a batch) through the attempt loop: sent to one
//! deployment of the provider that holds the object, retried as its request kind allows (a read is
//! resource-bound, a create, cancel or delete side-effecting), the answer read whole or relayed.

use std::sync::Arc;
use std::time::Duration;

use antirion_dialects::Endpoint;
use antirion_egress::{InboundBody, Payload};
use antirion_error::{Code, GatewayError, Message};
use antirion_json::JsonLimits;
use antirion_providers::{DeploymentRt, UpstreamCall, UpstreamOp};
use antirion_resilience::{
    Attempt, Class, Failure, KeyMode, KeyPick, LoopCx, Outcome, Planner, Policy, RequestKind, Selected,
    Sender, Sent, TimerKind,
};
use antirion_runtime::identity::KeyRt;
use bytes::Bytes;
use http::request::Parts;
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use serde_json::Value;
use tokio::time::Instant;

use crate::attempt::Plan;
use crate::cell::Cell;
use crate::ctx::Ctx;
use crate::parse::Parsed;
use crate::policy::StreamPolicy;
use crate::upload::Upload;
use crate::{failure, outcome, route};

/// What one provider call sends.
pub(crate) struct Call<'a> {
    pub(crate) method: Method,
    /// The operation class (`files`, `batches`) and the path under the provider's base.
    pub(crate) class: &'a str,
    pub(crate) path: String,
    pub(crate) body: Payload,
    pub(crate) content_type: Option<HeaderValue>,
    pub(crate) kind: RequestKind,
    /// Read the answer whole (JSON), or hand its body over as it arrives.
    pub(crate) whole: bool,
    /// The streamed rest of an upload the body carries.
    pub(crate) upload: Option<Upload>,
}

/// A 2xx answer.
pub(crate) struct Answered {
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    pub(crate) body: AnswerBody,
}

pub(crate) enum AnswerBody {
    Whole(Bytes),
    Streamed(InboundBody),
}

/// The deployment a call to `provider` goes through: the first, by name, that the key may use
/// and that declares the operation class.
pub(crate) fn carrier(cx: &Ctx, key: &KeyRt, provider: &str, class: &str) -> Option<Arc<DeploymentRt>> {
    let levels = cx.snap.identity.access_levels(key);
    let mut found: Vec<&Arc<DeploymentRt>> = cx
        .snap
        .providers
        .deployments
        .values()
        .filter(|d| {
            d.provider.name == provider
                && d.usable()
                && d.capabilities.operation(class)
                && crate::authorize::deployment_allowed(&levels, d)
        })
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found.first().map(|d| Arc::clone(d))
}

/// The one candidate of a call.
struct Owner<'a> {
    cx: &'a Ctx,
    deployment: Arc<DeploymentRt>,
    policy: Arc<Policy>,
    offered: bool,
}

impl Planner for Owner<'_> {
    fn select(&mut self, tried: &[String]) -> Option<Selected> {
        if self.offered || tried.contains(&self.deployment.name) {
            return None;
        }
        self.offered = true;
        Some(Selected {
            deployment: self.deployment.name.clone(),
            provider: self.deployment.provider.name.clone(),
            via: None,
            policy: Arc::clone(&self.policy),
        })
    }

    fn next_tier(&mut self) -> bool {
        false
    }

    fn attempted(&mut self, _: &Selected) {}

    fn select_key(&mut self, _: &Selected, tried: &[String], last: Option<&str>, mode: KeyMode) -> KeyPick {
        route::pick_key(self.cx, &self.deployment, tried, last, mode)
    }

    fn skip(&mut self, _: &Selected, _: &'static str) {}

    async fn gate(&mut self, _: &Selected) -> Result<(), &'static str> {
        Ok(())
    }
}

struct Proxy<'a> {
    cx: &'a Ctx,
    plan: &'a Plan<'a>,
    deployment: Arc<DeploymentRt>,
    call: &'a Call<'a>,
}

impl Proxy<'_> {
    fn attempt(&self, a: &Attempt<'_>) -> outcome::Attempt<'_> {
        outcome::Attempt {
            cx: self.cx,
            plan: self.plan,
            deployment: &self.deployment,
            cell: Cell::Identity,
            attempts: u16::try_from(a.index + 1).unwrap_or(u16::MAX),
            started: Instant::now(),
            started_at: self.cx.rt.now(),
            key: a.key.map(str::to_owned),
            wait: a.wait,
            via: None,
            first: a.first.to_owned(),
            timers: a.timers.clone(),
            inflight: None,
            acquired: Vec::new(),
            span: antirion_obs::trace::SpanId::random(),
            server: None,
            xlate: None,
            refusal_fails: false,
            byok: None,
            aggregate: false,
            predicates: &[],
            race: None,
        }
    }

    async fn once(&self, a: &Attempt<'_>, att: &mut outcome::Attempt<'_>) -> Result<Answered, Failure> {
        let cx = self.cx;
        let d = &self.deployment;
        let attempts = att.attempts;
        if let Some(u) = self.call.upload.as_ref().filter(|u| u.spent()) {
            return Err(final_failure(u.earlier_error()));
        }
        let timers = &a.timers;
        let prepared = d
            .build_request(UpstreamCall {
                op: UpstreamOp::OpenAi {
                    class: self.call.class,
                    path: &self.call.path,
                },
                stream: false,
                body: self.call.body.clone(),
                content_type: self.call.content_type.clone(),
                request_id: &cx.request_id,
                response_timeout: Duration::from_secs(86_400 * 365),
                connect_timeout: Some(timers.connect.max(Duration::from_millis(1))),
                anthropic: None,
                key: a.key,
                byok: None,
            })
            .map_err(|_| {
                failure::of_class(
                    d,
                    attempts,
                    Class::ConnectError,
                    None,
                    Message::Fixed("The upstream request could not be built."),
                )
            })?;
        let mut request = prepared.request;
        request.method = self.call.method.clone();
        request.resend_when_closed = false;
        crate::egress::outbound(
            cx,
            self.plan.client_headers,
            &self.plan.rules,
            false,
            &mut request.headers,
        );
        if self.call.content_type.is_none() {
            request.headers.remove(http::header::CONTENT_TYPE);
        }
        cx.trace.propagate(&mut request.headers, &att.span);
        let tls = cx
            .snap
            .security
            .egress_tls
            .clone()
            .unwrap_or_else(|| d.provider.tls.clone());
        match d.authorize(&mut request, &cx.rt.creds, &tls).await {
            Ok(auth) => att.acquired = auth.secrets(),
            Err(_) => {
                return Err(failure::fail(
                    d,
                    attempts,
                    Class::CredentialRejected,
                    Code::UpstreamAuthFailed,
                    Some(crate::reasons::CREDENTIAL_ACQUISITION_FAILED),
                    None,
                    Message::built(format!(
                        "The gateway could not obtain a credential for provider `{}`.",
                        d.provider.name
                    )),
                ));
            }
        }
        let far = att.started + Duration::from_secs(86_400 * 365);
        let (fire_at, fire_kind) = timers
            .fire_at(timers.timeouts.attempt, att.started, TimerKind::Attempt)
            .unwrap_or((far, TimerKind::Attempt));
        let sent = tokio::select! {
            () = cx.gone.cancelled() => return Err(failure::cancelled(d, attempts)),
            () = tokio::time::sleep_until(fire_at) => return Err(failure::timeout(d, attempts, fire_kind, timers)),
            r = cx.rt.egress.send(request) => r,
        };
        let upstream = sent.map_err(|t| failure::transport(d, attempts, &t, timers))?;
        let status = upstream.status;
        let class = antirion_resilience::classify::status(status.as_u16(), false);
        if class != Class::Success {
            return Err(outcome::error_response(att, upstream, class, false).await);
        }
        let headers = upstream.headers.clone();
        let body = if self.call.whole {
            match att.read(upstream.body, att.body_cap()).await? {
                Ok(b) => AnswerBody::Whole(b),
                Err(why) => return Err(att.unread(why, status.as_u16())),
            }
        } else {
            AnswerBody::Streamed(upstream.body)
        };
        Ok(Answered {
            status,
            headers,
            body,
        })
    }
}

/// A failure the loop does not retry, carrying `e` to the client.
fn final_failure(e: GatewayError) -> Failure {
    Failure {
        class: Class::CallerError,
        status: None,
        timer: None,
        hint: None,
        error: Box::new(e),
    }
}

impl Sender for Proxy<'_> {
    type Done = Answered;

    async fn send(&self, a: Attempt<'_>) -> Sent<Answered> {
        let mut att = self.attempt(&a);
        match self.once(&a, &mut att).await {
            Ok(done) => {
                att.record(Some(done.status.as_u16()), Class::Success, None);
                Sent::Success(done)
            }
            Err(mut f) => {
                if let Some(u) = &self.call.upload {
                    if let Some(e) = u.broken() {
                        f = final_failure(e);
                    }
                    if u.spent() {
                        u.sent_with(&f.error);
                    }
                }
                att.record_failure(f.status, &f);
                Sent::Failed(f)
            }
        }
    }
}

/// An empty JSON body for the plan of a call that carries no inference request.
fn empty_parsed() -> Option<Parsed> {
    let raw = Bytes::from_static(b"{}");
    let skim = antirion_json::skim(&raw, &JsonLimits::default()).ok()?;
    Some(Parsed::json(
        raw,
        skim,
        Some(Value::Object(serde_json::Map::new())),
    ))
}

/// Sends `call` to `deployment` through the attempt loop; the 2xx answer, or the client's error.
pub(crate) async fn send(
    cx: &Ctx,
    parts: &Parts,
    endpoint: &Endpoint,
    key: &KeyRt,
    deployment: Arc<DeploymentRt>,
    call: &Call<'_>,
) -> Result<Answered, GatewayError> {
    let Some(parsed) = empty_parsed() else {
        return Err(GatewayError::new(
            Code::Internal,
            antirion_core::Stage::Attempt,
            Message::Fixed("The call could not be prepared."),
        ));
    };
    let snap = Arc::clone(&cx.snap);
    let plan = Plan {
        endpoint,
        parsed: &parsed,
        requested: String::new(),
        model_absent: false,
        stream: false,
        client_wants_usage: false,
        stream_policy: StreamPolicy::new(&snap, &Value::Null),
        client_content_type: None,
        client_headers: &parts.headers,
        rules: crate::egress::rules(cx, key, None, ""),
        input_estimate: 0,
        cache: None,
        prompt_cache: None,
        openai_cache_key: None,
        turn: None,
        xlate: None,
        guard: None,
        hooks: None,
        extra_headers: Vec::new(),
        post_commit: crate::postcommit::PostCommit::default(),
        client_keys: None,
        json_array: false,
        fan_out: None,
    };
    let resilience = crate::policy::effective(&snap, "resilience", key, "", None);
    let policy = Policy::from_value(&resilience);
    let (max_attempts, deadline) = (policy.retry.max_attempts, policy.timeouts.deadline);
    let deadline = match (deadline, crate::controls::read_timeout(&parts.headers)) {
        (Some(d), Some(r)) => Some(d.min(r)),
        (d, r) => d.or(r),
    };
    let min_attempt_time = policy.retry.min_attempt_time;
    cx.log.update(|r| r.routed = true);
    let mut owner = Owner {
        cx,
        deployment: Arc::clone(&deployment),
        policy: Arc::new(policy),
        offered: false,
    };
    let mut proxy = Proxy {
        cx,
        plan: &plan,
        deployment,
        call,
    };
    let seed = cx.rt.with_rng(|r| r.next_u64());
    let outcome = antirion_resilience::run(
        &mut owner,
        &mut proxy,
        LoopCx {
            health: &cx.rt.state.health,
            health_cfg: &snap.health,
            clock: cx.rt.clock.as_ref(),
            cancel: &cx.gone,
            kind: call.kind,
            max_attempts,
            min_attempt_time,
            deadline,
            fallback: false,
            seed,
            read_timeout: None,
            ignore_health: &[],
        },
    )
    .await;
    match outcome {
        Outcome::Success(done) => Ok(done),
        Outcome::Failed(f) => Err(*f.error),
        Outcome::NoCandidate => Err(crate::respstate::affinity_unavailable()),
        Outcome::Cancelled => Err(GatewayError::new(
            Code::Internal,
            antirion_core::Stage::Attempt,
            Message::Fixed("The client went away."),
        )),
    }
}
