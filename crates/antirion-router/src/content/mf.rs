//! The matrix-factorisation scorer of cost-quality nodes: a pre-trained model file holds an
//! embedding per model `P_m` (normalised when read), a projection `W` of the routing text's
//! embedding `e` into the same space, and classifier weights `c`. A model's logit is
//! `c · (P_m ⊙ W e)`, and the win rate is `sigmoid(logit_strong − logit_weak)`. Both logits are
//! linear in `e`, so the difference is `v · e` with `v = Wᵀ (c ⊙ (P_strong − P_weak))`, computed
//! once when the file is read: scoring a request is one dot product, with no training.

use std::collections::HashMap;

use serde::Deserialize;

/// A model file as written: JSON with `models` (name → embedding), `projection` (one row per
/// dimension of the model embeddings, each as long as the text embedding) and `classifier` (one
/// weight per dimension).
#[derive(Deserialize)]
struct File {
    models: HashMap<String, Vec<f32>>,
    projection: Vec<Vec<f32>>,
    classifier: Vec<f32>,
}

/// Why a model file cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MfError {
    #[error("the model file is not valid: {0}")]
    Malformed(String),
    #[error("the model file has no model {0:?}")]
    UnknownModel(String),
}

/// A scorer read from a model file for one strong and weak pair.
#[derive(Clone, Debug, PartialEq)]
pub struct MatrixFactorization {
    /// The embeddings Model that embeds the routing text.
    pub embedding_model: String,
    /// `Wᵀ (c ⊙ (P_strong − P_weak))`: the logit difference is its dot product with the text's
    /// embedding.
    direction: Vec<f32>,
}

fn normalised(v: &[f32]) -> Vec<f32> {
    let norm = v
        .iter()
        .map(|x| f64::from(*x) * f64::from(*x))
        .sum::<f64>()
        .sqrt();
    if norm > 0.0 {
        #[allow(clippy::cast_possible_truncation)]
        v.iter().map(|x| (f64::from(*x) / norm) as f32).collect()
    } else {
        v.to_vec()
    }
}

impl MatrixFactorization {
    /// Reads a model file for the pair `strong`, `weak`.
    pub fn read(
        bytes: &[u8],
        strong: &str,
        weak: &str,
        embedding_model: &str,
    ) -> Result<MatrixFactorization, MfError> {
        let f: File = serde_json::from_slice(bytes).map_err(|e| MfError::Malformed(e.to_string()))?;
        let dim = f.classifier.len();
        if dim == 0 || f.projection.len() != dim {
            return Err(MfError::Malformed(
                "the projection needs one row per classifier weight".into(),
            ));
        }
        let text_dim = f.projection.first().map_or(0, Vec::len);
        if text_dim == 0 || f.projection.iter().any(|r| r.len() != text_dim) {
            return Err(MfError::Malformed("the projection rows differ in length".into()));
        }
        if f.models.values().any(|m| m.len() != dim) {
            return Err(MfError::Malformed(
                "a model embedding's length differs from the classifier's".into(),
            ));
        }
        let model = |name: &str| {
            f.models
                .get(name)
                .map(|m| normalised(m))
                .ok_or_else(|| MfError::UnknownModel(name.to_owned()))
        };
        let (s, w) = (model(strong)?, model(weak)?);
        let u: Vec<f64> = f
            .classifier
            .iter()
            .zip(s.iter().zip(&w))
            .map(|(c, (a, b))| f64::from(*c) * (f64::from(*a) - f64::from(*b)))
            .collect();
        let mut direction = vec![0f64; text_dim];
        for (row, uk) in f.projection.iter().zip(&u) {
            for (d, x) in direction.iter_mut().zip(row) {
                *d += uk * f64::from(*x);
            }
        }
        #[allow(clippy::cast_possible_truncation)]
        let direction = direction.into_iter().map(|x| x as f32).collect();
        Ok(MatrixFactorization {
            embedding_model: embedding_model.to_owned(),
            direction,
        })
    }

    /// The win rate for a text embedding; `None` when its length is not the model file's.
    #[must_use]
    pub fn win_rate(&self, embedding: &[f32]) -> Option<f64> {
        if embedding.len() != self.direction.len() {
            return None;
        }
        let z: f64 = self
            .direction
            .iter()
            .zip(embedding)
            .map(|(a, b)| f64::from(*a) * f64::from(*b))
            .sum();
        Some(1.0 / (1.0 + (-z).exp()))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use serde_json::json;

    use super::*;

    fn file() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "models": {"big": [2.0, 0.0], "small": [0.0, 1.0], "other": [1.0, 1.0]},
            "projection": [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            "classifier": [1.0, 1.0]
        }))
        .unwrap()
    }

    #[test]
    fn the_win_rate_is_the_sigmoid_of_the_logit_difference() {
        let mf = MatrixFactorization::read(&file(), "big", "small", "emb").unwrap();
        let w = mf.win_rate(&[3.0, 0.0, 0.0]).unwrap();
        assert!((w - 1.0 / (1.0 + (-3.0f64).exp())).abs() < 1e-6, "{w}");
        assert!(mf.win_rate(&[0.0, 3.0, 0.0]).unwrap() < 0.05);
        assert!(
            (mf.win_rate(&[0.0, 0.0, 9.0]).unwrap() - 0.5).abs() < 1e-9,
            "the projection ignores it"
        );
        assert_eq!(mf.win_rate(&[1.0]), None);
    }

    #[test]
    fn the_pair_must_be_in_the_file_and_the_file_well_formed() {
        assert_eq!(
            MatrixFactorization::read(&file(), "big", "nope", "emb"),
            Err(MfError::UnknownModel("nope".into()))
        );
        let bad = serde_json::to_vec(&json!({"models": {}, "projection": [[1.0]], "classifier": [1.0, 2.0]}))
            .unwrap();
        assert!(matches!(
            MatrixFactorization::read(&bad, "a", "b", "e"),
            Err(MfError::Malformed(_))
        ));
        assert!(matches!(
            MatrixFactorization::read(b"{", "a", "b", "e"),
            Err(MfError::Malformed(_))
        ));
    }

    #[test]
    fn scoring_takes_bounded_time() {
        let dim = 128;
        let text = 3072;
        let row: Vec<f32> = (0..text).map(|i| (i % 7) as f32 / 7.0).collect();
        let f = json!({
            "models": {"s": vec![0.5f32; dim], "w": vec![0.25f32; dim]},
            "projection": vec![row; dim],
            "classifier": vec![1.0f32; dim]
        });
        let mf = MatrixFactorization::read(&serde_json::to_vec(&f).unwrap(), "s", "w", "e").unwrap();
        let e = vec![0.01f32; text];
        let started = Instant::now();
        for _ in 0..1000 {
            assert!(mf.win_rate(&e).is_some());
        }
        assert!(started.elapsed().as_millis() < 5_000, "under 5 ms a score");
    }
}
