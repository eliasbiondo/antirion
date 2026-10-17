//! Rules of the configured metric labels and the feedback API the schema cannot express: a custom
//! label names only families that accept configured labels and never reuses a label they already
//! carry, and feedback needs the request log it attaches to.

use serde_json::Value;

use crate::{ConfigError, DetailCode, DocPath};

/// The families that accept configured labels, with the labels the catalogue gives them.
const LABELLED: [(&str, &[&str]); 4] = [
    (
        "antirion_requests_total",
        &[
            "operation",
            "dialect",
            "model",
            "stream",
            "status_code",
            "code",
            "response_path",
        ],
    ),
    (
        "antirion_request_duration_seconds",
        &["operation", "model", "stream", "status_class", "le"],
    ),
    (
        "antirion_tokens_total",
        &["model", "provider", "deployment", "upstream_model", "token_type"],
    ),
    ("antirion_cost_usd_total", &["model", "provider", "deployment"]),
];

/// The families a custom label goes on when it names none.
const DEFAULT_FAMILIES: [&str; 2] = ["antirion_requests_total", "antirion_tokens_total"];

const TENANT: [&str; 3] = ["organization", "team", "project"];

fn custom_labels(doc: &Value, errors: &mut Vec<ConfigError>) {
    let at = DocPath::root()
        .key("observability")
        .key("metrics")
        .key("custom_labels");
    for (name, def) in doc
        .pointer("/observability/metrics/custom_labels")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let here = at.key(name);
        let listed: Vec<(usize, &str)> = def
            .get("families")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
            .filter_map(|(i, f)| Some((i, f.as_str()?)))
            .collect();
        for (i, family) in &listed {
            if !LABELLED.iter().any(|(f, _)| f == family) {
                errors.push(ConfigError::new(
                    &here.key("families").index(*i),
                    DetailCode::InvalidEnum,
                    "custom labels go only on antirion_requests_total, antirion_request_duration_seconds, \
                     antirion_tokens_total and antirion_cost_usd_total",
                ));
            }
        }
        let families: Vec<&str> = if listed.is_empty() {
            DEFAULT_FAMILIES.to_vec()
        } else {
            listed.iter().map(|(_, f)| *f).collect()
        };
        let clash = TENANT.contains(&name.as_str())
            || LABELLED
                .iter()
                .filter(|(f, _)| families.contains(f))
                .any(|(_, labels)| labels.contains(&name.as_str()));
        if clash {
            errors.push(ConfigError::new(
                &here,
                DetailCode::Conflict,
                "the label name is already a label of a family it would go on",
            ));
        }
    }
}

fn feedback(doc: &Value, errors: &mut Vec<ConfigError>) {
    let enabled = doc.pointer("/observability/feedback/enabled") == Some(&Value::Bool(true));
    let logs_off = doc.pointer("/observability/request_logs/enabled") == Some(&Value::Bool(false));
    if enabled && logs_off {
        errors.push(ConfigError::new(
            &DocPath::root()
                .key("observability")
                .key("feedback")
                .key("enabled"),
            DetailCode::Conflict,
            "feedback attaches to request-log records, and request_logs.enabled is false",
        ));
    }
}

pub(crate) fn check(doc: &Value, errors: &mut Vec<ConfigError>) {
    custom_labels(doc, errors);
    feedback(doc, errors);
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn paths(doc: &Value) -> Vec<String> {
        let mut errors = Vec::new();
        check(doc, &mut errors);
        errors.into_iter().map(|e| e.path).collect()
    }

    #[test]
    fn custom_labels_name_labelled_families_and_new_labels() {
        let ok = json!({"observability": {"metrics": {"custom_labels": {
            "environment": {"from_metadata": "env", "allowed_values": ["prod"]}}}}});
        assert!(paths(&ok).is_empty());
        let bad = json!({"observability": {"metrics": {"custom_labels": {
            "environment": {"from_metadata": "env", "allowed_values": ["prod"],
                            "families": ["antirion_overhead_seconds"]},
            "model": {"from_metadata": "m", "allowed_values": ["x"]},
            "team": {"from_metadata": "t", "allowed_values": ["x"], "families": ["antirion_cost_usd_total"]}}}}});
        let got = paths(&bad);
        assert!(got.contains(&"observability.metrics.custom_labels.environment.families[0]".to_owned()));
        assert!(got.contains(&"observability.metrics.custom_labels.model".to_owned()));
        assert!(got.contains(&"observability.metrics.custom_labels.team".to_owned()));
    }

    #[test]
    fn feedback_needs_request_logs() {
        let bad =
            json!({"observability": {"feedback": {"enabled": true}, "request_logs": {"enabled": false}}});
        assert_eq!(paths(&bad), ["observability.feedback.enabled"]);
        let ok = json!({"observability": {"feedback": {"enabled": true}}});
        assert!(paths(&ok).is_empty());
    }
}
