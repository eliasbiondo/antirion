//! The `mcp_input` and `mcp_output` guardrail stages: the string leaves of tool and prompt arguments
//! are the input stage's segments, the text items and structured content of a tool result the output
//! stage's, so every check, redaction and fail mode works as on model traffic.

use std::sync::Arc;

use antirion_guardrails::def::{Def, GuardStage};
use antirion_guardrails::runner::{self, StageOutcome};
use antirion_guardrails::segments::Dialect;
use antirion_guardrails::webhook::CallFacts as HookFacts;
use antirion_mcp::{CallFacts, CallHook, HookVerdict, Pending};
use antirion_runtime::Runtime;
use antirion_runtime::identity::KeyRt;
use serde_json::{Value, json};

use crate::ctx::Ctx;
use crate::external::GuardNet;
use crate::guard::{RequestGuard, StreamCfg};
use crate::reqlog::Draft;

/// The guardrails attached to one caller's MCP calls.
#[derive(Debug)]
pub(crate) struct McpGuard {
    guard: RequestGuard,
    rt: Arc<Runtime>,
    log: Draft,
}

/// Pointers to the string leaves of `v`, in document order.
fn leaves(v: &Value, at: &str, out: &mut Vec<(String, String)>) {
    match v {
        Value::String(s) => out.push((at.to_owned(), s.clone())),
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                leaves(item, &format!("{at}/{i}"), out);
            }
        }
        Value::Object(map) => {
            for (k, item) in map {
                let key = k.replace('~', "~0").replace('/', "~1");
                leaves(item, &format!("{at}/{key}"), out);
            }
        }
        _ => {}
    }
}

/// The inspected strings of a tool result: text content items, then structured content leaves.
fn result_texts(result: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (i, item) in result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        if item.get("type").and_then(Value::as_str) == Some("text")
            && let Some(t) = item.get("text").and_then(Value::as_str)
        {
            out.push((format!("/content/{i}/text"), t.to_owned()));
        }
    }
    if let Some(sc) = result.get("structuredContent") {
        leaves(sc, "/structuredContent", &mut out);
    }
    out
}

/// Writes the texts the stage left in `body` back at their pointers in `target`.
fn write_back(target: &mut Value, pointers: &[(String, String)], texts: Vec<String>) {
    for ((ptr, _), text) in pointers.iter().zip(texts) {
        if let Some(slot) = target.pointer_mut(ptr) {
            *slot = Value::String(text);
        }
    }
}

impl McpGuard {
    /// The guardrails of `key`'s levels, or `None` when no MCP stage has any.
    pub(crate) fn new(cx: &Ctx, key: &KeyRt, end_user: Option<&str>) -> Option<McpGuard> {
        let levels = crate::policy::Levels::new(&cx.snap, key, "", None);
        let blocks = levels.member("guardrails");
        let attached = antirion_guardrails::attach::resolve_mcp(&cx.snap.guardrails, &blocks);
        if attached.is_empty() {
            return None;
        }
        let merged = levels.effective("guardrails");
        let g = &cx.snap.guardrails;
        Some(McpGuard {
            guard: RequestGuard {
                attached,
                sequential: merged.get("execution_order").and_then(Value::as_str) == Some("sequential"),
                streaming: StreamCfg {
                    mode: "buffer".to_owned(),
                    holdback: 0,
                    max_buffer: 0,
                    finish_on_block: false,
                    buffer_heartbeat: None,
                },
                facts: HookFacts {
                    request_id: cx.request_id.as_str().to_owned(),
                    endpoint: "mcp".to_owned(),
                    model: String::new(),
                    organization: key.organization.clone(),
                    team: key.team.clone(),
                    project: key.project.clone(),
                    key: key.name.clone(),
                    end_user: end_user.map(str::to_owned),
                },
                dialect: None,
                expose: g.expose_results,
                block_uninspectable: false,
                hash_key: g.hash_key.clone(),
                net: GuardNet {
                    rt: Arc::clone(&cx.rt),
                    snap: Arc::clone(&cx.snap),
                    request_id: cx.request_id.clone(),
                },
                request_schema: None,
                tokenizer: antirion_guardrails::tokens::Tokenizer::default(),
                tokens: None,
                json_output: false,
            },
            rt: Arc::clone(&cx.rt),
            log: cx.log.clone(),
        })
    }

    async fn run(
        &self,
        stage: GuardStage,
        defs: &[Arc<Def>],
        texts: &[(String, String)],
    ) -> (StageOutcome, Value) {
        let (mut body, dialect) = match stage {
            GuardStage::Input => (
                json!({"input": texts.iter().map(|(_, t)| t.clone()).collect::<Vec<_>>()}),
                Dialect::Embeddings,
            ),
            GuardStage::Output => {
                let choices: Vec<Value> = texts
                    .iter()
                    .enumerate()
                    .map(|(i, (_, t))| json!({"index": i, "message": {"role": "assistant", "content": t}}))
                    .collect();
                (json!({"choices": choices}), Dialect::Chat)
            }
        };
        let out = runner::run_stage(&self.guard.cx(stage, defs), &mut body, dialect).await;
        crate::guard::record(&self.rt, &self.log, None, &out.evals, false);
        (out, body)
    }

    /// The verdict of a stage over `texts`, with the replacement of `original` when it redacted.
    async fn verdict(
        &self,
        stage: GuardStage,
        original: &Value,
        texts: Vec<(String, String)>,
    ) -> HookVerdict {
        let defs = match stage {
            GuardStage::Input => &self.guard.attached.input,
            GuardStage::Output => &self.guard.attached.output,
        };
        if defs.is_empty() || texts.is_empty() {
            return HookVerdict::Continue;
        }
        let (out, body) = self.run(stage, defs, &texts).await;
        if let Some(e) = out.failed.and_then(|i| out.evals.get(i)) {
            return HookVerdict::Refuse(self.guard.error(e, 0));
        }
        if !out.changed {
            return HookVerdict::Continue;
        }
        let edited: Vec<String> = match stage {
            GuardStage::Input => body
                .get("input")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|v| v.as_str().unwrap_or_default().to_owned())
                .collect(),
            GuardStage::Output => body
                .get("choices")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|c| {
                    c.pointer("/message/content")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect(),
        };
        let mut replaced = original.clone();
        write_back(&mut replaced, &texts, edited);
        HookVerdict::Replace(replaced)
    }
}

impl CallHook for McpGuard {
    fn before_call<'a>(&'a self, call: &'a CallFacts<'a>) -> Pending<'a, HookVerdict> {
        Box::pin(async move {
            if !matches!(call.method, "tools/call" | "prompts/get") {
                return HookVerdict::Continue;
            }
            let Some(args) = call.arguments.filter(|a| !a.is_null()) else {
                return HookVerdict::Continue;
            };
            let mut texts = Vec::new();
            leaves(args, "", &mut texts);
            self.verdict(GuardStage::Input, args, texts).await
        })
    }

    fn after_result<'a>(&'a self, call: &'a CallFacts<'a>, result: &'a Value) -> Pending<'a, HookVerdict> {
        Box::pin(async move {
            if call.method != "tools/call" {
                return HookVerdict::Continue;
            }
            self.verdict(GuardStage::Output, result, result_texts(result))
                .await
        })
    }

    fn checks_results(&self, method: &str) -> bool {
        method == "tools/call" && !self.guard.attached.output.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_leaves_are_found_and_written_back() {
        let args = json!({"q": "secret", "n": 3, "list": ["a", {"b/c": "d"}]});
        let mut texts = Vec::new();
        leaves(&args, "", &mut texts);
        texts.sort();
        let ptrs: Vec<&str> = texts.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(ptrs, ["/list/0", "/list/1/b~1c", "/q"]);
        let mut out = args.clone();
        write_back(
            &mut out,
            &texts,
            vec!["A".into(), "D".into(), "[REDACTED]".into()],
        );
        assert_eq!(
            out,
            json!({"q": "[REDACTED]", "n": 3, "list": ["A", {"b/c": "D"}]})
        );
    }

    #[test]
    fn tool_results_offer_text_items_and_structured_content() {
        let r = json!({"content": [{"type": "text", "text": "hi"}, {"type": "image", "data": "x"}],
                       "structuredContent": {"k": "v"}});
        let texts = result_texts(&r);
        assert_eq!(
            texts,
            vec![
                ("/content/0/text".to_owned(), "hi".to_owned()),
                ("/structuredContent/k".to_owned(), "v".to_owned())
            ]
        );
    }
}
