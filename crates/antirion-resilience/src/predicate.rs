//! Operator-defined response predicates (`retry.retry_on_response`): a 2xx answer that matches one
//! fails the attempt, moves to the next deployment, and never counts toward health. Header
//! predicates read the response head. Body predicates apply to complete (non-streaming) responses.

use http::HeaderMap;
use serde_json::{Number, Value};

use crate::Class;

/// One predicate over a 2xx answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResponsePredicate {
    /// The response header `name` (lowercase) has exactly `value`.
    Header { name: String, value: String },
    /// A finish reason of the answer, as the upstream wrote it, is one of these.
    FinishReason(Vec<String>),
    /// The answer carries no assistant content: no text and no tool call.
    EmptyContent,
    /// The number at the JSON Pointer `path` is strictly below `below`.
    Below { path: String, below: Number },
}

impl ResponsePredicate {
    fn parse(v: &Value) -> Option<ResponsePredicate> {
        if let (Some(name), Some(value)) = (
            v.get("header").and_then(Value::as_str),
            v.get("equals").and_then(Value::as_str),
        ) {
            return Some(ResponsePredicate::Header {
                name: name.to_ascii_lowercase(),
                value: value.to_owned(),
            });
        }
        if let Some(list) = v.get("finish_reason").and_then(Value::as_array) {
            return Some(ResponsePredicate::FinishReason(
                list.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
            ));
        }
        if let (Some(path), Some(below)) = (
            v.get("path").and_then(Value::as_str),
            v.get("below").and_then(Value::as_number),
        ) {
            return Some(ResponsePredicate::Below {
                path: path.to_owned(),
                below: below.clone(),
            });
        }
        (v.get("empty_content").and_then(Value::as_bool) == Some(true))
            .then_some(ResponsePredicate::EmptyContent)
    }

    /// Whether the predicate needs the whole answer.
    #[must_use]
    pub fn needs_body(&self) -> bool {
        !matches!(self, ResponsePredicate::Header { .. })
    }
}

/// The predicates of a merged `policies.resilience` value.
#[must_use]
pub fn predicates(resilience: &Value) -> Vec<ResponsePredicate> {
    resilience
        .pointer("/retry/retry_on_response")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(ResponsePredicate::parse)
        .collect()
}

/// Whether a header predicate matches the response head.
#[must_use]
pub fn head_matches(preds: &[ResponsePredicate], headers: &HeaderMap) -> bool {
    preds.iter().any(|p| match p {
        ResponsePredicate::Header { name, value } => headers
            .get_all(name.as_str())
            .iter()
            .any(|v| v.as_bytes() == value.as_bytes()),
        _ => false,
    })
}

/// What a complete answer shows the body predicates.
#[derive(Clone, Debug, Default)]
pub struct AnswerFacts {
    pub finish_reasons: Vec<String>,
    /// The answer carries assistant text or a tool call.
    pub content: bool,
}

/// Whether a predicate matches the derived facts, without access to the original answer body.
#[must_use]
pub fn answer_matches(preds: &[ResponsePredicate], facts: &AnswerFacts) -> bool {
    answer_failure(preds, facts, None).is_some()
}

/// The failure class of the first matching body predicate. JSON Pointer predicates need the
/// original answer body.
#[must_use]
pub fn answer_failure(
    preds: &[ResponsePredicate],
    facts: &AnswerFacts,
    body: Option<&Value>,
) -> Option<Class> {
    preds.iter().find_map(|p| match p {
        ResponsePredicate::FinishReason(set) if facts.finish_reasons.iter().any(|f| set.contains(f)) => {
            Some(Class::ServerError)
        }
        ResponsePredicate::EmptyContent if !facts.content => Some(Class::ServerError),
        ResponsePredicate::Below { path, below } => body
            .and_then(|value| value.pointer(path))
            .and_then(Value::as_number)
            .filter(|value| number_below(value, below))
            .map(|_| Class::LowConfidence),
        _ => None,
    })
}

fn number_below(value: &Number, below: &Number) -> bool {
    // Preserve integer precision in mixed comparisons by rounding the float instead of the integer.
    match (value.as_i128(), below.as_i128()) {
        (Some(value), Some(below)) => value < below,
        (Some(value), None) => below.as_f64().is_some_and(|below| value < below.ceil() as i128),
        (None, Some(below)) => value
            .as_f64()
            .is_some_and(|value| (value.floor() as i128) < below),
        (None, None) => value
            .as_f64()
            .zip(below.as_f64())
            .is_some_and(|(value, below)| value < below),
    }
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;
    use serde_json::json;

    use super::*;

    #[test]
    fn parsing_and_matching() {
        let preds = predicates(&json!({"retry": {"retry_on_response": [
            {"header": "X-Fail", "equals": "1"},
            {"finish_reason": ["length", "content_filter"]},
            {"empty_content": true},
            {"empty_content": false}
        ]}}));
        assert_eq!(preds.len(), 3);
        assert!(!preds[0].needs_body() && preds[1].needs_body());
        let mut h = HeaderMap::new();
        assert!(!head_matches(&preds, &h));
        h.insert("x-fail", HeaderValue::from_static("0"));
        assert!(!head_matches(&preds, &h));
        h.append("x-fail", HeaderValue::from_static("1"));
        assert!(head_matches(&preds, &h));
        let full = AnswerFacts {
            finish_reasons: vec!["stop".into()],
            content: true,
        };
        assert!(!answer_matches(&preds, &full));
        assert!(answer_matches(
            &preds,
            &AnswerFacts {
                finish_reasons: vec!["length".into()],
                content: true
            }
        ));
        assert!(answer_matches(
            &preds,
            &AnswerFacts {
                finish_reasons: vec!["stop".into()],
                content: false
            }
        ));
        assert!(predicates(&json!({})).is_empty());
    }

    #[test]
    fn only_numeric_values_strictly_below_the_threshold_match() {
        let preds = predicates(&json!({"retry": {"retry_on_response": [
            {"path": "/answers/a~1b/confidence", "below": 0.6}
        ]}}));
        let facts = AnswerFacts::default();
        for (value, expected) in [
            (json!(0.05), Some(Class::LowConfidence)),
            (json!(0.6), None),
            (json!(0.8), None),
            (json!("0.05"), None),
            (Value::Null, None),
        ] {
            let body = json!({"answers": {"a/b": {"confidence": value}}});
            assert_eq!(answer_failure(&preds, &facts, Some(&body)), expected, "{body}");
        }
        let body = json!({"answers": {"a/b": {"type": "noul"}}});
        assert_eq!(answer_failure(&preds, &facts, Some(&body)), None);
    }

    #[test]
    fn numeric_thresholds_preserve_large_integers() {
        let lower = Number::from(9_007_199_254_740_992_u64);
        let higher = Number::from(9_007_199_254_740_993_u64);
        let float = Number::from_f64(9_007_199_254_740_992.0).unwrap();
        assert!(number_below(&lower, &higher));
        assert!(number_below(&float, &higher));
        assert!(!number_below(&higher, &float));
    }
}
