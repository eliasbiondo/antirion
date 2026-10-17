//! `topic`: the last user message is embedded with a configured Model and compared with the
//! embeddings of the operator's allowed and denied topic descriptions. The descriptions are
//! embedded once per configuration generation, on first use.

use std::sync::{Arc, Mutex};

use antirion_config::{ConfigError, DetailCode, DocPath};
use serde_json::Value;

/// A `topic` guardrail's settings.
#[derive(Debug)]
pub struct Topic {
    /// The public Model that embeds.
    pub embedding_model: String,
    pub allowed: Vec<String>,
    pub denied: Vec<String>,
    pub threshold: f64,
    /// The descriptions' vectors, allowed then denied, once embedded.
    vectors: Mutex<Option<Arc<Vec<Vec<f32>>>>>,
}

/// What a comparison found.
#[derive(Clone, Debug, PartialEq)]
pub enum Judged {
    Pass,
    /// Off topic: the detail (`denied:<i>` or `allowed`) and the similarity that decided.
    OffTopic(String, f64),
}

fn texts(b: Option<&Value>, key: &str) -> Vec<String> {
    b.and_then(|b| b.get(key))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

/// Cosine similarity; 0 for vectors of different lengths or a zero vector.
#[must_use]
pub fn cosine(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (f64::from(*x), f64::from(*y));
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

impl Topic {
    pub(crate) fn compile(
        b: Option<&Value>,
        at: &DocPath,
        doc: &Value,
        errors: &mut Vec<ConfigError>,
    ) -> Option<Topic> {
        let at = at.key("topic");
        let model = b?.get("embedding_model")?.as_str()?.to_owned();
        crate::services::model_exists(doc, &model, &at.key("embedding_model"), errors);
        let (allowed, denied) = (texts(b, "allowed"), texts(b, "denied"));
        if allowed.is_empty() && denied.is_empty() {
            errors.push(ConfigError::new(
                &at,
                DetailCode::Required,
                "a topic guardrail needs allowed or denied topics",
            ));
        }
        Some(Topic {
            embedding_model: model,
            allowed,
            denied,
            threshold: b
                .and_then(|b| b.get("threshold"))
                .and_then(Value::as_f64)
                .unwrap_or(0.5),
            vectors: Mutex::new(None),
        })
    }

    /// The descriptions' vectors, when already embedded.
    #[must_use]
    pub fn known(&self) -> Option<Arc<Vec<Vec<f32>>>> {
        self.vectors.lock().ok()?.clone()
    }

    /// Every description, allowed then denied: what is embedded beside the query the first time.
    #[must_use]
    pub fn descriptions(&self) -> Vec<String> {
        self.allowed.iter().chain(&self.denied).cloned().collect()
    }

    /// Keeps the descriptions' vectors for later requests.
    pub fn remember(&self, vectors: Vec<Vec<f32>>) -> Arc<Vec<Vec<f32>>> {
        let v = Arc::new(vectors);
        if let Ok(mut slot) = self.vectors.lock() {
            *slot = Some(Arc::clone(&v));
        }
        v
    }

    /// What a finding's detail names: the denied topic's description (its first 64 characters), or
    /// `outside allowed topics`.
    #[must_use]
    pub fn label(&self, detail: &str) -> String {
        detail
            .strip_prefix("denied:")
            .and_then(|i| i.parse::<usize>().ok())
            .and_then(|i| self.denied.get(i))
            .map_or_else(
                || "outside allowed topics".to_owned(),
                |d| d.chars().take(64).collect(),
            )
    }

    /// Compares the query's vector with the descriptions' (allowed then denied).
    #[must_use]
    pub fn judge(&self, query: &[f32], described: &[Vec<f32>]) -> Judged {
        let (allowed, denied) = described.split_at(self.allowed.len().min(described.len()));
        let mut worst: Option<(usize, f64)> = None;
        for (i, v) in denied.iter().enumerate() {
            let s = cosine(query, v);
            if s >= self.threshold && worst.is_none_or(|(_, w)| s > w) {
                worst = Some((i, s));
            }
        }
        if let Some((i, s)) = worst {
            return Judged::OffTopic(format!("denied:{i}"), s);
        }
        if !allowed.is_empty() {
            let best = allowed
                .iter()
                .map(|v| cosine(query, v))
                .fold(f64::NEG_INFINITY, f64::max);
            if best < self.threshold {
                return Judged::OffTopic("allowed".to_owned(), best);
            }
        }
        Judged::Pass
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn topic(allowed: &[&str], denied: &[&str], threshold: f64) -> Topic {
        let mut errors = Vec::new();
        let t = Topic::compile(
            Some(&json!({"embedding_model": "e", "allowed": allowed, "denied": denied, "threshold": threshold})),
            &DocPath::root(),
            &json!({"models": {"e": {}}}),
            &mut errors,
        )
        .unwrap();
        assert!(errors.is_empty(), "{errors:?}");
        t
    }

    #[test]
    fn denied_topics_and_missing_allowed_ones_are_findings() {
        let t = topic(&["billing"], &["politics"], 0.8);
        let billing = vec![1.0, 0.0, 0.0];
        let politics = vec![0.0, 1.0, 0.0];
        let described = vec![billing.clone(), politics.clone()];
        assert_eq!(t.judge(&[0.9, 0.1, 0.0], &described), Judged::Pass);
        assert!(matches!(t.judge(&[0.1, 0.9, 0.0], &described), Judged::OffTopic(d, _) if d == "denied:0"));
        assert!(matches!(t.judge(&[0.0, 0.0, 1.0], &described), Judged::OffTopic(d, _) if d == "allowed"));
        assert_eq!(t.label("allowed"), "outside allowed topics");
        assert_eq!(t.label("denied:0"), t.denied[0]);
        let open = topic(&[], &["politics"], 0.8);
        assert_eq!(open.judge(&[0.0, 0.0, 1.0], &[politics]), Judged::Pass);
    }

    #[test]
    fn cosine_handles_degenerate_vectors() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-9);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
        assert_eq!(cosine(&[1.0], &[1.0, 0.0]), 0.0);
    }
}
