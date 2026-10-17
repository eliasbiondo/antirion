//! The feedback API's settings (`observability.feedback`) and the check of one submitted value
//! against its metric.

use std::collections::BTreeMap;
use std::time::Duration;

use antirion_config::scalar::parse_duration;
use serde_json::Value;

const DEFAULT_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 3600);
const DEFAULT_MAX_BYTES: usize = 4096;

/// What a metric accepts.
#[derive(Clone, Debug, PartialEq)]
pub enum MetricKind {
    Boolean,
    Number { min: Option<f64>, max: Option<f64> },
    Text { max_bytes: usize },
}

/// `observability.feedback`.
#[derive(Clone, Debug, PartialEq)]
pub struct FeedbackSettings {
    pub enabled: bool,
    pub max_age: Duration,
    pub metrics: BTreeMap<String, MetricKind>,
}

impl Default for FeedbackSettings {
    fn default() -> FeedbackSettings {
        FeedbackSettings::compile(None)
    }
}

/// Why a value was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    UnknownMetric,
    InvalidValue,
}

impl FeedbackSettings {
    /// Reads `observability.feedback` of a validated document.
    #[must_use]
    pub fn compile(block: Option<&Value>) -> FeedbackSettings {
        let get = |k: &str| block.and_then(|b| b.get(k));
        let metrics = get("metrics")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(name, m)| {
                let kind = match m.get("type").and_then(Value::as_str)? {
                    "boolean" => MetricKind::Boolean,
                    "number" => MetricKind::Number {
                        min: m.get("min").and_then(Value::as_f64),
                        max: m.get("max").and_then(Value::as_f64),
                    },
                    _ => MetricKind::Text {
                        max_bytes: m
                            .get("max_bytes")
                            .and_then(Value::as_u64)
                            .and_then(|n| usize::try_from(n).ok())
                            .unwrap_or(DEFAULT_MAX_BYTES),
                    },
                };
                Some((name.clone(), kind))
            })
            .collect();
        FeedbackSettings {
            enabled: get("enabled").and_then(Value::as_bool).unwrap_or(false),
            max_age: get("max_age")
                .and_then(Value::as_str)
                .and_then(parse_duration)
                .unwrap_or(DEFAULT_MAX_AGE),
            metrics,
        }
    }

    /// Checks `value` against `metric`.
    pub fn check(&self, metric: &str, value: &Value) -> Result<(), Refusal> {
        let kind = self.metrics.get(metric).ok_or(Refusal::UnknownMetric)?;
        let ok = match (kind, value) {
            (MetricKind::Boolean, Value::Bool(_)) => true,
            (MetricKind::Number { min, max }, Value::Number(n)) => n
                .as_f64()
                .is_some_and(|v| v.is_finite() && min.is_none_or(|m| v >= m) && max.is_none_or(|m| v <= m)),
            (MetricKind::Text { max_bytes }, Value::String(s)) => s.len() <= *max_bytes,
            _ => false,
        };
        if ok { Ok(()) } else { Err(Refusal::InvalidValue) }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn values_follow_their_metric() {
        let s = FeedbackSettings::compile(Some(&json!({"enabled": true, "max_age": "1h", "metrics": {
            "thumbs": {"type": "boolean"},
            "score": {"type": "number", "min": 0, "max": 5},
            "note": {"type": "text", "max_bytes": 4}}})));
        assert!(s.enabled);
        assert_eq!(s.max_age, Duration::from_secs(3600));
        assert_eq!(s.check("thumbs", &json!(true)), Ok(()));
        assert_eq!(s.check("thumbs", &json!(1)), Err(Refusal::InvalidValue));
        assert_eq!(s.check("score", &json!(4.5)), Ok(()));
        assert_eq!(s.check("score", &json!(6)), Err(Refusal::InvalidValue));
        assert_eq!(s.check("score", &json!(true)), Err(Refusal::InvalidValue));
        assert_eq!(s.check("note", &json!("abcd")), Ok(()));
        assert_eq!(s.check("note", &json!("abcde")), Err(Refusal::InvalidValue));
        assert_eq!(s.check("nope", &json!(true)), Err(Refusal::UnknownMetric));
        assert!(!FeedbackSettings::default().enabled);
    }
}
