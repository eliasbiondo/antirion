//! The decisions of a route taken before its plan is walked: classifiers and scorers that call a
//! Model or a webhook, pools whose endpoints an external picker chooses, and continuation turns
//! that replay a session's earlier class. Every call is bounded by its node's `timeout`. A failure
//! fails open (the default class, the branch the scorer's `on_error` names, or the pool's built-in
//! scheduler, with `routing` degraded and the cause recorded) or, with `reject`, fails the request
//! with `router_unavailable`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use antirion_core::Stage;
use antirion_error::{GatewayError, Message};
use antirion_router::classifier::routing_text;
use antirion_router::content::{
    self, ClassifierSpec, Classify, CostQualitySpec, Embedding, OnScorerError, Scorer, Webhook,
};
use antirion_router::tree::Node;
use antirion_router::{AttemptPlan, NodeKind, RouteRt, SelectCx};
use serde_json::{Value, json};

use crate::ctx::Ctx;
use crate::external::{Post, header_map, post_json};
use crate::infer::Request;
use crate::internal::{Answer, Asker, Question, ask};
use crate::reasons;

/// The largest classifier or scorer webhook answer read.
const MAX_ANSWER: u64 = 1 << 20;

/// What the decisions of one request share.
struct Ahead<'a, 'r> {
    cx: &'a Ctx,
    req: &'a Request<'r>,
    route: &'a RouteRt,
    session: Option<&'a str>,
    /// Routing calls made so far, which number their ledger lines.
    calls: u32,
}

/// Takes every pending decision reachable through the decisions already taken; the open streams of
/// the endpoint pickers asked.
pub(crate) async fn take(
    cx: &Ctx,
    req: &Request<'_>,
    route: &RouteRt,
    plan: &mut AttemptPlan<'_>,
    sel: &SelectCx<'_>,
    session: Option<&str>,
) -> Result<Vec<crate::picker::Session>, GatewayError> {
    let mut ahead = Ahead {
        cx,
        req,
        route,
        session,
        calls: 0,
    };
    let mut pickers = Vec::new();
    loop {
        let pending = plan.pending(sel);
        if pending.is_empty() {
            return Ok(pickers);
        }
        for i in pending {
            let Some(node) = route.tree.node(i) else {
                plan.set_decision(i, usize::MAX);
                continue;
            };
            let child = match &node.kind {
                NodeKind::Classifier(spec) => {
                    let (child, name) = ahead.classify(node, spec).await?;
                    plan.classes.push((node.path.clone(), name));
                    child
                }
                NodeKind::CostQuality(spec) => ahead.score(node, spec).await?,
                NodeKind::Pool(spec) => {
                    let Some(picker) = &spec.picker else {
                        plan.set_picked(i, None);
                        continue;
                    };
                    match crate::picker::pick(cx, req, &route.tree, node, picker).await {
                        Ok((chosen, stream)) => {
                            plan.set_picked(i, Some(&chosen));
                            pickers.push(stream);
                        }
                        Err(_) if picker.fail_open => {
                            crate::routing::degraded(cx, "picker");
                            plan.set_picked(i, None);
                        }
                        Err(_) => return Err(picker_unavailable()),
                    }
                    continue;
                }
                _ => usize::MAX,
            };
            plan.set_decision(i, child);
        }
    }
}

fn router_unavailable() -> GatewayError {
    GatewayError::with_reason(
        reasons::ROUTER_UNAVAILABLE,
        Stage::Route,
        Message::Fixed("The routing classifier of this model is unavailable."),
    )
}

fn picker_unavailable() -> GatewayError {
    GatewayError::with_reason(
        reasons::ROUTER_UNAVAILABLE,
        Stage::Route,
        Message::Fixed("The endpoint picker of this model is unavailable."),
    )
}

/// A uniform draw in [0, 1).
fn unit(bits: u64) -> f64 {
    (bits >> 11) as f64 / (1u64 << 53) as f64
}

/// The answer of a classifier: the class (none for `default`) and its score when it has one.
type Classified = (Option<usize>, Option<f64>);

impl Ahead<'_, '_> {
    fn body(&self) -> Option<&Value> {
        self.req.parsed.doc()
    }

    fn scope(&self) -> String {
        let key = &self.req.auth.key;
        format!("{}/{}", key.organization, key.project)
    }

    fn next_line(&mut self) -> u32 {
        self.calls += 1;
        self.calls
    }

    /// A classifier's decision: the child to follow (the class, or the default after the classes)
    /// and the class name recorded for debugging.
    async fn classify(
        &mut self,
        node: &Node,
        spec: &ClassifierSpec,
    ) -> Result<(usize, String), GatewayError> {
        let default = spec.classes.len();
        let name = |c: usize| {
            spec.classes
                .get(c)
                .cloned()
                .unwrap_or_else(|| "default".to_owned())
        };
        let state = &self.cx.rt.state.routing;
        let replay_key = self
            .session
            .filter(|_| spec.replay)
            .map(|s| format!("{}\u{1f}{}\u{1f}{s}", node.state_key, self.scope()));
        if let (Some(k), Some(body)) = (&replay_key, self.body())
            && content::is_continuation(body)
            && let Some(c) = state.replayed(k, self.cx.rt.now())
        {
            return Ok((c, name(c)));
        }
        let Some(text) = self.body().and_then(|b| routing_text(b, &spec.input)) else {
            return Ok((default, name(default)));
        };
        let answer = match &spec.how {
            Classify::Local(m) => Ok((m.classify(&text), None)),
            Classify::Embedding(e) => {
                tokio::time::timeout(spec.timeout, self.embedded(node, e, &text, spec.timeout))
                    .await
                    .unwrap_or_else(|_| Err("timed out".to_owned()))
            }
            Classify::Llm { ask: q, descriptions } => {
                let system = content::prompt_for_classes(&spec.classes, descriptions);
                let line = self.next_line();
                let call = ask(
                    self.cx,
                    &q.model,
                    Question::Chat {
                        system: &system,
                        user: &text,
                        max_tokens: q.max_tokens,
                    },
                    spec.timeout,
                    Asker::router(line),
                );
                match tokio::time::timeout(spec.timeout, call).await {
                    Ok(Ok(Answer::Text(t))) => Ok((content::llm_class(&t, &spec.classes), None)),
                    Ok(Ok(Answer::Vectors(_))) => Err("the answer is malformed".to_owned()),
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err("timed out".to_owned()),
                }
            }
            Classify::External(w) => {
                let payload = json!({"input": text, "classes": spec.classes, "model": self.req.model_name,
                                     "route": self.route.tree.name});
                match self.webhook(w, &payload, spec.timeout).await {
                    Ok(v) => match v.get("class").and_then(Value::as_str) {
                        Some(c) => Ok((
                            spec.classes.iter().position(|k| k == c),
                            v.get("score").and_then(Value::as_f64),
                        )),
                        None => Err("the answer names no class".to_owned()),
                    },
                    Err(e) => Err(e),
                }
            }
        };
        match answer {
            Ok((class, score)) => {
                let child = class.unwrap_or(default);
                if let Some(k) = replay_key {
                    let until = self.cx.rt.now().saturating_add(self.route.affinity.ttl);
                    state.remember(k, child, until);
                }
                if let Some(s) = score {
                    self.cx.log.update(|r| {
                        if r.class_score.is_none() {
                            r.class_score = Some(s);
                        }
                    });
                }
                Ok((child, name(child)))
            }
            Err(_) if spec.reject_on_error => Err(router_unavailable()),
            Err(_) => {
                crate::routing::degraded(self.cx, "classifier");
                Ok((default, name(default)))
            }
        }
    }

    /// The class of an embedding classifier: the texts not cached yet (the query and any example)
    /// are embedded in one call, then each class scores its best example.
    async fn embedded(
        &mut self,
        node: &Node,
        e: &Embedding,
        text: &str,
        timeout: Duration,
    ) -> Result<Classified, String> {
        let state = Arc::clone(&self.cx.rt.state.routing);
        let cache = state.embeddings();
        let examples: usize = e.examples.iter().map(Vec::len).sum();
        let capacity = e.cache_entries.saturating_add(examples);
        let scope = self.scope();
        let query_key = content::cache::key(&e.model, &scope, text);
        let example_keys: Vec<Vec<u128>> = e
            .examples
            .iter()
            .map(|list| {
                list.iter()
                    .map(|x| content::cache::key(&e.model, "", x))
                    .collect()
            })
            .collect();
        let mut have: HashMap<u128, Arc<[f32]>> = HashMap::new();
        let mut need: Vec<(u128, String)> = Vec::new();
        let wanted = std::iter::once((query_key, text)).chain(
            example_keys
                .iter()
                .flatten()
                .zip(e.examples.iter().flatten())
                .map(|(k, x)| (*k, x.as_str())),
        );
        for (k, t) in wanted {
            if have.contains_key(&k) || need.iter().any(|(n, _)| *n == k) {
                continue;
            }
            match cache.get(&node.state_key, k) {
                Some(v) => {
                    have.insert(k, v);
                }
                None => need.push((k, t.to_owned())),
            }
        }
        if !need.is_empty() {
            let texts: Vec<String> = need.iter().map(|(_, t)| t.clone()).collect();
            let line = self.next_line();
            let Answer::Vectors(vectors) = ask(
                self.cx,
                &e.model,
                Question::Embed(&texts),
                timeout,
                Asker::router(line),
            )
            .await?
            else {
                return Err("the answer is malformed".to_owned());
            };
            for ((k, _), v) in need.into_iter().zip(vectors) {
                let v: Arc<[f32]> = Arc::from(v);
                cache.put(&node.state_key, k, Arc::clone(&v), capacity);
                have.insert(k, v);
            }
        }
        let query = have.get(&query_key).ok_or("the query was not embedded")?;
        let per_class: Vec<Vec<Arc<[f32]>>> = example_keys
            .iter()
            .map(|keys| keys.iter().filter_map(|k| have.get(k).cloned()).collect())
            .collect();
        let (class, score) = content::best_class(query, &per_class, &e.thresholds)
            .ok_or("the vectors do not have the same size")?;
        Ok((class, Some(score)))
    }

    /// POSTs `payload` to an operator webhook and reads a JSON object from a 2xx answer.
    async fn webhook(&self, w: &Webhook, payload: &Value, timeout: Duration) -> Result<Value, String> {
        let post = Post {
            url: &w.url,
            headers: header_map(&w.headers),
            body: serde_json::to_vec(payload).unwrap_or_default(),
            timeout,
            max: MAX_ANSWER,
        };
        let sent = post_json(&self.cx.rt, &self.cx.snap, &self.cx.request_id, post);
        let (status, raw) = match tokio::time::timeout(timeout, sent).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(e)) => return Err(e.text()),
            Err(_) => return Err("timed out".to_owned()),
        };
        if !(200..300).contains(&status) {
            return Err(format!("status {status}"));
        }
        match serde_json::from_slice::<Value>(&raw) {
            Ok(v @ Value::Object(_)) => Ok(v),
            _ => Err("the answer is not a JSON object".to_owned()),
        }
    }

    /// The win rate of a cost-quality node's scorer for `text`.
    async fn win_rate(&mut self, spec: &CostQualitySpec, text: &str) -> Result<f64, String> {
        let w = match &spec.scorer {
            Scorer::Random => Ok(self.cx.rt.with_rng(|r| unit(r.next_u64()))),
            Scorer::Classifier(m) => Ok(if m.classify(text).is_some() { 1.0 } else { 0.0 }),
            Scorer::External(hook) => {
                let payload = json!({"input": text, "classes": [], "model": self.req.model_name,
                                     "route": self.route.tree.name});
                let v = self.webhook(hook, &payload, spec.timeout).await?;
                v.get("score")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| "the answer has no numeric score".to_owned())
            }
            Scorer::Llm(q) => {
                let line = self.next_line();
                let call = ask(
                    self.cx,
                    &q.model,
                    Question::Chat {
                        system: content::prompt_for_grade(),
                        user: text,
                        max_tokens: q.max_tokens,
                    },
                    spec.timeout,
                    Asker::router(line),
                );
                match tokio::time::timeout(spec.timeout, call).await {
                    Ok(Ok(Answer::Text(t))) => {
                        content::grade(&t).ok_or_else(|| "the grade is not 1 to 5".to_owned())
                    }
                    Ok(Ok(Answer::Vectors(_))) => Err("the answer is malformed".to_owned()),
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err("timed out".to_owned()),
                }
            }
            Scorer::MatrixFactorization(mf) => {
                let line = self.next_line();
                let texts = [text.to_owned()];
                let call = ask(
                    self.cx,
                    &mf.embedding_model,
                    Question::Embed(&texts),
                    spec.timeout,
                    Asker::router(line),
                );
                match tokio::time::timeout(spec.timeout, call).await {
                    Ok(Ok(Answer::Vectors(v))) => v
                        .first()
                        .and_then(|e| mf.win_rate(e))
                        .ok_or_else(|| "the embedding does not fit the model file".to_owned()),
                    Ok(Ok(Answer::Text(_))) => Err("the answer is malformed".to_owned()),
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err("timed out".to_owned()),
                }
            }
        }?;
        if w.is_finite() && (0.0..=1.0).contains(&w) {
            Ok(w)
        } else {
            Err("the score is outside [0, 1]".to_owned())
        }
    }

    /// A cost-quality node's decision: 0 for `strong`, 1 for `weak`.
    async fn score(&mut self, node: &Node, spec: &CostQualitySpec) -> Result<usize, GatewayError> {
        let fallback = match spec.on_error {
            OnScorerError::Weak => 1,
            OnScorerError::Strong | OnScorerError::Reject => 0,
        };
        let (branch, w, tau) = if let Some(strong) = spec.fixed() {
            (usize::from(!strong), None, None)
        } else if let Some(text) = self.body().and_then(|b| routing_text(b, &spec.input)) {
            let state = Arc::clone(&self.cx.rt.state.routing);
            match tokio::time::timeout(spec.timeout, self.win_rate(spec, &text)).await {
                Ok(Ok(w)) => {
                    let tau = state.threshold(&node.state_key, spec, Some(w));
                    (usize::from(w < tau), Some(w), Some(tau))
                }
                Ok(Err(_)) | Err(_) => {
                    if spec.on_error == OnScorerError::Reject {
                        return Err(router_unavailable());
                    }
                    crate::routing::degraded(self.cx, "scorer");
                    (fallback, None, None)
                }
            }
        } else {
            (fallback, None, None)
        };
        let debug = json!({"node": node.path, "w": w, "tau": tau,
                           "branch": if branch == 0 { "strong" } else { "weak" }});
        self.cx.log.update(|r| {
            if r.cost_quality.is_none() {
                r.cost_quality = Some(debug);
            }
        });
        Ok(branch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_draws_stay_below_one() {
        assert_eq!(unit(0), 0.0);
        assert!(unit(u64::MAX) < 1.0);
    }
}
