//! The attempt loop: the decision table walked over the tiers of an attempt plan, with key rotation,
//! retries of the same deployment after a backoff or a hint, the attempt caps and the deadline. It
//! is generic over where candidates come from ([`Planner`]) and how one attempt is sent
//! ([`Sender`]), so it runs against scripted outcomes in tests.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use antirion_core::{Clock, SplitMix64, UtcMicros};
use antirion_error::{Code, GatewayError};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::class::{Class, RequestKind, RetrySame, TimerKind};
use crate::health::{Committed, HealthRegistry, KeyMode, Verdict};
use crate::policy::{Policy, Timeouts};
use crate::settings::HealthConfig;

mod hedge;
mod state;

use state::{Current, Loop};

/// A candidate deployment at a tree position.
#[derive(Clone, Debug)]
pub struct Selected {
    pub deployment: String,
    pub provider: String,
    /// The transition label from the last attempted position, for an attempt that moves.
    pub via: Option<String>,
    /// The resilience policy that applies at this position.
    pub policy: Arc<Policy>,
}

/// The key an attempt uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyPick {
    Key(String),
    /// The provider sends no credential.
    Keyless,
    /// No key of the deployment may be used now.
    None,
}

/// Where candidates come from: the tiers of the attempt plan.
pub trait Planner {
    /// The next eligible candidate of the current tier whose deployment is not in `tried`.
    fn select(&mut self, tried: &[String]) -> Option<Selected>;
    /// Moves to the next tier; false when there is none.
    fn next_tier(&mut self) -> bool;
    /// The position of `s` is being attempted: later transition labels are relative to it.
    fn attempted(&mut self, s: &Selected);
    /// A key of the candidate's deployment, excluding `tried` keys, or `last` under
    /// [`KeyMode::PreferUntried`] when no other qualifies.
    fn select_key(&mut self, s: &Selected, tried: &[String], last: Option<&str>, mode: KeyMode) -> KeyPick;
    /// The candidate was passed over without an attempt.
    fn skip(&mut self, s: &Selected, reason: &'static str);
    /// Checks made before an attempt that may refuse the candidate (a skip, with its reason).
    fn gate(&mut self, s: &Selected) -> impl Future<Output = Result<(), &'static str>> + Send;
    /// A verdict asked to move to the tiers of Model `model` (a class fallback): from now on the
    /// plan is that Model's, from its first tier. False when the Model cannot be used, and the
    /// plan is unchanged.
    fn class_fallback(&mut self, model: &str) -> bool {
        let _ = model;
        false
    }
}

/// The timers of one attempt: each effective timer, and the request deadline that clamps them.
#[derive(Clone, Debug)]
pub struct AttemptTimers {
    pub connect: Duration,
    pub timeouts: Timeouts,
    /// The policy's timers, before the client's read timeout shortened any.
    pub configured: Timeouts,
    pub deadline_at: Option<Instant>,
    /// The effective deadline, for messages.
    pub deadline: Option<Duration>,
}

impl AttemptTimers {
    /// When a timer measured from `from` fires and which timer it is: the configured one, or the
    /// deadline when that comes first.
    #[must_use]
    pub fn fire_at(
        &self,
        configured: Option<Duration>,
        from: Instant,
        kind: TimerKind,
    ) -> Option<(Instant, TimerKind)> {
        let own = configured.map(|d| from + d);
        match (own, self.deadline_at) {
            (Some(o), Some(d)) if d <= o => Some((d, TimerKind::Deadline)),
            (Some(o), _) => Some((o, kind)),
            (None, Some(d)) => Some((d, TimerKind::Deadline)),
            (None, None) => None,
        }
    }

    /// The effective value of a timer kind, for messages.
    #[must_use]
    pub fn value(&self, kind: TimerKind) -> Option<Duration> {
        match kind {
            TimerKind::Deadline => self.deadline,
            other => of(&self.timeouts, other),
        }
    }

    /// Whether a timer that fired was shorter than the policy's (the deadline, or the client's
    /// read timeout, shortened it): its expiry never counts against health.
    #[must_use]
    pub fn shortened(&self, kind: TimerKind) -> bool {
        match kind {
            TimerKind::Deadline => true,
            TimerKind::Upstream => false,
            other => match (of(&self.timeouts, other), of(&self.configured, other)) {
                (Some(e), Some(c)) => e < c,
                (Some(_), None) => true,
                _ => false,
            },
        }
    }
}

fn of(t: &Timeouts, kind: TimerKind) -> Option<Duration> {
    match kind {
        TimerKind::ResponseHeaders => t.response_headers,
        TimerKind::Ttft => t.ttft,
        TimerKind::StreamIdle => t.stream_idle,
        TimerKind::Attempt => t.attempt,
        TimerKind::Deadline | TimerKind::Upstream => None,
    }
}

/// The policy's timers with `ttft` and `stream_idle` capped by the client's read timeout.
fn capped(t: &Timeouts, read: Option<Duration>) -> Timeouts {
    let Some(read) = read else {
        return t.clone();
    };
    let cap = |v: Option<Duration>| Some(v.map_or(read, |v| v.min(read)));
    Timeouts {
        ttft: cap(t.ttft),
        stream_idle: cap(t.stream_idle),
        ..t.clone()
    }
}

/// One attempt to send.
#[derive(Debug)]
pub struct Attempt<'a> {
    /// 0-based; the attempt count once it is made is `index + 1`.
    pub index: u32,
    pub selected: &'a Selected,
    pub key: Option<&'a str>,
    pub timers: AttemptTimers,
    /// The wait before this attempt.
    pub wait: Duration,
    /// The transition label, for an attempt that moved to a new position.
    pub via: Option<&'a str>,
    /// The deployment of the first attempt.
    pub first: &'a str,
    /// The upstream rejected the request's encrypted reasoning or thinking signatures: this
    /// attempt is sent without them.
    pub repair: bool,
    /// The attempt takes part in a hedge race: it may be cancelled when another one wins.
    pub hedged: bool,
    /// The attempt is a hedge (an extra attempt of the race), not the first.
    pub hedge: bool,
}

/// A failed attempt.
#[derive(Debug)]
pub struct Failure {
    pub class: Class,
    pub status: Option<u16>,
    /// The timer that ended the attempt, for a timeout.
    pub timer: Option<TimerKind>,
    /// The upstream hint, on the gateway clock.
    pub hint: Option<UtcMicros>,
    /// The error the client gets if this attempt ends the request.
    pub error: Box<GatewayError>,
}

/// What one attempt produced.
#[derive(Debug)]
pub enum Sent<C> {
    Success(C),
    Failed(Failure),
}

/// Sends one attempt: one upstream request. A stream returns at its commit point.
pub trait Sender {
    /// What a successful attempt hands to the loop's caller.
    type Done;
    /// Sends one attempt. Several sends may run at once (a hedge race); a send that is dropped
    /// before it returns was cancelled.
    fn send(&self, a: Attempt<'_>) -> impl Future<Output = Sent<Self::Done>> + Send;
    /// The success the loop returns was recorded; a stream's failure after its commit needs it.
    fn committed(&mut self, c: Committed) {
        let _ = c;
    }
}

/// What the loop needs besides the plan and the sender.
#[derive(Debug)]
pub struct LoopCx<'a> {
    pub health: &'a HealthRegistry,
    pub health_cfg: &'a HealthConfig,
    pub clock: &'a dyn Clock,
    /// Cancelled when the client goes away.
    pub cancel: &'a CancellationToken,
    pub kind: RequestKind,
    pub max_attempts: u32,
    pub min_attempt_time: Duration,
    /// The effective deadline of the request.
    pub deadline: Option<Duration>,
    /// Whether the loop may move to another deployment at all.
    pub fallback: bool,
    /// Seeds the jitter of the backoff.
    pub seed: u64,
    /// The client's read timeout: it caps every attempt's `ttft` and `stream_idle`.
    pub read_timeout: Option<Duration>,
    /// Deployments whose breaker and cooldown the request ignores: every candidate was unhealthy
    /// and the configuration says to ignore health then.
    pub ignore_health: &'a [String],
}

/// How the loop ended.
#[derive(Debug)]
pub enum Outcome<C> {
    Success(C),
    /// The error of the last attempt that reached a verdict.
    Failed(Box<Failure>),
    /// Every candidate was skipped: no attempt was made.
    NoCandidate,
    /// The client went away.
    Cancelled,
}

/// Runs the attempt loop.
pub async fn run<P: Planner, S: Sender>(plan: &mut P, send: &mut S, cx: LoopCx<'_>) -> Outcome<S::Done> {
    let deadline_at = cx.deadline.map(|d| Instant::now() + d);
    let mut rng = SplitMix64::new(cx.seed);
    let mut state = Loop::new(&cx);
    let mut attempts = 0u32;
    // Whether hedges ran: the senders then count only their own attempt, so the total is set here.
    let mut hedged = false;
    let mut first: Option<String> = None;
    let mut last: Option<Box<Failure>> = None;
    let mut last_pair: (String, Option<String>) = (String::new(), None);
    let mut first_tier = true;
    let mut class_fallback_used = false;
    let mut repair = false;
    'tiers: loop {
        if !first_tier && (!cx.fallback || !plan.next_tier()) {
            break;
        }
        first_tier = false;
        let mut used = 0u32;
        let Some(mut cur) = state.fresh(plan).await else {
            continue;
        };
        loop {
            let starved = attempts > 0
                && state
                    .left(deadline_at)
                    .is_some_and(|l| l + state::MEASUREMENT_SLACK < cx.min_attempt_time);
            if attempts >= cx.max_attempts || starved {
                cx.health.abandon(&cur.ticket);
                break 'tiers;
            }
            if cur.moved {
                plan.attempted(&cur.sel);
            }
            let first_name = first.get_or_insert_with(|| cur.sel.deployment.clone()).clone();
            let mut timers = state.timers(&cur.sel, deadline_at);
            let hedging = cur.sel.policy.hedging.filter(|_| {
                attempts == 0 && cx.kind == RequestKind::Generation && cx.fallback && cx.max_attempts > 1
            });
            let sent = match hedging {
                Some(h) => {
                    let raced = hedge::race(
                        hedge::Race {
                            plan: &mut *plan,
                            send: &*send,
                            state: &mut state,
                            deadline_at,
                            first: &first_name,
                            hedging: h,
                        },
                        cur,
                    )
                    .await;
                    attempts += raced.extra;
                    hedged |= raced.extra > 0;
                    used += raced.extra;
                    cur = raced.decider;
                    timers = raced.timers;
                    raced.sent
                }
                None => {
                    send.send(Attempt {
                        index: attempts,
                        selected: &cur.sel,
                        key: cur.key.as_deref(),
                        timers: timers.clone(),
                        wait: cur.wait,
                        via: if cur.moved { cur.via.as_deref() } else { None },
                        first: &first_name,
                        repair,
                        hedged: false,
                        hedge: false,
                    })
                    .await
                }
            };
            attempts += 1;
            let d = cur.sel.deployment.clone();
            state.sent(&d, cur.key.as_deref());
            let settings = cx.health_cfg.for_deployment(&d);
            let f = match sent {
                Sent::Success(done) => {
                    let v = Verdict {
                        class: Class::Success,
                        status: None,
                        hint: None,
                        counts: true,
                    };
                    if let Some(c) = cx.health.finish(&cur.ticket, &v, settings, state.now()) {
                        send.committed(c);
                    }
                    return Outcome::Success(done);
                }
                Sent::Failed(f) => f,
            };
            let deadline_hit = f.timer == Some(TimerKind::Deadline);
            let predicate = f.error.reason == Some(crate::reasons::RESPONSE_PREDICATE_MATCHED);
            let v = verdict_of(&f, &timers);
            cx.health.finish(&cur.ticket, &v, settings, state.now());
            if f.class == Class::ClientCancelled {
                return Outcome::Cancelled;
            }
            let actions = if predicate {
                crate::class::Actions {
                    rotate_key: false,
                    next_deployment: cx.kind != RequestKind::ResourceBound,
                    retry_same: RetrySame::No,
                }
            } else {
                cur.sel.policy.retry.actions(f.class, cx.kind)
            };
            let hint = f.hint;
            let class_fallback = cur.sel.policy.retry.class_fallback(f.class).map(str::to_owned);
            let last_key = cur.key.clone();
            last_pair = (d.clone(), last_key.clone());
            last = Some(Box::new(f));
            if deadline_hit {
                break 'tiers;
            }
            if let Some(model) = class_fallback.filter(|_| !class_fallback_used) {
                class_fallback_used = true;
                if plan.class_fallback(&model) {
                    first_tier = true;
                    continue 'tiers;
                }
                break 'tiers;
            }
            if actions.retry_same == RetrySame::Repair {
                if repair {
                    break 'tiers;
                }
                repair = true;
                if !state.retry_allowed(&d) {
                    plan.skip(&cur.sel, state::RETRY_BUDGET);
                    break 'tiers;
                }
                let pick = last_key.clone().map_or(KeyPick::Keyless, KeyPick::Key);
                let Some((key, ticket)) = state.begin(&cur.sel, &pick) else {
                    break 'tiers;
                };
                cur = Current::again(cur.sel, key, ticket, Duration::ZERO);
                continue;
            }
            if actions.only_fail() {
                break 'tiers;
            }
            let mut budget_spent = false;
            if actions.rotate_key
                && let pick @ KeyPick::Key(_) =
                    plan.select_key(&cur.sel, state.keys_of(&d), None, KeyMode::Untried)
            {
                if state.retry_allowed(&d) {
                    if let Some((key, ticket)) = state.begin(&cur.sel, &pick) {
                        cur = Current::again(cur.sel, key, ticket, Duration::ZERO);
                        continue;
                    }
                } else {
                    budget_spent = true;
                    plan.skip(&cur.sel, state::RETRY_BUDGET);
                }
            }
            let retry = &cur.sel.policy.retry;
            if used < retry.tier_retries {
                if actions.next_deployment
                    && cx.fallback
                    && let Some(next) = state.fresh(plan).await
                {
                    used += 1;
                    cur = next;
                    continue;
                }
                let ignores = state.ignores(&d);
                if !budget_spent
                    && actions.retry_same != RetrySame::No
                    && !cx.health.blocked(&d, settings, state.now(), ignores)
                {
                    let mode = if actions.retry_same == RetrySame::UntriedKey {
                        KeyMode::Untried
                    } else {
                        KeyMode::PreferUntried
                    };
                    let pick = plan.select_key(&cur.sel, state.keys_of(&d), last_key.as_deref(), mode);
                    let pick = match (&pick, &last_key) {
                        (KeyPick::None, None) if mode == KeyMode::PreferUntried => KeyPick::Keyless,
                        _ => pick,
                    };
                    let now = state.now();
                    let chosen = match &pick {
                        KeyPick::Key(k) => Some(k.as_str()),
                        _ => None,
                    };
                    let pending = [hint, cx.health.cooldown(&d, chosen, now)]
                        .into_iter()
                        .flatten()
                        .max()
                        .map_or(Duration::ZERO, |at| at.since(now));
                    let n = state.retries_same.get(&d).copied().unwrap_or(0) + 1;
                    let wait = if actions.retry_same == RetrySame::Immediate {
                        pending
                    } else {
                        let u = (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
                        retry.backoff.delay(n, u).max(pending)
                    };
                    let fits = state.left(deadline_at).is_none_or(|l| {
                        wait.saturating_add(cx.min_attempt_time) <= l + state::MEASUREMENT_SLACK
                    });
                    if pick != KeyPick::None && pending <= retry.retry_after_max_wait && fits {
                        if !state.retry_allowed(&d) {
                            plan.skip(&cur.sel, state::RETRY_BUDGET);
                        } else if let Some((key, ticket)) = state.begin(&cur.sel, &pick) {
                            tokio::select! {
                                () = cx.cancel.cancelled() => {
                                    cx.health.abandon(&ticket);
                                    return Outcome::Cancelled;
                                }
                                () = tokio::time::sleep(wait) => {}
                            }
                            used += 1;
                            state.retries_same.insert(d.clone(), n);
                            cur = Current::again(cur.sel, key, ticket, wait);
                            continue;
                        }
                    }
                }
            }
            if actions.next_deployment {
                continue 'tiers;
            }
            break 'tiers;
        }
    }
    match last {
        Some(mut f) => {
            finalize(&mut f, &cx, &state.tried, &last_pair);
            if hedged {
                f.error.attempts = f.error.attempts.max(u16::try_from(attempts).unwrap_or(u16::MAX));
            }
            Outcome::Failed(f)
        }
        None => Outcome::NoCandidate,
    }
}

/// The health verdict of a failed attempt: a timer the deadline or the client shortened, and an
/// answer a response predicate turned into a failure, never count against the deployment.
fn verdict_of(f: &Failure, timers: &AttemptTimers) -> Verdict {
    let predicate = f.error.reason == Some(crate::reasons::RESPONSE_PREDICATE_MATCHED);
    Verdict {
        class: f.class,
        status: f.status,
        hint: f.hint,
        counts: !predicate && !f.timer.is_some_and(|k| timers.shortened(k)),
    }
}

/// Sets the final error's `Retry-After`: the rate-limit hint or cooldown on a 429, else the
/// earliest known recovery among the tried deployments on a 502, 503 or 504.
fn finalize(f: &mut Failure, cx: &LoopCx<'_>, tried: &[String], last: &(String, Option<String>)) {
    if f.error.retry_after.is_some() {
        return;
    }
    let now = cx.clock.now();
    let at = match f.error.code {
        Code::UpstreamRateLimited => [f.hint, cx.health.cooldown(&last.0, last.1.as_deref(), now)]
            .into_iter()
            .flatten()
            .max(),
        Code::UpstreamUnavailable
        | Code::UpstreamTimeout
        | Code::UpstreamBadResponse
        | Code::NoHealthyDeployment => tried.iter().filter_map(|d| cx.health.recovery(d, now)).min(),
        _ => None,
    };
    if let Some(at) = at.filter(|a| *a > now) {
        f.error.retry_after = Some(at.since(now));
    } else if f.error.code == Code::UpstreamRateLimited {
        f.error.retry_after = Some(Duration::from_secs(1));
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
