//! Guardrails of one inference request: the effective guardrails and stream settings, the input
//! stage, the output stage of a complete response, and how verdicts reach the client
//! (`x-antirion-guardrails`, `x-antirion-degraded`, the block and failure errors), the request log,
//! the metrics and the spans.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use antirion_core::{Stage, headers as h};
use antirion_dialects::Endpoint;
use antirion_error::{Code, GatewayError, Message, Reason};
use antirion_guardrails::attach::Attached;
use antirion_guardrails::runner::{self, Deferred, Evaluation, StageCx, Verdict};
use antirion_guardrails::segments::Dialect;
use antirion_guardrails::structured::RequestSchema;
use antirion_guardrails::tokens::{Tokenizer, Tokens};
use antirion_guardrails::webhook::CallFacts;
use antirion_guardrails::window::Window;
use antirion_guardrails::{Category, Def, GuardStage};
use antirion_obs::metrics::Fam;
use antirion_runtime::Runtime;
use http::header::CONTENT_TYPE;
use http::{HeaderName, HeaderValue, Response};
use serde_json::{Value, json};

use crate::ctx::{Body, Ctx};
use crate::external::GuardNet;
use crate::reasons;
use crate::reqlog::Draft;

const INPUT_MESSAGE: &str = "The request was blocked by a content policy.";
const OUTPUT_MESSAGE: &str = "The response was blocked by a content policy.";
const DEFAULT_HOLDBACK: usize = 256;
const DEFAULT_MAX_BUFFER: u64 = 16 << 20;

/// What the response headers report about guardrails and extensions.
#[derive(Debug, Default)]
pub(crate) struct Trail(Mutex<TrailState>);

#[derive(Debug, Default)]
struct TrailState {
    items: Vec<(String, &'static str, &'static str)>,
    expose: bool,
    guardrails_degraded: bool,
    extensions_degraded: bool,
    /// Other components this request was served without.
    degraded: Vec<&'static str>,
    stream_mode: Option<&'static str>,
    /// A `json_schema` block that may be asked again: its `reask`.
    reask: Option<u32>,
}

impl Trail {
    fn with(&self, f: impl FnOnce(&mut TrailState)) {
        if let Ok(mut s) = self.0.lock() {
            f(&mut s);
        }
    }

    pub(crate) fn expose(&self, expose: bool) {
        self.with(|s| s.expose = expose);
    }

    fn push(&self, name: &str, stage: GuardStage, verdict: Verdict) {
        self.with(|s| s.items.push((name.to_owned(), stage.as_str(), verdict.as_str())));
    }

    pub(crate) fn extensions_degraded(&self) {
        self.with(|s| s.extensions_degraded = true);
    }

    pub(crate) fn degraded(&self, component: &'static str) {
        self.with(|s| s.degraded.push(component));
    }

    pub(crate) fn stream_mode(&self, mode: &'static str) {
        self.with(|s| s.stream_mode = Some(mode));
    }

    /// A `json_schema` guardrail with `reask` blocked the output stage.
    pub(crate) fn set_reask(&self, reask: u32) {
        self.with(|s| s.reask = Some(reask));
    }

    /// The `reask` of the `json_schema` guardrail that blocked the last output stage, once.
    pub(crate) fn take_reask(&self) -> Option<u32> {
        let mut out = None;
        self.with(|s| out = s.reask.take());
        out
    }

    /// The request is asked again: the output items of the stage that blocked leave the header.
    pub(crate) fn forget_output(&self) {
        self.with(|s| s.items.retain(|(_, stage, _)| *stage != "output"));
    }
}

/// Whether a streamed response's head announces the `x-antirion-guardrails` trailer: an output
/// guardrail applied to the stream and results are exposed.
pub(crate) fn announces_trailer(cx: &Ctx) -> bool {
    cx.guard
        .0
        .lock()
        .is_ok_and(|s| s.expose && s.stream_mode.is_some())
}

fn append_header(resp: &mut Response<Body>, name: &'static str, value: &str) {
    let h = resp.headers_mut();
    let joined = match h.get(name).and_then(|v| v.to_str().ok()) {
        Some(existing) if !existing.is_empty() => format!("{existing}, {value}"),
        _ => value.to_owned(),
    };
    if let Ok(v) = HeaderValue::from_str(&joined) {
        h.insert(HeaderName::from_static(name), v);
    }
}

/// Adds the guardrail and extension headers to a response.
pub(crate) fn finish(cx: &Ctx, resp: &mut Response<Body>) {
    let Ok(s) = cx.guard.0.lock() else { return };
    if s.expose && !s.items.is_empty() {
        let list: Vec<String> = s
            .items
            .iter()
            .map(|(n, st, v)| format!("{n};stage={st};verdict={v}"))
            .collect();
        if let Ok(v) = HeaderValue::from_str(&list.join(", ")) {
            resp.headers_mut()
                .insert(HeaderName::from_static("x-antirion-guardrails"), v);
        }
    }
    if s.guardrails_degraded {
        append_header(resp, h::DEGRADED, "guardrails");
    }
    if s.extensions_degraded {
        append_header(resp, h::DEGRADED, "extensions");
    }
    crate::mark_degraded(&s.degraded, resp.headers_mut());
    let stream = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/event-stream") || ct.starts_with("application/vnd.amazon"));
    if let (Some(mode), true) = (s.stream_mode, stream) {
        resp.headers_mut().insert(
            HeaderName::from_static("x-antirion-guardrail-stream"),
            HeaderValue::from_static(mode),
        );
    }
}

/// The stream setting of the effective `policies.guardrails.streaming`.
#[derive(Clone, Debug)]
pub(crate) struct StreamCfg {
    pub(crate) mode: String,
    pub(crate) holdback: usize,
    pub(crate) max_buffer: u64,
    pub(crate) finish_on_block: bool,
    /// Buffer mode commits and sends comment lines at this interval while it holds the stream.
    pub(crate) buffer_heartbeat: Option<Duration>,
}

/// How a streamed response is checked.
#[derive(Clone, Debug)]
pub(crate) enum OutMode {
    Passthrough,
    Window(Window),
    Buffer,
    Observe,
}

impl OutMode {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            OutMode::Passthrough => "passthrough",
            OutMode::Window(_) => "window",
            OutMode::Buffer => "buffer",
            OutMode::Observe => "observe",
        }
    }
}

/// The guardrails of one request and what running them needs.
#[derive(Debug)]
pub(crate) struct RequestGuard {
    pub(crate) attached: Attached,
    pub(crate) sequential: bool,
    pub(crate) streaming: StreamCfg,
    pub(crate) facts: CallFacts,
    /// The grammar of the endpoint's bodies; `None` when it has no inspectable content.
    pub(crate) dialect: Option<Dialect>,
    pub(crate) expose: bool,
    pub(crate) block_uninspectable: bool,
    pub(crate) hash_key: Option<Vec<u8>>,
    pub(crate) net: GuardNet,
    /// The schema the request asks the model to follow, when a `json_schema` guardrail may use it.
    pub(crate) request_schema: Option<RequestSchema>,
    /// Where the input stage keeps reversible tokens.
    pub(crate) tokenizer: Tokenizer,
    /// The tokens the input stage handed out, put back in the response.
    pub(crate) tokens: Option<Arc<Tokens>>,
    /// The request asked for JSON output and a `json_repair` guardrail may apply.
    pub(crate) json_output: bool,
}

/// What the input stage decided beyond the body.
#[derive(Debug, Default)]
pub(crate) struct InputOutcome {
    pub(crate) changed: bool,
    /// Checks that run alongside the upstream call.
    pub(crate) alongside: Vec<Deferred>,
    /// The Model a diverting guardrail sends the request to.
    pub(crate) divert: Option<String>,
    /// The reversible tokens handed out.
    pub(crate) tokens: Option<Arc<Tokens>>,
}

/// The body grammar of an endpoint whose content guardrails inspect.
pub(crate) fn dialect_of(endpoint: &Endpoint) -> Option<Dialect> {
    Some(match endpoint.id {
        "chat_completions" => Dialect::Chat,
        "completions" => Dialect::Completions,
        "responses" => Dialect::Responses,
        "messages" => Dialect::Anthropic,
        "gemini_generate" if matches!(endpoint.action, Some("generateContent" | "streamGenerateContent")) => {
            Dialect::Gemini
        }
        "bedrock_converse" => Dialect::Converse,
        "embeddings" => Dialect::Embeddings,
        "images" => Dialect::Images,
        "rerank" => Dialect::Rerank,
        "audio"
            if matches!(
                endpoint.upstream_path,
                Some("/audio/transcriptions" | "/audio/translations")
            ) =>
        {
            Dialect::Transcription
        }
        _ => return None,
    })
}

fn endpoint_name(endpoint: &Endpoint) -> String {
    match endpoint.id {
        "messages" | "count_tokens" => format!("anthropic.{}", endpoint.id),
        "gemini_generate" => "gemini.generate_content".to_owned(),
        "bedrock_converse" => "bedrock.converse".to_owned(),
        "bedrock_invoke" => "bedrock.invoke".to_owned(),
        "rerank" => "rerank".to_owned(),
        id => format!("openai.{id}"),
    }
}

/// What builds a [`RequestGuard`].
pub(crate) struct GuardInputs<'a> {
    pub(crate) levels: &'a [Option<&'a Value>],
    /// `policies.guardrails` merged over the levels (settings: narrowest wins).
    pub(crate) merged: &'a Value,
    pub(crate) endpoint: &'a Endpoint,
    pub(crate) model: &'a str,
    pub(crate) key: &'a antirion_runtime::identity::KeyRt,
    pub(crate) end_user: Option<&'a str>,
    /// The client's JSON body as parsed.
    pub(crate) body: Option<&'a Value>,
}

impl RequestGuard {
    pub(crate) fn new(cx: &Ctx, i: &GuardInputs<'_>) -> RequestGuard {
        let g = &cx.snap.guardrails;
        let dialect = dialect_of(i.endpoint);
        let request_schema = match (g.uses_request_schema, i.body, dialect) {
            (true, Some(body), Some(d)) => antirion_guardrails::structured::requested(body, d),
            _ => None,
        };
        let st = i.merged.get("streaming");
        let st_str = |k: &str| st.and_then(|s| s.get(k)).and_then(Value::as_str);
        let attached = antirion_guardrails::attach::resolve(g, i.levels);
        let repairs = attached
            .output
            .iter()
            .any(|d| matches!(d.check, antirion_guardrails::Check::JsonRepair));
        let json_output = match (repairs, i.body, dialect) {
            (true, Some(b), Some(d)) => antirion_guardrails::repair::wants_json(b, d),
            _ => false,
        };
        RequestGuard {
            attached,
            sequential: i.merged.get("execution_order").and_then(Value::as_str) == Some("sequential"),
            streaming: StreamCfg {
                mode: st_str("mode").unwrap_or("auto").to_owned(),
                holdback: st
                    .and_then(|s| s.get("holdback_chars"))
                    .and_then(Value::as_u64)
                    .and_then(|n| usize::try_from(n).ok())
                    .unwrap_or(DEFAULT_HOLDBACK),
                max_buffer: st_str("max_buffer")
                    .and_then(antirion_config::scalar::parse_size)
                    .unwrap_or(DEFAULT_MAX_BUFFER),
                finish_on_block: st_str("on_block") == Some("finish"),
                buffer_heartbeat: st_str("buffer_heartbeat")
                    .and_then(antirion_config::scalar::parse_duration),
            },
            facts: CallFacts {
                request_id: cx.request_id.as_str().to_owned(),
                endpoint: endpoint_name(i.endpoint),
                model: i.model.to_owned(),
                organization: i.key.organization.clone(),
                team: i.key.team.clone(),
                project: i.key.project.clone(),
                key: i.key.name.clone(),
                end_user: i.end_user.map(str::to_owned),
            },
            dialect,
            expose: g.expose_results,
            block_uninspectable: g.block_uninspectable,
            hash_key: g.hash_key.clone(),
            net: GuardNet {
                rt: Arc::clone(&cx.rt),
                snap: Arc::clone(&cx.snap),
                request_id: cx.request_id.clone(),
            },
            request_schema,
            tokenizer: Tokenizer::default(),
            tokens: None,
            json_output,
        }
    }

    /// What identifies the request's guardrails for the cache key: each attached guardrail, by
    /// stage, with its definition.
    pub(crate) fn fingerprint(&self, doc: &Value) -> Vec<u8> {
        let defs = doc.pointer("/guardrails/definitions");
        let entry = |stage: &str, d: &Arc<Def>| {
            json!([
                stage,
                d.name,
                defs.and_then(|m| m.get(&d.name)).cloned().unwrap_or(Value::Null)
            ])
        };
        let all: Vec<Value> = self
            .attached
            .input
            .iter()
            .map(|d| entry("input", d))
            .chain(self.attached.output.iter().map(|d| entry("output", d)))
            .collect();
        if all.is_empty() {
            return Vec::new();
        }
        serde_json::to_vec(&all).unwrap_or_default()
    }

    fn enforcing_output(&self) -> Vec<Arc<Def>> {
        self.attached
            .output
            .iter()
            .filter(|d| d.enforcing())
            .cloned()
            .collect()
    }

    /// A streaming request refused up front (`streaming.mode: reject`).
    pub(crate) fn rejects_stream(&self) -> bool {
        self.streaming.mode == "reject" && !self.enforcing_output().is_empty()
    }

    /// The tokens a streamed response must have put back: the input stage handed some out and a
    /// reversible guardrail applies to the output.
    pub(crate) fn restoring(&self) -> Option<Arc<Tokens>> {
        self.tokens
            .clone()
            .filter(|_| self.attached.output.iter().any(|d| d.reversible()))
    }

    /// A `json_repair` guardrail must see the whole output.
    fn repairs(&self) -> bool {
        self.json_output
            && self
                .attached
                .output
                .iter()
                .any(|d| matches!(d.check, antirion_guardrails::Check::JsonRepair))
    }

    /// How a streamed response is checked; `None` when no output guardrail applies.
    pub(crate) fn out_mode(&self) -> Option<OutMode> {
        if self.attached.output.is_empty() || self.dialect.is_none_or(Dialect::is_flat) {
            return None;
        }
        let enforcing = self.enforcing_output();
        let restore = self.restoring();
        let with_tokens = |w: Window| match &restore {
            Some(t) => w.restoring(Arc::clone(t)),
            None => w,
        };
        let restore_only = || {
            restore
                .as_ref()
                .map(|t| OutMode::Window(Window::new(Vec::new(), 0, None).restoring(Arc::clone(t))))
        };
        let windowed = || {
            let fits = enforcing
                .iter()
                .all(|d| d.window_len().is_some_and(|l| l <= self.streaming.holdback));
            if fits {
                OutMode::Window(with_tokens(Window::new(
                    enforcing.clone(),
                    self.streaming.holdback,
                    self.hash_key.clone(),
                )))
            } else {
                OutMode::Buffer
            }
        };
        Some(match self.streaming.mode.as_str() {
            "observe" => restore_only().unwrap_or(OutMode::Observe),
            _ if self.repairs() => OutMode::Buffer,
            _ if enforcing.is_empty() => restore_only().unwrap_or(OutMode::Passthrough),
            "buffer" => OutMode::Buffer,
            _ => windowed(),
        })
    }

    /// Streams in observe mode only record what the checks find, even while tokens are restored.
    pub(crate) fn observing(&self) -> bool {
        self.streaming.mode == "observe"
    }

    pub(crate) fn cx<'a>(&'a self, stage: GuardStage, defs: &'a [Arc<Def>]) -> StageCx<'a, GuardNet> {
        StageCx {
            stage,
            defs,
            sequential: self.sequential,
            hash_key: self.hash_key.as_deref(),
            facts: &self.facts,
            net: &self.net,
            request_schema: self.request_schema.as_ref(),
            tokens: Some(&self.tokenizer),
            json_output: self.json_output,
        }
    }

    /// Runs the output stage over a client-dialect response body (also assembled streams).
    pub(crate) async fn run_output(&self, body: &mut Value) -> antirion_guardrails::StageOutcome {
        let Some(d) = self.dialect else {
            return antirion_guardrails::StageOutcome::default();
        };
        runner::run_stage(&self.cx(GuardStage::Output, &self.attached.output), body, d).await
    }

    /// The error of a failed stage.
    pub(crate) fn error(&self, e: &Evaluation, attempts: u16) -> GatewayError {
        let stage = match e.stage {
            GuardStage::Input => Stage::GuardInput,
            GuardStage::Output => Stage::GuardOutput,
        };
        let failure = e.verdict == Verdict::Error;
        let category = e.category.unwrap_or(Category::Custom);
        let mut err = if failure {
            let mut x = GatewayError::with_reason(
                reasons::GUARDRAIL_UNAVAILABLE,
                stage,
                Message::Fixed("A guardrail could not be evaluated."),
            );
            x.retry_after = Some(Duration::from_secs(1));
            x
        } else {
            let message = e.def.message.clone().map_or(
                Message::Fixed(if e.stage == GuardStage::Input {
                    INPUT_MESSAGE
                } else {
                    OUTPUT_MESSAGE
                }),
                Message::built,
            );
            let mut x = GatewayError::new(Code::GuardrailBlocked, stage, message)
                .openai_code("content_policy_violation");
            if self.expose {
                x.reason = Some(category_reason(category));
            }
            x
        };
        err.attempts = attempts;
        if self.expose {
            err.guardrail = Some(Box::new(antirion_error::GuardrailFacts {
                shown: json!({
                    "name": e.def.name,
                    "type": e.def.type_name(),
                    "stage": e.stage.as_str(),
                    "category": if failure { "error" } else { category.as_str() },
                }),
                status: None,
            }));
        } else {
            err.hide_guardrail = true;
        }
        err
    }
}

/// The reason a block reports for its category.
pub(crate) fn category_reason(c: Category) -> Reason {
    match c {
        Category::DenyPattern => reasons::DENY_PATTERN,
        Category::AllowPattern => reasons::ALLOW_PATTERN,
        Category::Keyword => reasons::KEYWORD,
        Category::Pii => reasons::PII,
        Category::Secret => reasons::SECRET,
        Category::Hate => reasons::HATE,
        Category::Harassment => reasons::HARASSMENT,
        Category::Violence => reasons::VIOLENCE,
        Category::Sexual => reasons::SEXUAL,
        Category::SelfHarm => reasons::SELF_HARM,
        Category::Illicit => reasons::ILLICIT,
        Category::PromptInjection => reasons::PROMPT_INJECTION,
        Category::OffTopic => reasons::OFF_TOPIC,
        Category::InvalidJson => antirion_error::reason::hygiene::INVALID_JSON,
        Category::SchemaViolation => reasons::SCHEMA_VIOLATION,
        Category::ToolNotAllowed => reasons::TOOL_NOT_ALLOWED,
        Category::Length => reasons::LENGTH,
        Category::Uninspectable => reasons::UNINSPECTABLE,
        Category::Custom | Category::Error => reasons::CUSTOM,
    }
}

/// Records evaluations: the response header trail (when `trail`), the log record, metrics and
/// spans. In observe mode block and redact findings are recorded as flags.
pub(crate) fn record(rt: &Runtime, log: &Draft, trail: Option<&Trail>, evals: &[Evaluation], observe: bool) {
    let now = Instant::now();
    for e in evals {
        let verdict = match e.verdict {
            Verdict::Block | Verdict::Redact if observe => Verdict::Flag,
            v => v,
        };
        let failed_open = verdict == Verdict::ErrorOpen;
        if let Some(t) = trail {
            t.push(&e.def.name, e.stage, verdict);
            if failed_open {
                t.with(|s| s.guardrails_degraded = true);
            }
        }
        let start = now.checked_sub(e.duration).unwrap_or(now);
        let exposed = verified_keys(rt, &e.exposed);
        log.update(|r| {
            if e.digest.is_some() || !exposed.is_empty() {
                r.safety.push(antirion_runtime::safety::Mark {
                    entry: r.guardrails.len(),
                    digest: e.digest,
                    exposed: exposed.clone(),
                });
            }
            let spans: Vec<Value> = e
                .spans
                .iter()
                .map(|s| json!({"start": s.start, "end": s.end, "entity": s.entity}))
                .collect();
            let found = !matches!(verdict, Verdict::Pass | Verdict::Skipped | Verdict::Async);
            r.guardrails.push(json!({
                "name": e.def.name,
                "stage": e.stage.as_str(),
                "verdict": verdict.as_str(),
                "duration_ms": antirion_obs::timing::ms3(e.duration.as_secs_f64() * 1000.0),
                "failed_open": failed_open,
                "category": e.category.filter(|_| found).map(|c| c.as_str()),
                "severity": found.then_some(e.def.severity.as_str()),
                "confidence": found.then_some(e.confidence),
                "score": e.score.filter(|_| found),
                "spans": spans,
            }));
            if verdict != Verdict::Skipped && verdict != Verdict::Async {
                r.internal_spans.push(
                    crate::reqlog::InternalSpan::new(
                        format!("antirion.guardrail {}", e.def.name),
                        start,
                        now,
                    )
                    .attr("antirion.guardrail.stage", e.stage.as_str()),
                );
            }
            if failed_open && !r.degraded.iter().any(|(c, _)| *c == "guardrails") {
                r.degraded.push(("guardrails", Some("guardrail_error")));
            }
        });
        rt.metrics.add(
            Fam::GuardrailChecks,
            &[&e.def.name, e.stage.as_str(), verdict.as_str()],
            1.0,
        );
        if verdict != Verdict::Skipped && verdict != Verdict::Async {
            rt.metrics.observe(
                Fam::GuardrailDuration,
                &[&e.def.name, e.stage.as_str()],
                e.duration.as_secs_f64(),
            );
        }
    }
}

/// The key ids of the gateway virtual keys among `found` whose secret verifies against a key of
/// the serving configuration.
fn verified_keys(rt: &Runtime, found: &[antirion_core::Secret]) -> Vec<String> {
    if found.is_empty() {
        return Vec::new();
    }
    let snap = rt.gens.load();
    found
        .iter()
        .filter_map(|s| {
            let kid = antirion_guardrails::secrets::gateway_kid(s.expose())?;
            let held = snap.identity.keys_by_kid.get(&kid)?;
            rt.pepper.verify(s.expose(), &held.verifier).then_some(kid)
        })
        .collect()
}

/// The input stage. `body` is the client body (`None` for a body that is not JSON); returns
/// whether it changed, the checks that run alongside the upstream call, a divert and the tokens.
pub(crate) async fn input(
    cx: &Ctx,
    g: &RequestGuard,
    body: Option<&mut Value>,
) -> Result<InputOutcome, GatewayError> {
    if g.attached.is_empty() {
        return Ok(InputOutcome::default());
    }
    cx.guard.expose(g.expose);
    let mut no_text = Value::Object(serde_json::Map::new());
    let body = match (g.dialect, body) {
        (Some(Dialect::Transcription), None) => Some(&mut no_text),
        (_, b) => b,
    };
    let (Some(dialect), Some(body)) = (g.dialect, body) else {
        return uninspectable(cx, g).map(|changed| InputOutcome {
            changed,
            ..InputOutcome::default()
        });
    };
    if g.attached.input.is_empty() {
        return Ok(InputOutcome::default());
    }
    let out = runner::run_stage(&g.cx(GuardStage::Input, &g.attached.input), body, dialect).await;
    if let Some(t) = &out.template {
        return Err(crate::templating::refusal(t));
    }
    record(&cx.rt, &cx.log, Some(&cx.guard), &out.evals, false);
    if let Some(e) = out.failed.and_then(|i| out.evals.get(i)) {
        return Err(g.error(e, 0));
    }
    crate::asyncguard::spawn(cx, g, GuardStage::Input, out.asyncs);
    Ok(InputOutcome {
        changed: out.changed,
        alongside: out.parallel,
        divert: out.divert,
        tokens: g.tokenizer.finish(),
    })
}

/// An endpoint without inspectable content: every attached guardrail is skipped, or the request is
/// refused with `on_uninspectable: block`.
fn uninspectable(cx: &Ctx, g: &RequestGuard) -> Result<bool, GatewayError> {
    let skipped = |def: &Arc<Def>, stage| Evaluation {
        def: Arc::clone(def),
        stage,
        verdict: Verdict::Skipped,
        category: None,
        detail: None,
        duration: Duration::ZERO,
        spans: Vec::new(),
        score: None,
        confidence: 1.0,
        digest: None,
        exposed: Vec::new(),
    };
    let mut evals: Vec<Evaluation> = g
        .attached
        .input
        .iter()
        .map(|d| skipped(d, GuardStage::Input))
        .chain(g.attached.output.iter().map(|d| skipped(d, GuardStage::Output)))
        .collect();
    if g.block_uninspectable
        && let Some(first) = evals.first_mut()
    {
        first.verdict = Verdict::Block;
        first.category = Some(Category::Uninspectable);
        let blocked = first.clone();
        record(&cx.rt, &cx.log, Some(&cx.guard), &evals, false);
        return Err(g.error(&blocked, 0));
    }
    record(&cx.rt, &cx.log, Some(&cx.guard), &evals, false);
    Ok(false)
}

/// The output stage of a complete response in the client's dialect; returns whether the body
/// changed.
pub(crate) async fn output(
    cx: &Ctx,
    g: &RequestGuard,
    body: &mut Value,
    attempts: u16,
) -> Result<bool, GatewayError> {
    if g.attached.output.is_empty() || g.dialect.is_none() {
        return Ok(false);
    }
    let out = g.run_output(body).await;
    record(&cx.rt, &cx.log, Some(&cx.guard), &out.evals, false);
    if let Some(e) = out.failed.and_then(|i| out.evals.get(i)) {
        if let antirion_guardrails::Check::JsonSchema(j) = &e.def.check
            && j.reask > 0
            && e.verdict == Verdict::Block
        {
            cx.guard.set_reask(j.reask);
        }
        return Err(g.error(e, attempts));
    }
    crate::asyncguard::spawn(cx, g, GuardStage::Output, out.asyncs);
    let restored = match (g.restoring(), g.dialect) {
        (Some(t), Some(d)) => t.restore_body(body, d),
        _ => false,
    };
    Ok(out.changed || restored)
}

/// Whether a stream's output verdicts follow it as an `x-antirion-guardrails` trailer: output
/// guardrails apply, their results are exposed, and the client accepts trailers.
pub(crate) fn trailer_applies(g: Option<&RequestGuard>, headers: &http::HeaderMap) -> bool {
    g.is_some_and(|g| g.expose && g.out_mode().is_some()) && crate::account::accepts_trailers(headers)
}

/// The output items of a request's record, as the `x-antirion-guardrails` list.
pub(crate) fn output_items(log: &Draft) -> Option<HeaderValue> {
    let list = log.read(|r| {
        r.guardrails
            .iter()
            .filter(|e| e.get("stage").and_then(Value::as_str) == Some("output"))
            .filter_map(|e| {
                let name = e.get("name").and_then(Value::as_str)?;
                let verdict = e.get("verdict").and_then(Value::as_str)?;
                Some(format!("{name};stage=output;verdict={verdict}"))
            })
            .collect::<Vec<_>>()
            .join(", ")
    })?;
    if list.is_empty() {
        return None;
    }
    HeaderValue::from_str(&list).ok()
}

/// The refusal of a streaming request in reject mode.
pub(crate) fn stream_rejected() -> GatewayError {
    GatewayError::with_reason(
        reasons::GUARDRAIL_STREAM_REJECTED,
        Stage::GuardInput,
        Message::Fixed("Streaming is not allowed on this route because output guardrails are enforced."),
    )
}

/// The failure of buffer mode when the response exceeds `max_buffer`.
pub(crate) fn buffer_overflow(g: &RequestGuard, attempts: u16) -> GatewayError {
    let def = g
        .enforcing_output()
        .into_iter()
        .next()
        .or_else(|| g.attached.output.first().cloned());
    match def {
        Some(def) => g.error(
            &Evaluation {
                def,
                stage: GuardStage::Output,
                verdict: Verdict::Error,
                category: Some(Category::Error),
                detail: Some("max_buffer".to_owned()),
                duration: Duration::ZERO,
                spans: Vec::new(),
                score: None,
                confidence: 1.0,
                digest: None,
                exposed: Vec::new(),
            },
            attempts,
        ),
        None => {
            let mut e = GatewayError::with_reason(
                reasons::GUARDRAIL_UNAVAILABLE,
                Stage::GuardOutput,
                Message::Fixed("A guardrail could not be evaluated."),
            );
            e.retry_after = Some(Duration::from_secs(1));
            e.attempts = attempts;
            e
        }
    }
}

/// The output stage and the `response` hooks of a complete client-dialect response: the body to
/// send (changed only when a guardrail or hook changed it) and the headers hooks add.
pub(crate) async fn complete_response(
    a: &crate::outcome::Attempt<'_>,
    status: u16,
    body: bytes::Bytes,
) -> Result<(bytes::Bytes, Vec<(HeaderName, HeaderValue)>), GatewayError> {
    let guard = a
        .plan
        .guard
        .as_deref()
        .filter(|g| !g.attached.output.is_empty() && g.dialect.is_some());
    let hooks = a.plan.hooks.as_deref().filter(|h| crate::extend::has_response(h));
    if guard.is_none() && hooks.is_none() {
        return Ok((body, Vec::new()));
    }
    let Ok(mut v) = serde_json::from_slice::<Value>(&body) else {
        return Ok((body, Vec::new()));
    };
    let mut changed = false;
    if let Some(g) = guard {
        changed |= Box::pin(output(a.cx, g, &mut v, a.attempts)).await?;
    }
    let mut headers = Vec::new();
    if let Some(h) = hooks {
        changed |= Box::pin(crate::extend::response(
            a.cx,
            h,
            status,
            &mut v,
            &mut headers,
            a.attempts,
        ))
        .await?;
    }
    if !changed {
        return Ok((body, headers));
    }
    Ok((
        bytes::Bytes::from(serde_json::to_vec(&v).unwrap_or_default()),
        headers,
    ))
}

/// The credentials the gateway sends to guardrail and extension webhooks (header values and
/// signing secrets, and the token of a `Bearer` value), which upstream text is scrubbed of.
pub(crate) fn operator_secrets(snap: &antirion_runtime::Snapshot) -> Vec<String> {
    let mut raw: Vec<&str> = Vec::new();
    for d in snap.guardrails.defs.values() {
        if let antirion_guardrails::Check::Webhook(w) = &d.check {
            raw.extend(w.headers.iter().map(|(_, v)| v.as_str()));
            raw.extend(w.signing_secret.as_ref().map(antirion_core::Secret::expose));
        }
    }
    for p in snap.plugins.by_name.values() {
        if let antirion_ext::Carrier::Webhook {
            headers,
            signing_secret,
            ..
        } = &p.carrier
        {
            raw.extend(headers.iter().map(|(_, v)| v.as_str()));
            raw.extend(signing_secret.as_ref().map(antirion_core::Secret::expose));
        }
    }
    let mut out = Vec::new();
    for v in raw {
        let token = v.split_once(' ').map_or(v, |(_, t)| t.trim());
        for s in [v, token] {
            if s.len() >= 8 && !out.iter().any(|o| o == s) {
                out.push(s.to_owned());
            }
        }
    }
    out
}
