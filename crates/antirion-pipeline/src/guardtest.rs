//! `POST /admin/v1/guardrails/{name}:test`: one guardrail run on a sample text or body exactly as
//! its stage would run it, including its own external calls, and nothing else: no model call, no
//! record, no ledger line.

use std::sync::{Arc, Weak};

use antirion_core::RequestId;
use antirion_guardrails::runner::{self, Evaluation, StageCx, Verdict};
use antirion_guardrails::segments::Dialect;
use antirion_guardrails::webhook::CallFacts;
use antirion_guardrails::{GuardStage, structured};
use antirion_runtime::guardtest::{GuardrailTester, Sample, TestError, TestFuture};
use antirion_runtime::{Runtime, Snapshot};
use serde_json::{Value, json};

use crate::external::GuardNet;

/// The tester the server installs.
#[derive(Debug)]
pub struct Tester {
    pub rt: Weak<Runtime>,
}

/// The dialect of a sample body: Responses with `input` or `instructions`, Gemini with
/// `contents`/`candidates`, Bedrock Converse with `output.message`, Anthropic Messages with a
/// top-level `system`, Anthropic-only blocks or a `type: message` answer; OpenAI Chat otherwise.
fn dialect_of(body: &Value, output: bool) -> Dialect {
    let has = |k: &str| body.get(k).is_some();
    if output {
        return if has("choices") {
            Dialect::Chat
        } else if has("candidates") {
            Dialect::Gemini
        } else if body.pointer("/output/message").is_some() {
            Dialect::Converse
        } else if has("output") {
            Dialect::Responses
        } else {
            Dialect::Anthropic
        };
    }
    if has("input") || has("instructions") {
        return Dialect::Responses;
    }
    if has("contents") {
        return Dialect::Gemini;
    }
    let anthropic_block = body
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| m.get("content").and_then(Value::as_array))
        .flatten()
        .any(|b| {
            matches!(
                b.get("type").and_then(Value::as_str),
                Some("tool_use" | "tool_result" | "thinking" | "document")
            ) || b.get("source").is_some()
        });
    if has("system") || anthropic_block {
        Dialect::Anthropic
    } else {
        Dialect::Chat
    }
}

/// The body a text sample stands for, and where its text is read back.
fn text_body(text: &str, output: bool) -> (Value, &'static str) {
    if output {
        (
            json!({"choices": [{"index": 0, "message": {"role": "assistant", "content": text}}]}),
            "/choices/0/message/content",
        )
    } else {
        (json!({"messages": [{"role": "user", "content": text}]}), "")
    }
}

fn report(e: &Evaluation, redacted: Value) -> Value {
    let found = !matches!(e.verdict, Verdict::Pass | Verdict::Skipped | Verdict::Async);
    let spans: Vec<Value> = e
        .spans
        .iter()
        .map(|s| json!({"start": s.start, "end": s.end, "entity": s.entity.to_ascii_lowercase()}))
        .collect();
    json!({
        "verdict": e.verdict.as_str(),
        "category": e.category.filter(|_| found).map(|c| c.as_str()),
        "redacted": redacted,
        "spans": spans,
    })
}

pub(crate) async fn test(
    rt: Arc<Runtime>,
    snap: Arc<Snapshot>,
    name: String,
    output: bool,
    sample: Sample,
) -> Result<Value, TestError> {
    let def = Arc::clone(snap.guardrails.get(&name).ok_or(TestError::Unknown)?);
    let stage = if output {
        GuardStage::Output
    } else {
        GuardStage::Input
    };
    if !def.stages().contains(&stage) {
        return Err(TestError::Invalid(format!(
            "the guardrail {name} does not run at the {} stage",
            stage.as_str()
        )));
    }
    let (mut body, text_at) = match &sample {
        Sample::Text(t) => {
            let (b, at) = text_body(t, output);
            (b, Some(at))
        }
        Sample::Body(b) if b.is_object() => (b.clone(), None),
        Sample::Body(_) => return Err(TestError::Invalid("`body` must be a JSON object".to_owned())),
    };
    let dialect = dialect_of(&body, output);
    let request_id = RequestId::generate();
    let net = GuardNet {
        rt,
        snap: Arc::clone(&snap),
        request_id: request_id.clone(),
    };
    let facts = CallFacts {
        request_id: request_id.as_str().to_owned(),
        endpoint: "test".to_owned(),
        ..CallFacts::default()
    };
    let request_schema = match (&sample, output) {
        (Sample::Body(b), false) => structured::requested(b, dialect),
        _ => None,
    };
    let defs = [Arc::clone(&def)];
    let tokenizer = antirion_guardrails::tokens::Tokenizer::default();
    let cx = StageCx {
        stage,
        defs: &defs,
        sequential: true,
        hash_key: snap.guardrails.hash_key.as_deref(),
        facts: &facts,
        net: &net,
        request_schema: request_schema.as_ref(),
        tokens: Some(&tokenizer),
        json_output: true,
    };
    let out = runner::run_stage(&cx, &mut body, dialect).await;
    let mut eval = out.evals.into_iter().find(|e| e.verdict != Verdict::Async);
    if eval.is_none()
        && let Some(job) = out.asyncs.first().or_else(|| out.parallel.first())
    {
        eval = Some(job.run(&cx).await);
    }
    let eval = eval.ok_or_else(|| TestError::Invalid("the guardrail did not run".to_owned()))?;
    let redacted = match text_at {
        Some("") => body
            .get("messages")
            .and_then(Value::as_array)
            .and_then(|m| m.last())
            .and_then(|m| m.get("content"))
            .cloned()
            .unwrap_or(Value::Null),
        Some(at) => body.pointer(at).cloned().unwrap_or(Value::Null),
        None => body,
    };
    Ok(report(&eval, redacted))
}

impl GuardrailTester for Tester {
    fn test(&self, snap: Arc<Snapshot>, name: String, output: bool, sample: Sample) -> TestFuture {
        let rt = self.rt.upgrade();
        Box::pin(async move {
            let rt = rt.ok_or(TestError::Unavailable)?;
            test(rt, snap, name, output, sample).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_dialects() {
        assert_eq!(dialect_of(&json!({"messages": []}), false), Dialect::Chat);
        assert_eq!(
            dialect_of(&json!({"system": "s", "messages": []}), false),
            Dialect::Anthropic
        );
        assert_eq!(dialect_of(&json!({"input": "x"}), false), Dialect::Responses);
        assert_eq!(dialect_of(&json!({"contents": []}), false), Dialect::Gemini);
        assert_eq!(dialect_of(&json!({"choices": []}), true), Dialect::Chat);
        assert_eq!(
            dialect_of(&json!({"type": "message", "content": []}), true),
            Dialect::Anthropic
        );
        assert_eq!(dialect_of(&json!({"output": []}), true), Dialect::Responses);
    }
}
