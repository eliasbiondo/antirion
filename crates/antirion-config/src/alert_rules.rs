//! Rules of alerting the schema cannot express: rules and channels name entries that exist, an email
//! channel needs outgoing mail, event subscriptions are exact types, `*` or `<prefix>.*`, a latency
//! objective names its threshold, and OTLP metric export names its endpoint.

use serde_json::{Map, Value};

use crate::{ConfigError, DetailCode, DocPath};

/// Whether `pattern` is an event subscription: an exact type, `*`, or `<prefix>.*`.
#[must_use]
pub fn is_event_pattern(pattern: &str) -> bool {
    let name = |s: &str| {
        !s.is_empty()
            && s.split('.').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            })
    };
    pattern == "*" || pattern.strip_suffix(".*").map_or_else(|| name(pattern), name)
}

fn entries<'a>(doc: &'a Value, pointer: &str) -> impl Iterator<Item = (&'a String, &'a Value)> {
    doc.pointer(pointer)
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(Map::iter)
}

fn names(v: Option<&Value>) -> impl Iterator<Item = (usize, &str)> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(i, n)| Some((i, n.as_str()?)))
}

fn patterns(at: &DocPath, events: Option<&Value>, errors: &mut Vec<ConfigError>) {
    for (i, p) in names(events) {
        if !is_event_pattern(p) {
            errors.push(ConfigError::new(
                &at.index(i),
                DetailCode::InvalidFormat,
                "an event subscription is an exact type, \"*\" or \"<prefix>.*\"",
            ));
        }
    }
}

pub(crate) fn check(doc: &Value, errors: &mut Vec<ConfigError>) {
    let alerts = DocPath::root().key("observability").key("alerts");
    let channels = doc.pointer("/observability/alerts/channels");
    let webhooks = doc.pointer("/observability/alerts/webhooks");
    let has = |set: Option<&Value>, name: &str| set.and_then(|s| s.get(name)).is_some();
    for (name, w) in entries(doc, "/observability/alerts/webhooks") {
        patterns(
            &alerts.key("webhooks").key(name).key("events"),
            w.get("events"),
            errors,
        );
    }
    for (name, r) in entries(doc, "/observability/alerts/rules") {
        let at = alerts.key("rules").key(name);
        for member in ["channels", "escalation"] {
            for (i, c) in names(r.get(member)) {
                if !has(channels, c) {
                    errors.push(ConfigError::new(
                        &at.key(member).index(i),
                        DetailCode::UnresolvedReference,
                        format!("alert channel {c:?} does not exist"),
                    ));
                }
            }
        }
        patterns(&at.key("events"), r.get("events"), errors);
    }
    let mail = doc.pointer("/smtp/enabled") == Some(&Value::Bool(true));
    for (name, c) in entries(doc, "/observability/alerts/channels") {
        let at = alerts.key("channels").key(name);
        if let Some(w) = c.pointer("/webhook/webhook").and_then(Value::as_str)
            && !has(webhooks, w)
        {
            errors.push(ConfigError::new(
                &at.key("webhook").key("webhook"),
                DetailCode::UnresolvedReference,
                format!("alert webhook {w:?} does not exist"),
            ));
        }
        if c.get("type").and_then(Value::as_str) == Some("email") && !mail {
            errors.push(ConfigError::new(
                &at.key("type"),
                DetailCode::Conflict,
                "an email channel needs outgoing mail (smtp.enabled)",
            ));
        }
    }
    for (name, s) in entries(doc, "/observability/slos") {
        if s.get("kind").and_then(Value::as_str) == Some("latency") && s.get("latency_threshold_ms").is_none()
        {
            errors.push(ConfigError::new(
                &DocPath::root()
                    .key("observability")
                    .key("slos")
                    .key(name)
                    .key("latency_threshold_ms"),
                DetailCode::Required,
                "a latency objective needs latency_threshold_ms",
            ));
        }
    }
    let otlp = doc.pointer("/observability/metrics/otlp");
    if otlp.and_then(|o| o.get("enabled")) == Some(&Value::Bool(true))
        && otlp.and_then(|o| o.get("endpoint")).is_none_or(Value::is_null)
    {
        errors.push(ConfigError::new(
            &DocPath::root()
                .key("observability")
                .key("metrics")
                .key("otlp")
                .key("endpoint"),
            DetailCode::Required,
            "OTLP metric export needs an endpoint",
        ));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn codes(doc: &Value) -> Vec<(String, &'static str)> {
        let mut errors = Vec::new();
        check(doc, &mut errors);
        errors.iter().map(|e| (e.path.clone(), e.code.as_str())).collect()
    }

    #[test]
    fn subscriptions_are_exact_types_all_or_prefixes() {
        for ok in [
            "*",
            "alert.*",
            "alert.fired",
            "budget.threshold_crossed",
            "log_sink.*",
        ] {
            assert!(is_event_pattern(ok), "{ok}");
        }
        for bad in ["", ".*", "alert.", "Alert.fired", "alert.*.x", "a..b", "al ert"] {
            assert!(!is_event_pattern(bad), "{bad}");
        }
    }

    #[test]
    fn references_mail_and_objectives_are_checked() {
        let doc = json!({"observability": {
            "alerts": {
                "webhooks": {"ops": {"url": "https://h", "events": ["alert.*", "Bad"]}},
                "channels": {
                    "mail": {"type": "email", "email": {"to": ["a@b.c"]}},
                    "hook": {"type": "webhook", "webhook": {"webhook": "missing"}}
                },
                "rules": {"r": {"category": "latency", "channels": ["mail", "nope"], "escalation": ["gone"]}}
            },
            "slos": {"s": {"objective": 0.9, "kind": "latency"}},
            "metrics": {"otlp": {"enabled": true}}
        }});
        let got = codes(&doc);
        for want in [
            ("observability.alerts.webhooks.ops.events[1]", "invalid_format"),
            ("observability.alerts.rules.r.channels[1]", "unresolved_reference"),
            (
                "observability.alerts.rules.r.escalation[0]",
                "unresolved_reference",
            ),
            (
                "observability.alerts.channels.hook.webhook.webhook",
                "unresolved_reference",
            ),
            ("observability.alerts.channels.mail.type", "conflict"),
            ("observability.slos.s.latency_threshold_ms", "required"),
            ("observability.metrics.otlp.endpoint", "required"),
        ] {
            assert!(
                got.iter().any(|(p, c)| p == want.0 && *c == want.1),
                "{want:?} not in {got:?}"
            );
        }
        assert_eq!(got.len(), 7, "{got:?}");
        let mut fine = doc.clone();
        fine["smtp"] = json!({"enabled": true});
        fine["observability"]["alerts"]["channels"]["hook"]["webhook"]["webhook"] = json!("ops");
        fine["observability"]["alerts"]["webhooks"]["ops"]["events"] = json!(["*"]);
        fine["observability"]["alerts"]["rules"]["r"] = json!({"category": "latency", "channels": ["mail"]});
        fine["observability"]["slos"]["s"]["latency_threshold_ms"] = json!(1500);
        fine["observability"]["metrics"]["otlp"]["endpoint"] = json!("http://c");
        assert!(codes(&fine).is_empty(), "{:?}", codes(&fine));
    }
}
