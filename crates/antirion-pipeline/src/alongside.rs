//! Input checks with `execution: parallel_with_call`: they run while the upstream call does, and
//! the response is held (nothing reaches the client) until every one of them has passed. A block or
//! a fail-closed failure drops the call, which cancels the upstream, and answers with the check's
//! error instead.

use std::future::Future;

use antirion_error::GatewayError;
use antirion_guardrails::GuardStage;
use antirion_guardrails::runner::Deferred;

use crate::attempt::Done;
use crate::ctx::Ctx;
use crate::guard::{RequestGuard, record};

/// Runs the checks together; the error of the first in execution order that failed.
pub(crate) async fn checks(cx: &Ctx, g: &RequestGuard, jobs: &[Deferred]) -> Result<(), GatewayError> {
    let defs: Vec<_> = jobs.iter().map(|j| std::sync::Arc::clone(&j.def)).collect();
    let scx = g.cx(GuardStage::Input, &defs);
    let evals = futures_util::future::join_all(jobs.iter().map(|j| j.run(&scx))).await;
    record(&cx.rt, &cx.log, Some(&cx.guard), &evals, false);
    match evals.iter().find(|e| e.verdict.fails()) {
        Some(e) => {
            let attempts = cx.log.read(|r| r.attempts.len()).unwrap_or(0);
            Err(g.error(e, u16::try_from(attempts).unwrap_or(u16::MAX)))
        }
        None => Ok(()),
    }
}

/// The upstream call `call`, answered only once `jobs` have passed.
pub(crate) async fn run<F>(
    cx: &Ctx,
    g: &RequestGuard,
    jobs: &[Deferred],
    call: F,
) -> Result<Done, GatewayError>
where
    F: Future<Output = Result<Done, GatewayError>>,
{
    let verdict = checks(cx, g, jobs);
    tokio::pin!(verdict);
    tokio::pin!(call);
    tokio::select! {
        biased;
        v = &mut verdict => match v {
            Ok(()) => call.await,
            Err(e) => Err(e),
        },
        done = &mut call => match verdict.await {
            Ok(()) => done,
            Err(e) => {
                drop(done);
                Err(e)
            }
        },
    }
}
