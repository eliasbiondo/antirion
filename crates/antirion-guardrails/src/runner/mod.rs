//! One stage of guardrails over a body: transforms, then redactions, then checks (in parallel with a
//! reported guardrail that does not depend on timing, or one at a time), then the asynchronous ones.
//! Checks that run alongside the upstream call are handed back to run there.

mod asked;
mod conclude;
mod transforms;

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use serde_json::Value;

pub use self::conclude::{Found, Subject, scan, spans_in};
use crate::def::{Action, Category, Check, Def, GuardStage, Origin};
use crate::segments::{Dialect, Segment, input_segments, output_segments, strip_marks, write_back};
use crate::structured::RequestSchema;
use crate::template::TemplateError;
use crate::tokens::Tokenizer;
use crate::transform;
use crate::webhook::CallFacts;

/// Allowance over a check's `timeout` for connection setup and scheduling, within the 100 ms per
/// phase the latency bound grants.
const GRACE: Duration = Duration::from_millis(50);

/// The network calls of webhook and moderation checks. Implementations need not bound the time;
/// the runner does.
pub trait Net: Send + Sync {
    /// POSTs a webhook body; the answer's status and body.
    fn webhook(
        &self,
        hook: &crate::def::Webhook,
        body: Vec<u8>,
        timeout: Duration,
    ) -> impl Future<Output = Result<(u16, Vec<u8>), String>> + Send;

    /// Sends `inputs` to the moderation Model; the answer's status and body.
    fn moderate(
        &self,
        model: &str,
        inputs: Vec<String>,
        timeout: Duration,
    ) -> impl Future<Output = Result<(u16, Vec<u8>), String>> + Send;

    /// Asks the judge's Model `message`; the text of its answer.
    fn judge(
        &self,
        judge: &crate::services::Judge,
        message: String,
        timeout: Duration,
    ) -> impl Future<Output = Result<String, String>> + Send;

    /// POSTs a JSON body to a classifier or partner service; the answer's status and body.
    fn post(
        &self,
        call: crate::services::ServiceCall,
        timeout: Duration,
    ) -> impl Future<Output = Result<(u16, Vec<u8>), String>> + Send;

    /// Asks Model `model` one question (`system`, then `user`); the text of its answer.
    fn chat(
        &self,
        model: &str,
        system: &str,
        user: &str,
        max_tokens: u32,
        timeout: Duration,
    ) -> impl Future<Output = Result<String, String>> + Send;

    /// Embeds `inputs` with Model `model`; one vector per input, in order.
    fn embed(
        &self,
        model: &str,
        inputs: Vec<String>,
        timeout: Duration,
    ) -> impl Future<Output = Result<Vec<Vec<f32>>, String>> + Send;
}

/// A guardrail's verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Flag,
    Redact,
    Block,
    Error,
    ErrorOpen,
    Skipped,
    Async,
    /// The request goes to another Model's route.
    Divert,
}

impl Verdict {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::Flag => "flag",
            Verdict::Redact => "redact",
            Verdict::Block => "block",
            Verdict::Error => "error",
            Verdict::ErrorOpen => "error_open",
            Verdict::Skipped => "skipped",
            Verdict::Async => "async",
            Verdict::Divert => "divert",
        }
    }

    /// The stage fails.
    #[must_use]
    pub const fn fails(self) -> bool {
        matches!(self, Verdict::Block | Verdict::Error)
    }
}

/// One guardrail's evaluation at one stage.
#[derive(Clone, Debug)]
pub struct Evaluation {
    pub def: Arc<Def>,
    pub stage: GuardStage,
    pub verdict: Verdict,
    /// The reported category of a block (or `error`).
    pub category: Option<Category>,
    /// What only logs see: the entity, detector, provider category or failure.
    pub detail: Option<String>,
    pub duration: Duration,
    /// Where the matches are: character offsets into the checked text, with the entity.
    pub spans: Vec<Located>,
    /// The score a classifier or service gave a finding.
    pub score: Option<f64>,
    /// How sure the check is of a finding, in [0, 1]: 1 for deterministic checks.
    pub confidence: f64,
    /// A digest of the normalised matched text (never the text), stable across requests.
    pub digest: Option<[u8; 32]>,
    /// The gateway virtual keys found in the text, for the caller to check against its keys.
    pub exposed: Vec<antirion_core::Secret>,
}

/// One match, in characters from the start of the segment it was found in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Located {
    pub segment: usize,
    pub start: usize,
    pub end: usize,
    pub entity: String,
}

/// What a stage needs besides the body.
#[derive(Debug)]
pub struct StageCx<'a, N> {
    pub stage: GuardStage,
    /// The guardrails in execution order.
    pub defs: &'a [Arc<Def>],
    pub sequential: bool,
    pub hash_key: Option<&'a [u8]>,
    pub facts: &'a CallFacts,
    pub net: &'a N,
    /// The schema the request asked the model to follow, for `json_schema` checks that use it.
    pub request_schema: Option<&'a RequestSchema>,
    /// Where reversible guardrails keep the tokens of the input stage.
    pub tokens: Option<&'a Tokenizer>,
    /// The request asked for JSON output (what `json_repair` needs).
    pub json_output: bool,
}

/// A guardrail that runs off the stage: asynchronously after it, or alongside the upstream call.
#[derive(Debug, Clone)]
pub struct Deferred {
    pub def: Arc<Def>,
    /// The segments in its scope.
    pub segments: Vec<Segment>,
    /// The body as the stage left it, for checks that read it.
    pub body: Option<(Value, Dialect)>,
}

impl Deferred {
    /// Runs the guardrail.
    pub async fn run<N: Net>(&self, cx: &StageCx<'_, N>) -> Evaluation {
        let scoped: Vec<(usize, &Segment)> = self.segments.iter().enumerate().collect();
        let subject = Subject {
            segs: &scoped,
            body: self.body.as_ref().map(|(b, d)| (b, *d)),
        };
        evaluate_scoped(&self.def, cx, subject).await.0
    }
}

/// The result of a stage.
#[derive(Debug, Default)]
pub struct StageOutcome {
    /// One evaluation per guardrail that ran in the stage, in execution order.
    pub evals: Vec<Evaluation>,
    /// The index (into `evals`) of the reported guardrail when the stage failed.
    pub failed: Option<usize>,
    /// The body was changed (transform or redaction).
    pub changed: bool,
    /// Asynchronous guardrails to run off the request path.
    pub asyncs: Vec<Deferred>,
    /// Input checks to run while the upstream call does.
    pub parallel: Vec<Deferred>,
    /// The Model the first diverting guardrail sends the request to.
    pub divert: Option<String>,
    /// The request's template could not be applied: nothing else ran.
    pub template: Option<TemplateError>,
}

/// Whether `seg` is in `def`'s scope at `stage`.
fn in_scope(def: &Def, stage: GuardStage, seg: &Segment, last_user: Option<usize>) -> bool {
    if seg.origin == Origin::Operator && !def.include_operator {
        return false;
    }
    if stage == GuardStage::Input {
        if let Some(kinds) = &def.kinds
            && !kinds.contains(&seg.kind)
        {
            return false;
        }
        if def.last_user && (last_user.is_none() || seg.message != last_user) {
            return false;
        }
    }
    true
}

/// The segments, and the body, one stage works on.
struct Work<'b> {
    segments: Vec<Segment>,
    last_user: Option<usize>,
    edited: Vec<bool>,
    body: &'b mut Value,
    dialect: Dialect,
    stage: GuardStage,
    operator_ptrs: Vec<String>,
}

impl Work<'_> {
    fn extract(&mut self) {
        let (segments, last_user) = match self.stage {
            GuardStage::Input => input_segments(self.body, self.dialect, &self.operator_ptrs),
            GuardStage::Output => (output_segments(self.body, self.dialect), None),
        };
        self.edited = vec![false; segments.len()];
        self.segments = segments;
        self.last_user = last_user;
    }

    /// Writes the edited segments into the body; whether any was.
    fn flush(&mut self) -> bool {
        let mut any = false;
        for (seg, edited) in self.segments.iter().zip(self.edited.iter_mut()) {
            if *edited {
                write_back(self.body, seg);
                *edited = false;
                any = true;
            }
        }
        any
    }

    fn scoped(&self, def: &Def) -> Vec<(usize, &Segment)> {
        self.segments
            .iter()
            .enumerate()
            .filter(|(_, s)| in_scope(def, self.stage, s, self.last_user))
            .collect()
    }

    fn deferred(&self, def: &Arc<Def>) -> Deferred {
        Deferred {
            def: Arc::clone(def),
            segments: self.scoped(def).into_iter().map(|(_, s)| s.clone()).collect(),
            body: def.reads_body().then(|| (self.body.clone(), self.dialect)),
        }
    }
}

/// Runs one guardrail over what it inspects.
pub async fn evaluate_scoped<N: Net>(
    def: &Arc<Def>,
    cx: &StageCx<'_, N>,
    subject: Subject<'_>,
) -> (Evaluation, Vec<(usize, String)>, Option<Value>) {
    let started = Instant::now();
    let c = conclude::conclude(def, cx, subject).await;
    (
        Evaluation {
            def: Arc::clone(def),
            stage: cx.stage,
            verdict: c.verdict,
            category: c.category,
            detail: c.detail,
            duration: started.elapsed(),
            spans: c.spans,
            score: c.score,
            confidence: c.confidence,
            digest: c.digest,
            exposed: c.exposed,
        },
        c.edits,
        c.replacement,
    )
}

/// An evaluation that did not run.
#[must_use]
pub fn skipped(def: &Arc<Def>, stage: GuardStage) -> Evaluation {
    Evaluation {
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
    }
}

/// The kind of work a guardrail does in a stage.
fn phase(def: &Def, stage: GuardStage) -> Phase {
    if def.reversible() && stage == GuardStage::Output {
        Phase::Restore
    } else if def.is_transform() {
        Phase::Transform
    } else if def.asynchronous {
        Phase::Async
    } else if def.parallel && stage == GuardStage::Input {
        Phase::Parallel
    } else if def.action == Action::Redact {
        if matches!(def.check, Check::ToolPermission(_)) {
            Phase::RemoveTools
        } else {
            Phase::Redact
        }
    } else {
        Phase::Check
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Transform,
    /// Reversible tokens are put back after the stage, by the caller that holds them.
    Restore,
    RemoveTools,
    Redact,
    Check,
    Async,
    Parallel,
}

/// Runs a stage over `body` (a client request at `input`, a client-dialect response at `output`).
pub async fn run_stage<N: Net>(cx: &StageCx<'_, N>, body: &mut Value, dialect: Dialect) -> StageOutcome {
    let n = cx.defs.len();
    let mut results: Vec<Option<Evaluation>> = vec![None; n];
    let mut out = StageOutcome::default();
    let pass = |def: &Arc<Def>, started: Instant| Evaluation {
        duration: started.elapsed(),
        verdict: Verdict::Pass,
        ..skipped(def, cx.stage)
    };
    if cx.stage == GuardStage::Input {
        let started = Instant::now();
        match transforms::templates(cx.defs, body, dialect) {
            Ok(None) => {}
            Ok(Some(changed)) => {
                out.changed |= changed;
                for (i, def) in cx.defs.iter().enumerate() {
                    if matches!(def.check, Check::Template(_))
                        && let Some(slot) = results.get_mut(i)
                    {
                        *slot = Some(pass(def, started));
                    }
                }
            }
            Err(e) => {
                out.template = Some(e);
                return out;
            }
        }
    }
    let mut operator_ptrs = Vec::new();
    for (i, def) in cx.defs.iter().enumerate() {
        let started = Instant::now();
        let ptrs = match &def.check {
            Check::SystemPrompt(sp) if cx.stage == GuardStage::Input => {
                transform::system_prompt(body, dialect, sp)
            }
            Check::Decorator(d) if cx.stage == GuardStage::Input => transform::decorate(body, dialect, d),
            Check::JsonRepair if cx.stage == GuardStage::Output => {
                if cx.json_output {
                    out.changed |= transforms::repair_content(body, dialect);
                }
                Vec::new()
            }
            Check::Pii(_) if phase(def, cx.stage) == Phase::Restore => {
                if let Some(slot) = results.get_mut(i) {
                    *slot = Some(pass(def, started));
                }
                continue;
            }
            _ => continue,
        };
        let edits = !matches!(def.check, Check::JsonRepair);
        operator_ptrs.extend(ptrs);
        out.changed |= edits;
        if let Some(slot) = results.get_mut(i) {
            *slot = Some(Evaluation {
                duration: started.elapsed(),
                verdict: Verdict::Pass,
                ..skipped(def, cx.stage)
            });
        }
    }
    for (i, def) in cx.defs.iter().enumerate() {
        let Check::ToolPermission(rules) = &def.check else {
            continue;
        };
        if phase(def, cx.stage) != Phase::RemoveTools {
            continue;
        }
        let started = Instant::now();
        let removed = rules.remove_refused(body, dialect);
        out.changed |= !removed.is_empty();
        if let Some(slot) = results.get_mut(i) {
            *slot = Some(Evaluation {
                duration: started.elapsed(),
                verdict: if removed.is_empty() {
                    Verdict::Pass
                } else {
                    Verdict::Redact
                },
                category: (!removed.is_empty()).then_some(Category::ToolNotAllowed),
                detail: (!removed.is_empty()).then(|| removed.join(",")),
                ..skipped(def, cx.stage)
            });
        }
    }
    let mut work = Work {
        segments: Vec::new(),
        last_user: None,
        edited: Vec::new(),
        body,
        dialect,
        stage: cx.stage,
        operator_ptrs,
    };
    work.extract();
    if let Some(t) = cx.tokens
        && cx.stage == GuardStage::Input
        && cx.defs.iter().any(|d| d.reversible())
    {
        t.saw(work.segments.iter().map(|s| s.text.as_str()));
    }
    let mut failed = false;
    for (i, def) in cx.defs.iter().enumerate() {
        if phase(def, cx.stage) != Phase::Redact {
            continue;
        }
        if failed {
            if let Some(slot) = results.get_mut(i) {
                *slot = Some(skipped(def, cx.stage));
            }
            continue;
        }
        if def.reads_body() {
            out.changed |= work.flush();
        }
        let scoped = work.scoped(def);
        let subject = Subject {
            segs: &scoped,
            body: Some((&*work.body, dialect)),
        };
        let (eval, edits, replacement) = evaluate_scoped(def, cx, subject).await;
        if let Some(new_body) = replacement {
            *work.body = new_body;
            out.changed = true;
            work.extract();
        }
        for (j, text) in edits {
            if let Some(seg) = work.segments.get_mut(j) {
                seg.text = text;
            }
            if let Some(e) = work.edited.get_mut(j) {
                *e = true;
            }
        }
        failed = eval.verdict.fails();
        if let Some(slot) = results.get_mut(i) {
            *slot = Some(eval);
        }
    }
    out.changed |= work.flush();
    let checks: Vec<usize> = (0..n)
        .filter(|&i| cx.defs.get(i).is_some_and(|d| phase(d, cx.stage) == Phase::Check))
        .collect();
    if !failed {
        run_checks(cx, &checks, &work, &mut results).await;
    }
    let stage_failed = results
        .iter()
        .any(|r| r.as_ref().is_some_and(|e| e.verdict.fails()));
    let mut ran = vec![true; n];
    for (i, def) in cx.defs.iter().enumerate() {
        let Some(slot) = results.get_mut(i) else { continue };
        if slot.is_some() {
            continue;
        }
        match phase(def, cx.stage) {
            Phase::Async if !stage_failed => {
                *slot = Some(Evaluation {
                    verdict: Verdict::Async,
                    ..skipped(def, cx.stage)
                });
                out.asyncs.push(work.deferred(def));
            }
            Phase::Parallel if !stage_failed => {
                out.parallel.push(work.deferred(def));
                if let Some(r) = ran.get_mut(i) {
                    *r = false;
                }
            }
            _ => *slot = Some(skipped(def, cx.stage)),
        }
    }
    out.evals = results
        .into_iter()
        .zip(ran)
        .filter_map(|(r, ran)| r.filter(|_| ran))
        .collect();
    out.failed = out.evals.iter().position(|e| e.verdict.fails());
    if out.failed.is_none() {
        out.divert = out
            .evals
            .iter()
            .find(|e| e.verdict == Verdict::Divert)
            .and_then(|e| e.def.divert_to.clone());
    }
    if cx.stage == GuardStage::Input {
        strip_marks(work.body);
    }
    out
}

/// Phase C: blocking checks, sequential or concurrent; once guardrail `i` fails, only those ordered
/// before it are awaited.
async fn run_checks<N: Net>(
    cx: &StageCx<'_, N>,
    checks: &[usize],
    work: &Work<'_>,
    results: &mut [Option<Evaluation>],
) {
    let body = Some((&*work.body, work.dialect));
    let scoped: Vec<Vec<(usize, &Segment)>> = cx
        .defs
        .iter()
        .enumerate()
        .map(|(i, d)| {
            if checks.contains(&i) {
                work.scoped(d)
            } else {
                Vec::new()
            }
        })
        .collect();
    let subject = |i: usize| Subject {
        segs: scoped.get(i).map_or(&[][..], Vec::as_slice),
        body,
    };
    if cx.sequential {
        for &i in checks {
            let Some(def) = cx.defs.get(i) else { continue };
            let (eval, _, _) = evaluate_scoped(def, cx, subject(i)).await;
            let stop = eval.verdict.fails();
            if let Some(slot) = results.get_mut(i) {
                *slot = Some(eval);
            }
            if stop {
                return;
            }
        }
        return;
    }
    let mut pending = FuturesUnordered::new();
    for &i in checks {
        let Some(def) = cx.defs.get(i) else { continue };
        if def.networked() {
            let s = subject(i);
            pending.push(async move { (i, evaluate_scoped(def, cx, s).await.0) });
        } else if let Some(slot) = results.get_mut(i) {
            *slot = Some(evaluate_scoped(def, cx, subject(i)).await.0);
        }
    }
    loop {
        let first_fail = checks.iter().copied().find(|&i| {
            results
                .get(i)
                .is_some_and(|r| r.as_ref().is_some_and(|e| e.verdict.fails()))
        });
        if let Some(f) = first_fail
            && checks
                .iter()
                .filter(|&&j| j < f)
                .all(|&j| results.get(j).is_some_and(Option::is_some))
        {
            return;
        }
        match pending.next().await {
            Some((i, e)) => {
                if let Some(slot) = results.get_mut(i) {
                    *slot = Some(e);
                }
            }
            None => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::compile::compile;

    struct NoNet;

    impl Net for NoNet {
        async fn webhook(
            &self,
            _: &crate::def::Webhook,
            _: Vec<u8>,
            _: Duration,
        ) -> Result<(u16, Vec<u8>), String> {
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok((200, br#"{"verdict":"block","category":"pii"}"#.to_vec()))
        }

        async fn moderate(&self, _: &str, _: Vec<String>, _: Duration) -> Result<(u16, Vec<u8>), String> {
            Err("unreachable".into())
        }

        async fn judge(&self, _: &crate::services::Judge, _: String, _: Duration) -> Result<String, String> {
            Ok(r#"{"passed": false, "reason": "x"}"#.into())
        }

        async fn post(&self, _: crate::services::ServiceCall, _: Duration) -> Result<(u16, Vec<u8>), String> {
            Ok((200, br#"[[{"label": "jailbreak", "score": 0.95}]]"#.to_vec()))
        }

        async fn chat(&self, _: &str, _: &str, user: &str, _: u32, _: Duration) -> Result<String, String> {
            Ok(user.replace("secret", "[gone]"))
        }

        async fn embed(&self, _: &str, inputs: Vec<String>, _: Duration) -> Result<Vec<Vec<f32>>, String> {
            Ok(inputs
                .iter()
                .map(|t| {
                    if t.contains("weather") {
                        vec![1.0, 0.0]
                    } else {
                        vec![0.0, 1.0]
                    }
                })
                .collect())
        }
    }

    fn defs(doc: &Value, names: &[&str]) -> Vec<Arc<Def>> {
        let g = compile(doc).unwrap();
        names.iter().map(|n| Arc::clone(&g.defs[*n])).collect()
    }

    #[tokio::test]
    async fn redaction_runs_before_checks_and_the_first_failure_is_reported() {
        let doc = json!({"guardrails": {"definitions": {
            "red": {"type": "regex", "action": "redact", "regex": {"deny": ["(?i)falcon"]}},
            "kw": {"type": "keywords", "keywords": {"words": ["falcon"]}},
            "hook": {"type": "webhook", "webhook": {"url": "https://h"}},
            "late": {"type": "regex", "regex": {"deny": ["project"]}}
        }}});
        let list = defs(&doc, &["kw", "red", "hook", "late"]);
        let facts = CallFacts::default();
        let cx = StageCx {
            stage: GuardStage::Input,
            defs: &list,
            sequential: false,
            hash_key: None,
            facts: &facts,
            net: &NoNet,
            request_schema: None,
            tokens: None,
            json_output: false,
        };
        let mut body = json!({"messages": [{"role": "user", "content": "project falcon"}]});
        let out = run_stage(&cx, &mut body, Dialect::Chat).await;
        assert_eq!(body["messages"][0]["content"], "project [REDACTED]");
        let verdicts: Vec<(&str, Verdict)> = out
            .evals
            .iter()
            .map(|e| (e.def.name.as_str(), e.verdict))
            .collect();
        assert_eq!(
            verdicts,
            [
                ("kw", Verdict::Pass),
                ("red", Verdict::Redact),
                ("hook", Verdict::Block),
                ("late", Verdict::Block)
            ]
        );
        assert_eq!(out.failed, Some(2));
    }

    fn cx<'a>(
        list: &'a [Arc<Def>],
        stage: GuardStage,
        facts: &'a CallFacts,
        tokens: Option<&'a Tokenizer>,
    ) -> StageCx<'a, NoNet> {
        StageCx {
            stage,
            defs: list,
            sequential: false,
            hash_key: None,
            facts,
            net: &NoNet,
            request_schema: None,
            tokens,
            json_output: true,
        }
    }

    #[tokio::test]
    async fn template_rewrite_pii_and_topic_work_in_one_input_stage() {
        let doc = json!({"models": {"safe": {}, "e": {}, "rw": {}}, "guardrails": {"definitions": {
            "t": {"type": "template", "template": {"templates": {"ask": {"messages": [
                {"role": "user", "content": "secret: {{q}} mail a@b.co"}]}}}},
            "rw": {"type": "llm_rewrite", "llm_rewrite": {"model": "rw", "prompt": "clean it"}},
            "p": {"type": "pii", "pii": {"entities": ["email"], "reversible": true}},
            "top": {"type": "topic", "action": "divert", "divert_to": "safe",
                    "topic": {"embedding_model": "e", "denied": ["the weather"], "threshold": 0.9}}
        }}});
        let list = defs(&doc, &["t", "rw", "p", "top"]);
        let facts = CallFacts::default();
        let tokens = Tokenizer::default();
        let cx = cx(&list, GuardStage::Input, &facts, Some(&tokens));
        let mut body = json!({"template": {"name": "ask", "variables": {"q": "weather?"}}});
        let out = run_stage(&cx, &mut body, Dialect::Chat).await;
        assert_eq!(body["messages"][0]["content"], "[gone]: weather? mail [EMAIL_1]");
        assert!(body.get("template").is_none());
        let verdicts: Vec<Verdict> = out.evals.iter().map(|e| e.verdict).collect();
        assert_eq!(
            verdicts,
            [Verdict::Pass, Verdict::Redact, Verdict::Redact, Verdict::Divert]
        );
        assert_eq!(out.divert.as_deref(), Some("safe"));
        let back = tokens.finish().unwrap();
        assert_eq!(back.restore("to [EMAIL_1]").as_deref(), Some("to a@b.co"));
        let mut body = json!({"template": {"name": "other"}});
        let out = run_stage(&cx, &mut body, Dialect::Chat).await;
        assert_eq!(out.template, Some(TemplateError::Unknown("other".into())));
    }

    #[tokio::test]
    async fn output_repairs_json_and_leaves_tokens_to_the_caller() {
        let doc = json!({"guardrails": {"definitions": {
            "fix": {"type": "json_repair"},
            "p": {"type": "pii", "pii": {"reversible": true}}
        }}});
        let list = defs(&doc, &["fix", "p"]);
        let facts = CallFacts::default();
        let cx = cx(&list, GuardStage::Output, &facts, None);
        let mut body = json!({"choices": [{"index": 0, "message": {"role": "assistant",
            "content": "```json\n{\"a\": \"[EMAIL_1]\",}\n```"}}]});
        let out = run_stage(&cx, &mut body, Dialect::Chat).await;
        assert!(out.changed);
        assert_eq!(body["choices"][0]["message"]["content"], "{\"a\": \"[EMAIL_1]\"}");
        assert!(out.evals.iter().all(|e| e.verdict == Verdict::Pass));
    }
}
