//! Operator-defined response predicates (`retry.retry_on_response`): a 2xx answer that matches one
//! is an attempt failure of class `server_error` whose only action is the next deployment, and
//! which never counts toward health. Header predicates are read from the response head; the others
//! need the whole answer, so they apply to complete (non-streaming) responses.

use http::HeaderMap;
use serde_json::Value;

/// One predicate over a 2xx answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResponsePredicate {
    /// The response header `name` (lowercase) has exactly `value`.
    Header { name: String, value: String },
    /// A finish reason of the answer, as the upstream wrote it, is one of these.
    FinishReason(Vec<String>),
    /// The answer carries no assistant content: no text and no tool call.
    EmptyContent,
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

/// Whether a body predicate matches a complete answer.
#[must_use]
pub fn answer_matches(preds: &[ResponsePredicate], facts: &AnswerFacts) -> bool {
    preds.iter().any(|p| match p {
        ResponsePredicate::FinishReason(set) => facts.finish_reasons.iter().any(|f| set.contains(f)),
        ResponsePredicate::EmptyContent => !facts.content,
        ResponsePredicate::Header { .. } => false,
    })
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
}
