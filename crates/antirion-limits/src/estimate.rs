//! The admission estimate: input tokens `E_in` and worst-case output tokens `E_out`.

use serde_json::Value;

use crate::money::{DEC_ONE, ceil_div};
use crate::settings::{EstimationSettings, InputEstimator, OutputMode};

/// The largest choice count (`n`) a request may ask for.
pub const MAX_CHOICES: u64 = 128;

/// A tokenizer shipped with the gateway.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Tokenizer {
    O200kBase,
    Cl100kBase,
}

impl Tokenizer {
    /// The shipped tokenizer of a deployment's `tokenizer` capability, if any.
    #[must_use]
    pub fn named(name: &str) -> Option<Tokenizer> {
        match name {
            "o200k_base" => Some(Tokenizer::O200kBase),
            "cl100k_base" => Some(Tokenizer::Cl100kBase),
            _ => None,
        }
    }

    #[must_use]
    pub fn count(self, text: &str) -> u64 {
        let n = match self {
            Tokenizer::O200kBase => bpe_openai::o200k_base().count(text),
            Tokenizer::Cl100kBase => bpe_openai::cl100k_base().count(text),
        };
        u64::try_from(n).unwrap_or(u64::MAX)
    }
}

/// Media parts of the request whose tokens are not counted from text.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaCounts {
    /// Images without a known token count (they use `image_tokens_default`).
    pub images: u64,
    pub audio: u64,
    pub video: u64,
    /// Tokens of images counted from their real dimensions.
    pub known_image_tokens: u64,
}

/// What the estimator needs to know about a request.
#[derive(Clone, Copy, Debug, Default)]
pub struct PromptFacts<'a> {
    /// Every text the upstream will receive, including gateway-resolved input (stored
    /// conversation state, forced system prompts, templates).
    pub texts: &'a [&'a str],
    pub messages: u64,
    /// Tool definitions as JSON.
    pub tools_json: Option<&'a str>,
    /// The upstream body as it will be serialised, inline media payloads excluded.
    pub body_bytes: u64,
    pub media: MediaCounts,
    /// The largest explicit output cap of the request, in any of the dialect's fields.
    pub max_output: Option<u64>,
    /// A reasoning budget the dialect bills outside the output cap.
    pub reasoning_outside_cap: u64,
    /// Choices requested (`n`, `candidateCount`); 0 is read as 1.
    pub choices: u64,
    /// Whether the endpoint generates tokens (not embeddings, moderations, rerank).
    pub generates: bool,
}

/// What the estimator needs to know about a candidate deployment.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct CandidateFacts {
    pub tokenizer: Option<Tokenizer>,
    pub max_output_tokens: Option<u64>,
}

/// Where `E_out` came from, recorded on ledger lines.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum OutSource {
    Request,
    Catalog,
    /// `default_max_output_tokens`: not a true upper bound.
    Default,
}

impl OutSource {
    #[must_use]
    pub fn parse(s: &str) -> Option<OutSource> {
        [OutSource::Request, OutSource::Catalog, OutSource::Default]
            .into_iter()
            .find(|v| v.as_str() == s)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            OutSource::Request => "request",
            OutSource::Catalog => "catalog",
            OutSource::Default => "default",
        }
    }
}

/// The admission estimate.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Estimate {
    pub input: u64,
    pub output: u64,
    pub out_source: OutSource,
    /// With `output: clamp`, the cap every upstream request of a generating endpoint carries.
    pub clamp_to: Option<u64>,
}

fn bytes_estimate(bytes: u64, cfg: &EstimationSettings) -> u64 {
    let n = ceil_div(u128::from(bytes).saturating_mul(DEC_ONE), cfg.bytes_per_token.0)
        .unwrap_or(u128::from(bytes));
    u64::try_from(n).unwrap_or(u64::MAX)
}

fn text_bytes(f: &PromptFacts<'_>) -> u64 {
    let texts = f
        .texts
        .iter()
        .map(|t| t.len() as u64)
        .fold(0u64, u64::saturating_add);
    texts.saturating_add(f.tools_json.map_or(0, |t| t.len() as u64))
}

fn tokenized(f: &PromptFacts<'_>, t: Tokenizer) -> u64 {
    let texts = f.texts.iter().map(|s| t.count(s)).fold(0u64, u64::saturating_add);
    texts
        .saturating_add(f.messages.saturating_mul(3))
        .saturating_add(3)
        .saturating_add(f.tools_json.map_or(0, |j| t.count(j)))
}

/// `E_in`: never below 1.
#[must_use]
pub fn input_tokens(f: &PromptFacts<'_>, cands: &[CandidateFacts], cfg: &EstimationSettings) -> u64 {
    let text = match cfg.input {
        InputEstimator::UpperBound => f.body_bytes,
        InputEstimator::Bytes => bytes_estimate(text_bytes(f), cfg),
        InputEstimator::Auto => {
            let tokenizers: Option<Vec<Tokenizer>> = cands.iter().map(|c| c.tokenizer).collect();
            match tokenizers {
                Some(ts) if !ts.is_empty() => {
                    let mut distinct: Vec<Tokenizer> = Vec::with_capacity(2);
                    for t in ts {
                        if !distinct.contains(&t) {
                            distinct.push(t);
                        }
                    }
                    distinct.iter().map(|t| tokenized(f, *t)).max().unwrap_or(0)
                }
                _ => bytes_estimate(text_bytes(f), cfg),
            }
        }
    };
    let media = f
        .media
        .images
        .saturating_mul(cfg.image_tokens_default)
        .saturating_add(f.media.audio.saturating_mul(cfg.audio_tokens_default))
        .saturating_add(f.media.video.saturating_mul(cfg.video_tokens_default))
        .saturating_add(f.media.known_image_tokens);
    text.saturating_add(media).max(1)
}

/// `E_out`: the request's cap, else the candidates' maximum, else the configured default; capped
/// at the candidates' maximum when every one is known; times the choices; then clamped by mode.
#[must_use]
pub fn output_tokens(
    f: &PromptFacts<'_>,
    cands: &[CandidateFacts],
    cfg: &EstimationSettings,
) -> (u64, OutSource, Option<u64>) {
    if !f.generates {
        return (0, OutSource::Request, None);
    }
    let known: Vec<u64> = cands.iter().filter_map(|c| c.max_output_tokens).collect();
    let all_known = !cands.is_empty() && known.len() == cands.len();
    let (mut m, source) = match f.max_output {
        Some(cap) => (cap.saturating_add(f.reasoning_outside_cap), OutSource::Request),
        None => match known.iter().max() {
            Some(max) => (*max, OutSource::Catalog),
            None => (cfg.default_max_output_tokens, OutSource::Default),
        },
    };
    if all_known && let Some(max) = known.iter().max() {
        m = m.min(*max);
    }
    let strict = m.saturating_mul(f.choices.max(1));
    match (cfg.output, cfg.output_cap) {
        (OutputMode::Clamp, Some(cap)) => (strict.min(cap), source, Some(cap)),
        (OutputMode::Optimistic, Some(cap)) => (strict.min(cap), source, None),
        _ => (strict, source, None),
    }
}

/// The admission estimate of a request over its candidate deployments.
#[must_use]
pub fn estimate(f: &PromptFacts<'_>, cands: &[CandidateFacts], cfg: &EstimationSettings) -> Estimate {
    let (output, out_source, clamp_to) = output_tokens(f, cands, cfg);
    Estimate {
        input: input_tokens(f, cands, cfg),
        output,
        out_source,
        clamp_to,
    }
}

/// An explicit output cap or choice count outside its range, by field name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{field} is out of range")]
pub struct CapOutOfRange {
    pub field: String,
}

/// Checks the explicit output caps (`(field, value)`) and the choice count of a request: a cap
/// must be an integer in `[0, ceiling]`, `n` at most 128. Returns the largest cap.
pub fn validate_caps(
    caps: &[(&str, &Value)],
    choices: Option<(&str, &Value)>,
    ceiling: u64,
) -> Result<Option<u64>, CapOutOfRange> {
    let mut largest: Option<u64> = None;
    for (field, v) in caps {
        if v.is_null() {
            continue;
        }
        let cap = v
            .as_u64()
            .filter(|c| *c <= ceiling)
            .ok_or_else(|| CapOutOfRange {
                field: (*field).to_owned(),
            })?;
        largest = Some(largest.map_or(cap, |l| l.max(cap)));
    }
    if let Some((field, v)) = choices
        && !v.is_null()
        && v.as_u64().is_none_or(|n| n > MAX_CHOICES)
    {
        return Err(CapOutOfRange {
            field: field.to_owned(),
        });
    }
    Ok(largest)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn cfg() -> EstimationSettings {
        EstimationSettings::default()
    }

    #[test]
    fn input_never_below_one_and_media_defaults() {
        let f = PromptFacts {
            media: MediaCounts {
                images: 1,
                ..MediaCounts::default()
            },
            ..PromptFacts::default()
        };
        assert_eq!(input_tokens(&f, &[], &cfg()), 1600);
        assert_eq!(input_tokens(&PromptFacts::default(), &[], &cfg()), 1);
    }

    #[test]
    fn estimators() {
        let text = "hello world, this is a prompt";
        let texts = [text];
        let f = PromptFacts {
            texts: &texts,
            messages: 1,
            body_bytes: 200,
            ..PromptFacts::default()
        };
        let tok = CandidateFacts {
            tokenizer: Some(Tokenizer::O200kBase),
            max_output_tokens: None,
        };
        let auto = input_tokens(&f, &[tok], &cfg());
        assert_eq!(auto, Tokenizer::O200kBase.count(text) + 3 + 3);
        let mixed = input_tokens(&f, &[tok, CandidateFacts::default()], &cfg());
        assert_eq!(mixed, (text.len() as u64).div_ceil(3));
        let mut ub = cfg();
        ub.input = InputEstimator::UpperBound;
        assert_eq!(input_tokens(&f, &[tok], &ub), 200);
        assert!(Tokenizer::O200kBase.count(text) <= text.len() as u64);
        assert!(Tokenizer::Cl100kBase.count(text) > 0);
    }

    #[test]
    fn output_sources() {
        let c = |m| CandidateFacts {
            tokenizer: None,
            max_output_tokens: m,
        };
        let f = PromptFacts {
            generates: true,
            max_output: Some(700),
            ..PromptFacts::default()
        };
        assert_eq!(
            output_tokens(&f, &[c(None)], &cfg()),
            (700, OutSource::Request, None)
        );
        assert_eq!(output_tokens(&f, &[c(Some(400))], &cfg()).0, 400);
        let none = PromptFacts {
            generates: true,
            ..PromptFacts::default()
        };
        assert_eq!(
            output_tokens(&none, &[c(Some(400))], &cfg()),
            (400, OutSource::Catalog, None)
        );
        assert_eq!(
            output_tokens(&none, &[c(None)], &cfg()),
            (4096, OutSource::Default, None)
        );
        let two = PromptFacts {
            generates: true,
            max_output: Some(300),
            choices: 2,
            ..PromptFacts::default()
        };
        assert_eq!(output_tokens(&two, &[], &cfg()).0, 600);
        let embed = PromptFacts {
            generates: false,
            max_output: Some(300),
            ..PromptFacts::default()
        };
        assert_eq!(output_tokens(&embed, &[], &cfg()).0, 0);
    }

    #[test]
    fn clamp_and_optimistic() {
        let f = PromptFacts {
            generates: true,
            max_output: Some(8000),
            ..PromptFacts::default()
        };
        let mut clamp = cfg();
        clamp.output = OutputMode::Clamp;
        clamp.output_cap = Some(1000);
        assert_eq!(
            output_tokens(&f, &[], &clamp),
            (1000, OutSource::Request, Some(1000))
        );
        let small = PromptFacts {
            max_output: Some(10),
            ..f
        };
        assert_eq!(
            output_tokens(&small, &[], &clamp),
            (10, OutSource::Request, Some(1000)),
            "the cap is enforced upstream even when the request asks for less"
        );
        let mut opt = clamp.clone();
        opt.output = OutputMode::Optimistic;
        assert_eq!(output_tokens(&f, &[], &opt), (1000, OutSource::Request, None));
    }

    #[test]
    fn caps_are_bounded() {
        let ceiling = 1_073_741_823;
        assert_eq!(
            validate_caps(&[("max_tokens", &json!(300))], None, ceiling),
            Ok(Some(300))
        );
        let huge = json!(18_446_744_073_686_646_784u64);
        assert_eq!(
            validate_caps(&[("max_tokens", &huge)], None, ceiling)
                .unwrap_err()
                .field,
            "max_tokens"
        );
        assert!(validate_caps(&[("max_tokens", &json!(-1))], None, ceiling).is_err());
        assert!(validate_caps(&[], Some(("n", &json!(129))), ceiling).is_err());
        assert_eq!(
            validate_caps(
                &[("max_tokens", &json!(5)), ("max_completion_tokens", &json!(9))],
                Some(("n", &json!(2))),
                ceiling
            ),
            Ok(Some(9))
        );
    }
}
