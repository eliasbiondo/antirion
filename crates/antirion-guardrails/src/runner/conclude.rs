//! What one guardrail concludes about what it inspects: the text segments in its scope, or the
//! whole body for the checks that read it.

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{GRACE, Located, Net, StageCx, Verdict};
use crate::def::{Action, Category, Check, Def, GuardStage, SegKind};
use crate::pattern::Span;
use crate::segments::{Dialect, Segment};
use crate::structured::choice_contents;
use crate::webhook::Said;
use crate::{moderation, partners, redact, secrets, services, webhook};

/// What a check inspects: the segments in its scope (with their index in the stage's list), and
/// the body as it stands.
#[derive(Clone, Copy, Debug)]
pub struct Subject<'a> {
    pub segs: &'a [(usize, &'a Segment)],
    pub body: Option<(&'a Value, Dialect)>,
}

/// A finding of an in-process check: spans per segment index, and its category.
#[derive(Clone, Debug)]
pub struct Found {
    pub spans: Vec<(usize, Vec<Span>)>,
    pub category: Category,
    pub detail: String,
}

/// The spans an in-process check finds in one text.
#[must_use]
pub fn spans_in(def: &Def, text: &str) -> Vec<Span> {
    match &def.check {
        Check::Regex(r) => r.deny_spans(text),
        Check::Keywords(k) => k.spans(text),
        Check::Pii(p) => p.entities.iter().flat_map(|e| e.spans(text)).collect(),
        Check::Secrets(r) => r.spans(text),
        _ => Vec::new(),
    }
}

fn category_of(def: &Def) -> Category {
    match def.check {
        Check::Regex(_) => Category::DenyPattern,
        Check::Keywords(_) => Category::Keyword,
        Check::Pii(_) => Category::Pii,
        Check::Secrets(_) => Category::Secret,
        _ => Category::Custom,
    }
}

/// Runs an in-process check over the segments in scope (`(index, segment)` pairs).
#[must_use]
pub fn scan(def: &Def, stage: GuardStage, segs: &[(usize, &Segment)]) -> Option<Found> {
    let mut spans = Vec::new();
    for (i, seg) in segs {
        let found = spans_in(def, &seg.text);
        if !found.is_empty() {
            spans.push((*i, redact::merge(found)));
        }
    }
    if let Some((_, first)) = spans.first() {
        let detail = first
            .first()
            .map(|s| s.label.to_ascii_lowercase())
            .unwrap_or_default();
        return Some(Found {
            spans,
            category: category_of(def),
            detail,
        });
    }
    if let Check::Regex(r) = &def.check
        && r.has_allow()
    {
        let target = if stage == GuardStage::Input {
            SegKind::User
        } else {
            SegKind::Content
        };
        if segs.iter().any(|(_, s)| s.kind == target && !r.allows(&s.text)) {
            return Some(Found {
                spans: Vec::new(),
                category: Category::AllowPattern,
                detail: "allow".to_owned(),
            });
        }
    }
    None
}

/// What one check concluded, before timing.
pub(super) struct Concluded {
    pub(super) verdict: Verdict,
    pub(super) category: Option<Category>,
    pub(super) detail: Option<String>,
    /// New texts by segment index (redactions and webhook mutations).
    pub(super) edits: Vec<(usize, String)>,
    pub(super) spans: Vec<Located>,
    /// A body-mode webhook's replacement of the whole body.
    pub(super) replacement: Option<Value>,
    pub(super) score: Option<f64>,
    pub(super) confidence: f64,
    pub(super) digest: Option<[u8; 32]>,
    pub(super) exposed: Vec<antirion_core::Secret>,
}

impl Concluded {
    pub(super) fn pass() -> Concluded {
        Concluded {
            verdict: Verdict::Pass,
            category: None,
            detail: None,
            edits: Vec::new(),
            spans: Vec::new(),
            replacement: None,
            score: None,
            confidence: 1.0,
            digest: None,
            exposed: Vec::new(),
        }
    }

    /// A finding with the score a service gave it.
    pub(super) fn scored(mut self, score: Option<f64>, confidence: f64) -> Concluded {
        self.score = score;
        self.confidence = confidence.clamp(0.0, 1.0);
        self
    }

    pub(super) fn failure(def: &Def, why: String) -> Concluded {
        Concluded {
            verdict: if def.fail_open {
                Verdict::ErrorOpen
            } else {
                Verdict::Error
            },
            category: Some(Category::Error),
            detail: Some(why),
            ..Concluded::pass()
        }
    }

    pub(super) fn finding(def: &Def, category: Category, detail: String) -> Concluded {
        Concluded {
            verdict: match def.action {
                Action::Block => Verdict::Block,
                Action::Flag => Verdict::Flag,
                Action::Redact => Verdict::Redact,
                Action::Divert => Verdict::Divert,
            },
            category: Some(category),
            detail: Some(detail),
            ..Concluded::pass()
        }
    }

    /// A finding that cannot redact: under `redact` it blocks.
    pub(super) fn finding_unredactable(def: &Def, category: Category, detail: String) -> Concluded {
        let mut c = Concluded::finding(def, category, detail);
        if c.verdict == Verdict::Redact {
            c.verdict = Verdict::Block;
        }
        c
    }
}

pub(super) async fn conclude<N: Net>(def: &Def, cx: &StageCx<'_, N>, subject: Subject<'_>) -> Concluded {
    let segs = subject.segs;
    match &def.check {
        Check::Webhook(_)
        | Check::Moderation(_)
        | Check::Judge(_)
        | Check::Classifier(_)
        | Check::Partner(_)
        | Check::Rewrite(_)
        | Check::Topic(_) => {
            let permit = match &def.limiter {
                Some(l) => match l.try_acquire() {
                    Some(p) => Some(p),
                    None => return Concluded::failure(def, "max_concurrency reached".to_owned()),
                },
                None => None,
            };
            let c = networked(def, cx, subject).await;
            drop(permit);
            c
        }
        Check::Length(l) => {
            let found = match (cx.stage, subject.body) {
                (GuardStage::Output, Some((body, d))) => choice_contents(body, d)
                    .iter()
                    .find_map(|text| l.breach(&[text.as_str()])),
                _ => {
                    let texts: Vec<&str> = segs.iter().map(|(_, s)| s.text.as_str()).collect();
                    l.breach(&texts)
                }
            };
            found.map_or_else(Concluded::pass, |detail| {
                Concluded::finding_unredactable(def, Category::Length, detail)
            })
        }
        Check::ToolPermission(rules) => {
            let Some((body, d)) = subject.body else {
                return Concluded::pass();
            };
            let refused = match cx.stage {
                GuardStage::Input => rules.refused_declaration(body, d),
                GuardStage::Output => rules.refused_call(body, d),
            };
            refused.map_or_else(Concluded::pass, |name| {
                Concluded::finding_unredactable(def, Category::ToolNotAllowed, name)
            })
        }
        Check::JsonSchema(j) => {
            let Some((body, d)) = subject.body else {
                return Concluded::pass();
            };
            match j.verdict(body, d, cx.request_schema) {
                Ok(Some((category, detail))) => Concluded::finding_unredactable(def, category, detail),
                Ok(None) => Concluded::pass(),
                Err(e) => Concluded::failure(def, format!("the request's schema cannot be used: {e}")),
            }
        }
        _ => in_process(def, cx, segs),
    }
}

/// Webhook and moderation checks, under their timeout.
async fn networked<N: Net>(def: &Def, cx: &StageCx<'_, N>, subject: Subject<'_>) -> Concluded {
    let segs = subject.segs;
    match &def.check {
        Check::Webhook(hook) => {
            let body = match (hook.body_mode, subject.body) {
                (true, Some((b, _))) => webhook::body_request(&def.name, cx.stage, cx.facts, b),
                (true, None) => return Concluded::pass(),
                (false, _) => {
                    let refs: Vec<&Segment> = segs.iter().map(|(_, s)| *s).collect();
                    webhook::request_body(&def.name, cx.stage, cx.facts, &refs)
                }
            };
            let answer =
                tokio::time::timeout(def.timeout + GRACE, cx.net.webhook(hook, body, def.timeout)).await;
            let answer = match answer {
                Err(_) => return Concluded::failure(def, "timeout".to_owned()),
                Ok(Err(e)) => return Concluded::failure(def, e),
                Ok(Ok((status, body))) => webhook::parse_answer(status, &body),
            };
            match answer {
                Ok(a) => answered(def, a, segs),
                Err(e) => Concluded::failure(def, e),
            }
        }
        Check::Moderation(m) => {
            let inputs: Vec<String> = segs
                .iter()
                .flat_map(|(_, s)| moderation::windows(&s.text, m.max_chars, m.overlap_chars))
                .collect();
            if inputs.is_empty() {
                return Concluded::pass();
            }
            let calls = inputs.chunks(m.max_inputs_per_call).map(|batch| {
                let batch = batch.to_vec();
                let n = batch.len();
                async move {
                    let r = tokio::time::timeout(
                        def.timeout + GRACE,
                        cx.net.moderate(&m.model, batch, def.timeout),
                    )
                    .await;
                    match r {
                        Err(_) => Err("timeout".to_owned()),
                        Ok(Err(e)) => Err(e),
                        Ok(Ok((status, body))) => moderation::results(status, &body, n),
                    }
                }
            });
            let mut all = Vec::new();
            for r in futures_util::future::join_all(calls).await {
                match r {
                    Ok(rs) => all.extend(rs),
                    Err(e) => return Concluded::failure(def, e),
                }
            }
            match moderation::finding(&all, &m.thresholds) {
                Some((category, detail)) => {
                    let score = moderation::top_score(&all, &detail);
                    Concluded::finding(def, category, detail).scored(score, score.unwrap_or(1.0))
                }
                None => Concluded::pass(),
            }
        }
        Check::Judge(j) => {
            let texts: Vec<&str> = segs.iter().map(|(_, s)| s.text.as_str()).collect();
            if texts.is_empty() {
                return Concluded::pass();
            }
            let asked = cx.net.judge(j, j.message(&texts), def.timeout);
            match tokio::time::timeout(def.timeout + GRACE, asked).await {
                Err(_) => Concluded::failure(def, "timeout".to_owned()),
                Ok(Err(e)) => Concluded::failure(def, e),
                Ok(Ok(answer)) => match services::judged(&answer) {
                    Ok(true) => Concluded::pass(),
                    Ok(false) => Concluded::finding_unredactable(def, Category::Custom, "judge".to_owned()),
                    Err(e) => Concluded::failure(def, e),
                },
            }
        }
        Check::Classifier(c) => classify(def, cx, c, segs).await,
        Check::Partner(p) => partner(def, cx, p, segs).await,
        Check::Rewrite(r) => super::asked::rewrite(def, cx, r, segs).await,
        Check::Topic(t) => super::asked::topic(def, cx, t, segs).await,
        _ => Concluded::pass(),
    }
}

/// Sends the windows of every segment to the classifier, in batches.
async fn classify<N: Net>(
    def: &Def,
    cx: &StageCx<'_, N>,
    c: &services::Classifier,
    segs: &[(usize, &Segment)],
) -> Concluded {
    let inputs: Vec<String> = segs
        .iter()
        .flat_map(|(_, s)| moderation::windows(&s.text, c.max_chars, c.overlap_chars))
        .collect();
    if inputs.is_empty() {
        return Concluded::pass();
    }
    let calls = inputs.chunks(c.max_inputs_per_call).map(|batch| async move {
        let call = services::ServiceCall {
            url: c.url.clone(),
            headers: c.headers.clone(),
            body: services::Classifier::body(batch),
            auth: services::ServiceAuth::None,
        };
        match tokio::time::timeout(def.timeout + GRACE, cx.net.post(call, def.timeout)).await {
            Err(_) => Err("timeout".to_owned()),
            Ok(Err(e)) => Err(e),
            Ok(Ok((status, body))) => c.read(status, &body, batch.len(), &def.categories),
        }
    });
    let mut best: Option<services::Classified> = None;
    for r in futures_util::future::join_all(calls).await {
        match r {
            Ok(Some(found)) if best.as_ref().is_none_or(|b| found.score > b.score) => best = Some(found),
            Ok(_) => {}
            Err(e) => return Concluded::failure(def, e),
        }
    }
    match best {
        Some(b) => Concluded::finding_unredactable(def, b.category, b.label).scored(Some(b.score), b.score),
        None => Concluded::pass(),
    }
}

/// Calls a partner service with the segments' texts and reads its verdict.
async fn partner<N: Net>(
    def: &Def,
    cx: &StageCx<'_, N>,
    p: &partners::Partner,
    segs: &[(usize, &Segment)],
) -> Concluded {
    let texts: Vec<&str> = segs.iter().map(|(_, s)| s.text.as_str()).collect();
    if texts.is_empty() {
        return Concluded::pass();
    }
    let calls = p.calls(cx.stage, &texts).into_iter().map(|call| async move {
        match tokio::time::timeout(def.timeout + GRACE, cx.net.post(call, def.timeout)).await {
            Err(_) => Err("timeout".to_owned()),
            Ok(r) => r,
        }
    });
    let mut answers = Vec::new();
    for r in futures_util::future::join_all(calls).await {
        match r {
            Ok(a) => answers.push(a),
            Err(e) => return Concluded::failure(def, e),
        }
    }
    match p.read(&answers, texts.len()) {
        Err(e) => Concluded::failure(def, e),
        Ok(partners::Said::Pass) => Concluded::pass(),
        Ok(partners::Said::Found {
            category,
            detail,
            score,
            confidence,
        }) => Concluded::finding_unredactable(def, category, detail).scored(score, confidence),
        Ok(partners::Said::Anonymized(outputs)) if def.action == Action::Redact => Concluded {
            verdict: Verdict::Redact,
            category: Some(Category::Pii),
            detail: Some("anonymized".to_owned()),
            edits: segs.iter().map(|(i, _)| *i).zip(outputs).collect(),
            ..Concluded::pass()
        },
        Ok(partners::Said::Anonymized(_)) => {
            Concluded::finding_unredactable(def, Category::Pii, "anonymized".to_owned())
        }
    }
}

/// A digest of what matched: each match with only its letters and digits, lowercased, sorted and
/// deduplicated, so the same value found again (in other spacing or case) digests the same.
fn digest_of(segs: &[(usize, &Segment)], spans: &[(usize, Vec<Span>)]) -> Option<[u8; 32]> {
    let mut parts: Vec<String> = spans
        .iter()
        .filter_map(|(i, list)| Some((segs.iter().find(|(j, _)| j == i)?.1, list)))
        .flat_map(|(seg, list)| {
            list.iter().filter_map(|s| {
                let m = seg.text.get(s.start..s.end)?;
                Some(
                    m.chars()
                        .filter(|c| c.is_alphanumeric())
                        .flat_map(char::to_lowercase)
                        .collect::<String>(),
                )
            })
        })
        .collect();
    if parts.is_empty() {
        return None;
    }
    parts.sort_unstable();
    parts.dedup();
    let mut h = Sha256::new();
    for p in &parts {
        h.update(p.as_bytes());
        h.update([0x1f]);
    }
    Some(h.finalize().into())
}

/// The gateway virtual keys among the matches.
fn exposed_keys(segs: &[(usize, &Segment)], spans: &[(usize, Vec<Span>)]) -> Vec<antirion_core::Secret> {
    let mut out: Vec<&str> = spans
        .iter()
        .filter_map(|(i, list)| Some((segs.iter().find(|(j, _)| j == i)?.1, list)))
        .flat_map(|(seg, list)| {
            list.iter()
                .filter(|s| s.label == secrets::GATEWAY_KEY)
                .filter_map(|s| seg.text.get(s.start..s.end))
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out.into_iter().map(antirion_core::Secret::new).collect()
}

/// A webhook's answer, as a verdict: mutations (segment mode) and a replacement body (body mode)
/// are used only under `redact`.
fn answered(def: &Def, a: webhook::Answer, segs: &[(usize, &Segment)]) -> Concluded {
    let category = a.category.as_deref().map_or(Category::Custom, |c| {
        Category::parse(c)
            .or_else(|| def.categories.get(c).copied())
            .unwrap_or(Category::Custom)
    });
    let detail = a.category.clone().unwrap_or_default();
    match a.verdict {
        Said::Block => Concluded::finding_unredactable(def, category, detail),
        Said::Flag => Concluded {
            verdict: Verdict::Flag,
            ..Concluded::finding(def, category, detail)
        },
        Said::Pass if def.action == Action::Redact && a.body.is_some() => Concluded {
            verdict: Verdict::Redact,
            replacement: a.body,
            ..Concluded::pass()
        },
        Said::Pass if def.action == Action::Redact && !a.mutations.is_empty() => {
            let mut edits = Vec::new();
            for (id, text) in a.mutations {
                let n = id.strip_prefix('s').and_then(|n| n.parse::<usize>().ok());
                let Some((index, _)) = n.and_then(|n| segs.get(n)) else {
                    return Concluded::failure(def, format!("mutation of unknown segment {id}"));
                };
                if edits.iter().any(|(i, _)| i == index) {
                    return Concluded::failure(def, format!("segment {id} mutated twice"));
                }
                edits.push((*index, text));
            }
            Concluded {
                verdict: Verdict::Redact,
                edits,
                ..Concluded::pass()
            }
        }
        Said::Pass => Concluded::pass(),
    }
}

/// Regex, keyword, PII and secret checks: findings with their spans, and redactions under
/// `redact`.
fn in_process<N: Net>(def: &Def, cx: &StageCx<'_, N>, segs: &[(usize, &Segment)]) -> Concluded {
    let Some(found) = scan(def, cx.stage, segs) else {
        return Concluded::pass();
    };
    let mut c = Concluded::finding(def, found.category, found.detail);
    c.digest = digest_of(segs, &found.spans);
    if matches!(def.check, Check::Secrets(_)) {
        c.exposed = exposed_keys(segs, &found.spans);
    }
    c.spans = found
        .spans
        .iter()
        .filter_map(|(i, spans)| Some((*i, spans, &segs.iter().find(|(j, _)| j == i)?.1.text)))
        .flat_map(|(i, spans, text)| {
            let chars = |b: usize| text.get(..b).map_or(0, |s| s.chars().count());
            spans.iter().map(move |s| Located {
                segment: i,
                start: chars(s.start),
                end: chars(s.end),
                entity: s.label.clone(),
            })
        })
        .collect();
    if def.action == Action::Redact
        && let (true, Some(tokens)) = (def.reversible() && cx.stage == GuardStage::Input, cx.tokens)
    {
        c.edits = found
            .spans
            .iter()
            .filter_map(|(i, spans)| {
                let seg = segs.iter().find(|(j, _)| j == i)?.1;
                Some((*i, tokens.apply(&seg.text, spans)))
            })
            .collect();
    } else if def.action == Action::Redact {
        c.edits = found
            .spans
            .iter()
            .filter_map(|(i, spans)| {
                let seg = segs.iter().find(|(j, _)| j == i)?.1;
                Some((*i, redact::apply(&seg.text, spans, def.style(cx.hash_key))))
            })
            .collect();
    }
    c
}
