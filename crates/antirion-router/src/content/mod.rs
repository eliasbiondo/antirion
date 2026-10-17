//! Content-based decisions taken with a call outside the route: the embedding, LLM and external
//! classifiers, and the scorers of cost-quality nodes. The router holds what each node asks for,
//! reads the answers and keeps the calibration windows; the data plane makes the calls, within each
//! node's time budget, and hands the decisions to the plan.

use std::time::Duration;

use crate::classifier::{Input, Matcher};

pub mod cache;
mod calibrate;
mod continuation;
pub mod mf;
mod reply;

pub use cache::EmbeddingCache;
pub use calibrate::Window;
pub use continuation::is_continuation;
pub use reply::{best_class, cosine, grade, llm_class, prompt_for_classes, prompt_for_grade};

/// An operator webhook: where a classifier or scorer posts its question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Webhook {
    pub url: String,
    pub headers: Vec<(String, String)>,
}

/// An embedding classifier: each class's examples, embedded through `model`.
#[derive(Clone, Debug, PartialEq)]
pub struct Embedding {
    /// The embeddings Model, routed by the gateway itself.
    pub model: String,
    /// The examples of each class, in class order.
    pub examples: Vec<Vec<String>>,
    /// The threshold of each class (its own, else the node's).
    pub thresholds: Vec<f64>,
    pub cache_entries: usize,
}

/// A question to a chat Model, answered in a few tokens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    pub model: String,
    pub max_tokens: u32,
}

/// How a classifier node picks its class.
#[derive(Clone, Debug)]
pub enum Classify {
    /// Keywords or patterns, matched in the request without a call.
    Local(Matcher),
    Embedding(Embedding),
    /// An LLM asked to name a class; the class descriptions, in class order.
    Llm {
        ask: Ask,
        descriptions: Vec<String>,
    },
    External(Webhook),
}

/// A classifier node.
#[derive(Clone, Debug)]
pub struct ClassifierSpec {
    pub how: Classify,
    pub classes: Vec<String>,
    pub input: Input,
    pub timeout: Duration,
    /// `on_error: reject`: a failed call fails the request.
    pub reject_on_error: bool,
    /// `continuation: replay`: a continuation turn reuses the session's previous class.
    pub replay: bool,
}

impl ClassifierSpec {
    /// Whether the decision needs a call.
    #[must_use]
    pub fn calls(&self) -> bool {
        !matches!(self.how, Classify::Local(_))
    }
}

/// The knob of a cost-quality node.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Knob {
    /// A fixed threshold.
    Threshold(f64),
    /// The share of traffic to send to `strong`; the threshold follows the observed scores.
    Fraction(f64),
}

/// How the threshold of a `strong_fraction` node is calibrated.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Calibration {
    pub window: usize,
    pub min_samples: usize,
    /// The threshold before `min_samples` scores.
    pub initial: f64,
}

/// Where a cost-quality node's win rate comes from.
#[derive(Clone, Debug)]
pub enum Scorer {
    /// Uniform in [0, 1).
    Random,
    External(Webhook),
    /// 1 when one of the classes matches, else 0.
    Classifier(Matcher),
    /// A judge Model's grade from 1 to 5, as (g − 1) / 4.
    Llm(Ask),
    /// A pre-trained matrix factorisation over the routing text's embedding.
    MatrixFactorization(std::sync::Arc<mf::MatrixFactorization>),
}

/// What a cost-quality node does when its scorer fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnScorerError {
    Strong,
    Weak,
    Reject,
}

/// A cost-quality node: children are `strong` then `weak`.
#[derive(Clone, Debug)]
pub struct CostQualitySpec {
    pub knob: Knob,
    pub calibration: Calibration,
    pub scorer: Scorer,
    pub input: Input,
    pub timeout: Duration,
    pub on_error: OnScorerError,
}

impl CostQualitySpec {
    /// The knob decides alone, with no score: `strong_fraction` 0 or 1.
    #[must_use]
    pub fn fixed(&self) -> Option<bool> {
        match self.knob {
            Knob::Fraction(p) if p <= 0.0 => Some(false),
            Knob::Fraction(p) if p >= 1.0 => Some(true),
            _ => None,
        }
    }

    /// Whether the decision needs a call.
    #[must_use]
    pub fn calls(&self) -> bool {
        self.fixed().is_none()
            && matches!(
                self.scorer,
                Scorer::External(_) | Scorer::Llm(_) | Scorer::MatrixFactorization(_)
            )
    }
}
