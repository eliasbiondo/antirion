//! A native stored Responses object (`GET` and `DELETE /v1/responses/{id}`) proxied to the
//! deployment that created it, through the attempt loop: a read is resource-bound (retried only on
//! its owner), a delete is side-effecting (re-sent only after a failure before sending).

use std::sync::Arc;
use std::time::Duration;

use antirion_dialects::Endpoint;
use antirion_error::{Code, Message};
use antirion_json::JsonLimits;
use antirion_providers::{DeploymentRt, UpstreamCall, UpstreamOp};
use antirion_resilience::{
    Attempt, Class, KeyMode, KeyPick, LoopCx, Outcome, Planner, Policy, RequestKind, Selected, Sender, Sent,
    TimerKind,
};
use antirion_runtime::identity::KeyRt;
use bytes::Bytes;
use http::request::Parts;
use http::{HeaderValue, Method, Response, StatusCode};
use serde_json::Value;
use tokio::time::Instant;

use crate::attempt::{Plan, decision_headers};
use crate::cell::Cell;
use crate::ctx::{Body, Ctx, full};
use crate::parse::Parsed;
use crate::policy::StreamPolicy;
use crate::{failure, outcome, route};

/// The one candidate of the request: the owning deployment.
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

/// Sends the read or delete to the owner.
struct Proxy<'a> {
    cx: &'a Ctx,
    plan: &'a Plan<'a>,
    deployment: Arc<DeploymentRt>,
    method: Method,
    path: String,
}

impl Sender for Proxy<'_> {
    type Done = Response<Body>;

    async fn send(&self, a: Attempt<'_>) -> Sent<Response<Body>> {
        let cx = self.cx;
        let d = &self.deployment;
        let attempts = u16::try_from(a.index + 1).unwrap_or(u16::MAX);
        let started = Instant::now();
        let mut att = outcome::Attempt {
            cx,
            plan: self.plan,
            deployment: d,
            cell: Cell::Identity,
            attempts,
            started,
            started_at: cx.rt.now(),
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
        };
        let timers = &a.timers;
        let prepared = d.build_request(UpstreamCall {
            op: UpstreamOp::OpenAi {
                class: "generate",
                path: &self.path,
            },
            stream: false,
            body: Bytes::new().into(),
            content_type: None,
            request_id: &cx.request_id,
            response_timeout: Duration::from_secs(86_400 * 365),
            connect_timeout: Some(timers.connect.max(Duration::from_millis(1))),
            anthropic: None,
            key: a.key,
            byok: None,
        });
        let Ok(prepared) = prepared else {
            att.record(None, Class::ConnectError, None);
            return Sent::Failed(failure::of_class(
                d,
                attempts,
                Class::ConnectError,
                None,
                Message::Fixed("The upstream request could not be built."),
            ));
        };
        let mut request = prepared.request;
        request.method = self.method.clone();
        request.resend_when_closed = false;
        let tls = cx
            .snap
            .security
            .egress_tls
            .clone()
            .unwrap_or_else(|| d.provider.tls.clone());
        match d.authorize(&mut request, &cx.rt.creds, &tls).await {
            Ok(auth) => att.acquired = auth.secrets(),
            Err(_) => {
                att.record(None, Class::CredentialRejected, None);
                return Sent::Failed(failure::fail(
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
        let far = started + Duration::from_secs(86_400 * 365);
        let (fire_at, fire_kind) = timers
            .fire_at(timers.timeouts.attempt, started, TimerKind::Attempt)
            .unwrap_or((far, TimerKind::Attempt));
        let sent = tokio::select! {
            () = cx.gone.cancelled() => {
                att.record(None, Class::ClientCancelled, None);
                return Sent::Failed(failure::cancelled(d, attempts));
            }
            () = tokio::time::sleep_until(fire_at) => {
                att.record(None, Class::Timeout, Some(fire_kind));
                return Sent::Failed(failure::timeout(d, attempts, fire_kind, timers));
            }
            r = cx.rt.egress.send(request) => r,
        };
        let upstream = match sent {
            Ok(u) => u,
            Err(t) => {
                let f = failure::transport(d, attempts, &t, timers);
                att.record_failure(None, &f);
                return Sent::Failed(f);
            }
        };
        let status = upstream.status;
        let class = antirion_resilience::classify::status(status.as_u16(), false);
        if class != Class::Success {
            let f = outcome::error_response(&att, upstream, class, false).await;
            att.record_failure(f.status.or(Some(status.as_u16())), &f);
            return Sent::Failed(f);
        }
        let body = match att.read(upstream.body, att.body_cap()).await {
            Ok(Ok(b)) => b,
            Ok(Err(why)) => {
                let f = att.unread(why, status.as_u16());
                att.record(Some(status.as_u16()), f.class, None);
                return Sent::Failed(f);
            }
            Err(f) => {
                att.record(None, f.class, f.timer);
                return Sent::Failed(f);
            }
        };
        att.record(Some(status.as_u16()), Class::Success, None);
        let mut out = Response::new(full(body));
        *out.status_mut() = status;
        out.headers_mut().insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        decision_headers(out.headers_mut(), d, attempts, status.as_u16(), a.first);
        cx.finish(&mut out, false);
        Sent::Success(out)
    }
}

/// Proxies the read or delete of native response `id` to `deployment`.
pub(crate) async fn proxy(
    cx: &Ctx,
    parts: &Parts,
    endpoint: &Endpoint,
    key: &KeyRt,
    deployment: &str,
    id: &str,
) -> Response<Body> {
    let Some(d) = cx
        .snap
        .providers
        .deployments
        .get(deployment)
        .filter(|d| d.usable())
        .cloned()
    else {
        return cx.error(&crate::respstate::affinity_unavailable());
    };
    let raw = Bytes::from_static(b"{}");
    let Ok(skim) = antirion_json::skim(&raw, &JsonLimits::default()) else {
        return cx.error(&crate::respstate::affinity_unavailable());
    };
    let parsed = Parsed::json(raw, skim, Some(Value::Object(serde_json::Map::new())));
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
    let kind = if parts.method == Method::DELETE {
        RequestKind::SideEffecting
    } else {
        RequestKind::ResourceBound
    };
    cx.log.update(|r| r.routed = true);
    let mut owner = Owner {
        cx,
        deployment: Arc::clone(&d),
        policy: Arc::new(policy),
        offered: false,
    };
    let mut proxy = Proxy {
        cx,
        plan: &plan,
        deployment: d,
        method: parts.method.clone(),
        path: format!("/responses/{}", antirion_providers::sigv4::uri_encode(id)),
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
            kind,
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
        Outcome::Success(resp) => resp,
        Outcome::Failed(f) => cx.error(&f.error),
        Outcome::NoCandidate => cx.error(&crate::respstate::affinity_unavailable()),
        Outcome::Cancelled => {
            let mut resp = Response::new(crate::ctx::empty());
            *resp.status_mut() = StatusCode::from_u16(499).unwrap_or(StatusCode::BAD_REQUEST);
            resp
        }
    }
}
