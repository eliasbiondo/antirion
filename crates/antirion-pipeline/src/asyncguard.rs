//! Asynchronous guardrails: started once their stage passes, off the request path; they never
//! delay or change the request, and their result is only in its log record.

use std::sync::Arc;
use std::time::Duration;

use antirion_guardrails::GuardStage;
use antirion_guardrails::runner::{Deferred, StageCx, Verdict};
use antirion_runtime::Runtime;
use serde_json::{Value, json};

use crate::ctx::Ctx;
use crate::guard::RequestGuard;
use crate::reqlog::Draft;

/// Starts the asynchronous guardrails of a stage off the request path. Each one's result goes into
/// its `async` entry of the request's record: the record in progress, or the stored one once the
/// request is settled.
pub(crate) fn spawn(cx: &Ctx, g: &RequestGuard, stage: GuardStage, asyncs: Vec<Deferred>) {
    for job in asyncs {
        let def = Arc::clone(&job.def);
        let net = g.net.clone();
        let facts = g.facts.clone();
        let hash_key = g.hash_key.clone();
        let request_schema = g.request_schema.clone();
        let rt = Arc::clone(&cx.rt);
        let log = cx.log.clone();
        let log_id = cx.log_id.clone();
        cx.rt.tasks.spawn(async move {
            let defs = [Arc::clone(&def)];
            let scx = StageCx {
                stage,
                defs: &defs,
                sequential: true,
                hash_key: hash_key.as_deref(),
                facts: &facts,
                net: &net,
                request_schema: request_schema.as_ref(),
                tokens: None,
                json_output: false,
            };
            let eval = job.run(&scx).await;
            let verdict = match eval.verdict {
                Verdict::Error => Verdict::ErrorOpen,
                v => v,
            };
            tracing::debug!(guardrail = %def.name, verdict = verdict.as_str(), "asynchronous guardrail finished");
            let found = AsyncResult {
                name: &def.name,
                stage,
                verdict,
                duration: eval.duration,
            };
            record_async(&rt, &log, &log_id, &found).await;
        });
    }
}

/// What an asynchronous guardrail found.
struct AsyncResult<'a> {
    name: &'a str,
    stage: GuardStage,
    verdict: Verdict,
    duration: Duration,
}

impl AsyncResult<'_> {
    /// Completes the guardrail's `async` entry among a record's evaluations; its index, `None` when
    /// it has none.
    fn apply(&self, evals: &mut [Value]) -> Option<usize> {
        let (index, entry) = evals.iter_mut().enumerate().find(|(_, e)| {
            e.get("name").and_then(Value::as_str) == Some(self.name)
                && e.get("stage").and_then(Value::as_str) == Some(self.stage.as_str())
                && e.get("verdict").and_then(Value::as_str) == Some(Verdict::Async.as_str())
        })?;
        if let Some(o) = entry.as_object_mut() {
            o.insert("result".into(), json!(self.verdict.as_str()));
            o.insert(
                "duration_ms".into(),
                json!(antirion_obs::timing::ms3(self.duration.as_secs_f64() * 1000.0)),
            );
            o.insert("failed_open".into(), json!(self.verdict == Verdict::ErrorOpen));
        }
        Some(index)
    }
}

async fn record_async(rt: &Runtime, log: &Draft, log_id: &str, found: &AsyncResult<'_>) {
    if log.update_live(|r| {
        found.apply(&mut r.guardrails);
    }) {
        return;
    }
    for _ in 0..20 {
        let mut completed: Option<(Value, usize)> = None;
        let patched = rt
            .patch_log(log_id, |record| {
                if let Some(evals) = record.get_mut("guardrails").and_then(Value::as_array_mut)
                    && let Some(i) = found.apply(evals)
                {
                    completed = Some((record.clone(), i));
                }
            })
            .await;
        if patched {
            if let Some((record, i)) = completed {
                rt.keep_safety(&record, &[], Some(i));
            }
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_async_result_completes_its_own_entry_only() {
        let mut evals = vec![
            json!({"name": "a", "stage": "input", "verdict": "pass", "duration_ms": 0.1, "failed_open": false}),
            json!({"name": "a", "stage": "output", "verdict": "async", "duration_ms": 0.0, "failed_open": false}),
        ];
        let found = AsyncResult {
            name: "a",
            stage: GuardStage::Output,
            verdict: Verdict::Flag,
            duration: Duration::from_millis(2),
        };
        assert_eq!(found.apply(&mut evals), Some(1));
        assert_eq!(evals[1]["verdict"], "async");
        assert_eq!(evals[1]["result"], "flag");
        assert_eq!(evals[1]["duration_ms"], 2.0);
        assert!(evals[0].get("result").is_none());
        let other = AsyncResult {
            name: "b",
            stage: GuardStage::Output,
            verdict: Verdict::Pass,
            duration: Duration::ZERO,
        };
        assert_eq!(other.apply(&mut evals), None);
    }
}
