//! Classifier and cost-quality nodes: the classes or the strong and weak branches, and what each
//! decision asks (matchers, embedding examples, the Model or webhook to call), checked at config
//! time.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use antirion_config::scalar::parse_duration;
use antirion_config::{DetailCode, DocPath};
use serde_json::{Map, Value};

use super::{Builder, error};
use crate::classifier::{Input, Matcher};
use crate::content::{
    Ask, Calibration, ClassifierSpec, Classify, CostQualitySpec, Embedding, Knob, OnScorerError, Scorer,
    Webhook,
};
use crate::tree::NodeKind;

fn duration(obj: &Map<String, Value>, key: &str, default_ms: u64) -> Duration {
    obj.get(key)
        .and_then(Value::as_str)
        .and_then(parse_duration)
        .unwrap_or(Duration::from_millis(default_ms))
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn webhook(v: &Value) -> Webhook {
    Webhook {
        url: v
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        headers: v
            .get("headers")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
            .collect(),
    }
}

fn ask(v: &Value) -> Ask {
    Ask {
        model: v
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        max_tokens: v
            .get("max_tokens")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(16),
    }
}

impl Builder<'_, '_> {
    fn known_model(&mut self, model: &str, at: &DocPath) {
        if !self.cx.models.contains(model) {
            error(
                self.cx.errors,
                at,
                DetailCode::UnresolvedReference,
                "the Model does not exist",
            );
        }
    }

    /// Keyword or regex matchers of `classes`, from each class's `field` list.
    fn matcher(&mut self, classes: &[Value], regex: bool, at: &DocPath) -> Option<Matcher> {
        let field = if regex { "patterns" } else { "keywords" };
        let lists: Vec<Vec<String>> = classes.iter().map(|c| strings(c.get(field))).collect();
        let built = if regex {
            Matcher::patterns(&lists)
        } else {
            Matcher::keywords(&lists)
        };
        match built {
            Ok(m) => Some(m),
            Err(bad) => {
                error(
                    self.cx.errors,
                    &at.index(bad.class).key(field).index(bad.pattern),
                    DetailCode::InvalidFormat,
                    "not a valid regular expression",
                );
                None
            }
        }
    }

    pub(super) fn classifier(
        &mut self,
        index: usize,
        obj: &Map<String, Value>,
        at: &DocPath,
        path: &str,
        depth: usize,
    ) -> usize {
        let kind = obj.get("kind").and_then(Value::as_str).unwrap_or_default();
        let classes = obj
            .get("classes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let cat = at.key("classes");
        let how = match kind {
            "keyword" | "regex" => match self.matcher(&classes, kind == "regex", &cat) {
                Some(m) => Classify::Local(m),
                None => return index,
            },
            "embedding" => {
                let e = obj.get("embedding").unwrap_or(&Value::Null);
                let model = e
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                self.known_model(&model, &at.key("embedding").key("model"));
                let node_threshold = obj.get("threshold").and_then(Value::as_f64).unwrap_or(0.75);
                Classify::Embedding(Embedding {
                    model,
                    examples: classes.iter().map(|c| strings(c.get("examples"))).collect(),
                    thresholds: classes
                        .iter()
                        .map(|c| {
                            c.get("threshold")
                                .and_then(Value::as_f64)
                                .unwrap_or(node_threshold)
                        })
                        .collect(),
                    cache_entries: e
                        .get("cache_entries")
                        .and_then(Value::as_u64)
                        .and_then(|n| usize::try_from(n).ok())
                        .unwrap_or(10_000),
                })
            }
            "llm" => {
                let a = ask(obj.get("llm").unwrap_or(&Value::Null));
                self.known_model(&a.model, &at.key("llm").key("model"));
                Classify::Llm {
                    ask: a,
                    descriptions: classes
                        .iter()
                        .map(|c| {
                            c.get("description")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned()
                        })
                        .collect(),
                }
            }
            "external" => Classify::External(webhook(obj.get("external").unwrap_or(&Value::Null))),
            _ => {
                if let Some(n) = self.nodes.get_mut(index) {
                    n.kind = NodeKind::Unsupported("classifier");
                }
                return index;
            }
        };
        let mut names = Vec::new();
        let mut children = Vec::new();
        let mut seen = HashSet::new();
        for (i, c) in classes.iter().enumerate() {
            let name = c
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if !seen.insert(name.clone()) {
                error(
                    self.cx.errors,
                    &cat.index(i).key("name"),
                    DetailCode::Conflict,
                    "duplicate class name",
                );
            }
            let target = c.get("target").unwrap_or(&Value::Null);
            children.push(self.node(
                target,
                &cat.index(i).key("target"),
                Some(index),
                Some(name.clone()),
                path,
                depth + 1,
                false,
            ));
            names.push(name);
        }
        let default = obj.get("default").unwrap_or(&Value::Null);
        let seg = default
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("default")
            .to_owned();
        children.push(self.node(
            default,
            &at.key("default"),
            Some(index),
            Some(seg),
            path,
            depth + 1,
            false,
        ));
        if let Some(n) = self.nodes.get_mut(index) {
            n.kind = NodeKind::Classifier(Arc::new(ClassifierSpec {
                how,
                classes: names,
                input: Input::from_value(obj.get("input")),
                timeout: duration(obj, "timeout", 500),
                reject_on_error: obj.get("on_error").and_then(Value::as_str) == Some("reject"),
                replay: obj.get("continuation").and_then(Value::as_str) == Some("replay"),
            }));
            n.children = children;
        }
        self.finish(index);
        index
    }

    fn scorer(&mut self, v: &Value, at: &DocPath) -> Scorer {
        match v.get("kind").and_then(Value::as_str) {
            Some("external") => {
                let w = webhook(v);
                if w.url.is_empty() {
                    error(
                        self.cx.errors,
                        &at.key("url"),
                        DetailCode::Required,
                        "an external scorer needs a url",
                    );
                }
                Scorer::External(w)
            }
            Some("llm") => {
                let a = ask(v);
                self.known_model(&a.model, &at.key("model"));
                Scorer::Llm(a)
            }
            Some("classifier") => {
                let classes = v
                    .get("classes")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if classes.is_empty() {
                    error(
                        self.cx.errors,
                        &at.key("classes"),
                        DetailCode::Required,
                        "a classifier scorer needs classes",
                    );
                }
                let regex = v.get("match").and_then(Value::as_str) == Some("regex");
                self.matcher(&classes, regex, &at.key("classes"))
                    .map_or(Scorer::Random, Scorer::Classifier)
            }
            Some("matrix_factorization") => self.matrix_factorization(v, at),
            _ => Scorer::Random,
        }
    }

    /// A matrix-factorisation scorer: its model file read now, its pair and embeddings Model
    /// checked.
    fn matrix_factorization(&mut self, v: &Value, at: &DocPath) -> Scorer {
        let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
        let pair = |k: &str| {
            v.pointer(&format!("/models/{k}"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let (path, embedding) = (text("model_file"), text("embedding_model"));
        self.known_model(&embedding, &at.key("embedding_model"));
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => {
                error(
                    self.cx.errors,
                    &at.key("model_file"),
                    DetailCode::InvalidFormat,
                    "the model file cannot be read",
                );
                return Scorer::Random;
            }
        };
        match crate::content::mf::MatrixFactorization::read(
            &bytes,
            &pair("strong"),
            &pair("weak"),
            &embedding,
        ) {
            Ok(mf) => Scorer::MatrixFactorization(std::sync::Arc::new(mf)),
            Err(crate::content::mf::MfError::UnknownModel(name)) => {
                let which = if name == pair("strong") { "strong" } else { "weak" };
                error(
                    self.cx.errors,
                    &at.key("models").key(which),
                    DetailCode::UnresolvedReference,
                    "the model file has no model of that name",
                );
                Scorer::Random
            }
            Err(e) => {
                error(
                    self.cx.errors,
                    &at.key("model_file"),
                    DetailCode::InvalidFormat,
                    &e.to_string(),
                );
                Scorer::Random
            }
        }
    }

    pub(super) fn cost_quality(
        &mut self,
        index: usize,
        obj: &Map<String, Value>,
        at: &DocPath,
        path: &str,
        depth: usize,
    ) -> usize {
        let knob = match (
            obj.get("threshold").and_then(Value::as_f64),
            obj.get("strong_fraction").and_then(Value::as_f64),
        ) {
            (Some(t), None) => Knob::Threshold(t),
            (None, Some(p)) => Knob::Fraction(p),
            _ => {
                error(
                    self.cx.errors,
                    at,
                    DetailCode::Conflict,
                    "exactly one of threshold and strong_fraction",
                );
                Knob::Threshold(0.5)
            }
        };
        let c = obj.get("calibration").unwrap_or(&Value::Null);
        let count = |k: &str, d: usize| {
            c.get(k)
                .and_then(Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(d)
        };
        let calibration = Calibration {
            window: count("window", 10_000),
            min_samples: count("min_samples", 200),
            initial: c.get("initial_threshold").and_then(Value::as_f64).unwrap_or(0.5),
        };
        if calibration.min_samples > calibration.window {
            error(
                self.cx.errors,
                &at.key("calibration").key("min_samples"),
                DetailCode::OutOfRange,
                "min_samples is larger than the window",
            );
        }
        let scorer = self.scorer(obj.get("scorer").unwrap_or(&Value::Null), &at.key("scorer"));
        let mut children = Vec::new();
        for branch in ["strong", "weak"] {
            let target = obj.get(branch).unwrap_or(&Value::Null);
            children.push(self.node(
                target,
                &at.key(branch),
                Some(index),
                Some(branch.to_owned()),
                path,
                depth + 1,
                false,
            ));
        }
        if let Some(n) = self.nodes.get_mut(index) {
            n.kind = NodeKind::CostQuality(Arc::new(CostQualitySpec {
                knob,
                calibration,
                scorer,
                input: Input::from_value(obj.get("input")),
                timeout: duration(obj, "timeout", 300),
                on_error: match obj.get("on_error").and_then(Value::as_str) {
                    Some("weak") => OnScorerError::Weak,
                    Some("reject") => OnScorerError::Reject,
                    _ => OnScorerError::Strong,
                },
            }));
            n.children = children;
        }
        self.finish(index);
        index
    }
}
