//! The hedge race of a request's first attempt: while no attempt has an answer, a hedge starts on
//! the next deployment every `delay`, up to `max_extra`. The first success wins and the attempts
//! still running are dropped, which cancels them; a failure is final for its attempt only, and the
//! race ends with the last failure once nothing runs. The loop goes on from the deciding attempt;
//! every other attempt is settled here: the health verdict of a failure, the probe slot of an
//! attempt that was cancelled.

use std::sync::OnceLock;
use std::time::Duration;

use antirion_core::Stage;
use antirion_error::{Code, GatewayError, Message};
use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use tokio::time::Instant;

use super::state::{Current, Loop};
use super::{Attempt, AttemptTimers, Failure, Planner, Sender, Sent, verdict_of};
use crate::class::Class;
use crate::hedging::Hedging;

/// What the race needs of the loop.
pub(super) struct Race<'r, 'c, 'a, P, S> {
    pub(super) plan: &'r mut P,
    pub(super) send: &'r S,
    pub(super) state: &'r mut Loop<'c, 'a>,
    pub(super) deadline_at: Option<Instant>,
    pub(super) first: &'r str,
    pub(super) hedging: Hedging,
}

/// How the race ended.
pub(super) struct Raced<C> {
    /// The attempt the loop goes on from: the winner, or the last to fail.
    pub(super) decider: Current,
    pub(super) timers: AttemptTimers,
    pub(super) sent: Sent<C>,
    /// Attempts started besides the decider.
    pub(super) extra: u32,
}

/// One attempt of the race.
struct Runner {
    cur: Current,
    timers: AttemptTimers,
    index: u32,
}

/// What woke the race.
enum Step<D> {
    Hedge,
    Answer(u32, Sent<D>),
    /// Nothing runs and no hedge may start, which the race never lets happen.
    Empty,
}

/// Hedges a race may start (`max_extra` is 1 or 2).
const HEDGES: usize = 2;

async fn run_one<'s, S: Sender>(send: &'s S, r: &'s Runner, first: &'s str) -> (u32, Sent<S::Done>) {
    let sent = send
        .send(Attempt {
            index: r.index,
            selected: &r.cur.sel,
            key: r.cur.key.as_deref(),
            timers: r.timers.clone(),
            wait: Duration::ZERO,
            via: if r.cur.moved { r.cur.via.as_deref() } else { None },
            first,
            repair: false,
            hedged: true,
            hedge: r.index > 0,
        })
        .await;
    (r.index, sent)
}

/// The next position for a hedge: the current tier's next one, else the next tier's first.
async fn next_position<P: Planner>(plan: &mut P, state: &Loop<'_, '_>) -> Option<Current> {
    if let Some(c) = state.fresh(plan).await {
        return Some(c);
    }
    if plan.next_tier() {
        return state.fresh(plan).await;
    }
    None
}

/// A race that ended with nothing running and no answer, which the loop's structure rules out.
fn no_answer() -> Failure {
    Failure {
        class: Class::ServerError,
        status: None,
        timer: None,
        hint: None,
        error: Box::new(GatewayError::new(
            Code::Internal,
            Stage::Attempt,
            Message::Fixed("The hedged attempts ended without an answer."),
        )),
    }
}

pub(super) async fn race<P: Planner, S: Sender>(
    r: Race<'_, '_, '_, P, S>,
    primary: Current,
) -> Raced<S::Done> {
    let Race {
        plan,
        send,
        state,
        deadline_at,
        first,
        hedging,
    } = r;
    let cx = state.cx;
    state.sent(&primary.sel.deployment, primary.key.as_deref());
    let first_runner = Runner {
        timers: state.timers(&primary.sel, deadline_at),
        cur: primary,
        index: 0,
    };
    let hedges: [OnceLock<Runner>; HEDGES] = Default::default();
    let max_extra = hedging.max_extra.min(u32::try_from(HEDGES).unwrap_or(0));
    let mut running = FuturesUnordered::new();
    running.push(run_one(send, &first_runner, first));
    let mut extra = 0u32;
    let mut next_at = Instant::now() + hedging.delay;
    let mut failures: Vec<(u32, Failure)> = Vec::new();
    let (index, sent) = loop {
        let can_hedge = extra < max_extra && 1 + extra < cx.max_attempts;
        let step = tokio::select! {
            () = tokio::time::sleep_until(next_at), if can_hedge => Step::Hedge,
            Some((index, sent)) = running.next() => Step::Answer(index, sent),
            else => Step::Empty,
        };
        match step {
            Step::Hedge => {
                next_at += hedging.delay;
                let Some(cell) = hedges.get(extra as usize) else {
                    extra = max_extra;
                    continue;
                };
                let Some(cur) = next_position(plan, state).await else {
                    extra = max_extra;
                    continue;
                };
                if cur.moved {
                    plan.attempted(&cur.sel);
                }
                state.sent(&cur.sel.deployment, cur.key.as_deref());
                let timers = state.timers(&cur.sel, deadline_at);
                extra += 1;
                let _ = cell.set(Runner {
                    cur,
                    timers,
                    index: extra,
                });
                if let Some(r) = cell.get() {
                    running.push(run_one(send, r, first));
                }
            }
            Step::Answer(index, Sent::Failed(f))
                if f.class != Class::ClientCancelled && !running.is_empty() =>
            {
                failures.push((index, f));
            }
            Step::Answer(index, done) => break (index, done),
            Step::Empty => break (0, Sent::Failed(no_answer())),
        }
    };
    drop(running);
    let now = state.now();
    let mut decider = first_runner;
    let mut others = Vec::with_capacity(HEDGES);
    for r in hedges.into_iter().filter_map(OnceLock::into_inner) {
        if r.index == index {
            others.push(std::mem::replace(&mut decider, r));
        } else {
            others.push(r);
        }
    }
    for r in others {
        match failures.iter().find(|(i, _)| *i == r.index) {
            Some((_, f)) => {
                let settings = cx.health_cfg.for_deployment(&r.cur.sel.deployment);
                cx.health
                    .finish(&r.cur.ticket, &verdict_of(f, &r.timers), settings, now);
            }
            None => cx.health.abandon(&r.cur.ticket),
        }
    }
    Raced {
        decider: decider.cur,
        timers: decider.timers,
        sent,
        extra,
    }
}

#[cfg(test)]
#[path = "hedge_tests.rs"]
mod tests;
