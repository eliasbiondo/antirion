//! Rules of `cache.invalidation_rules` the schema cannot express: each trigger's own field, a
//! valid schedule, and the value each action needs when the trigger does not supply it.

use serde_json::Value;

use crate::cron::Cron;
use crate::{ConfigError, DetailCode, DocPath};

/// The kind of value an action drops by that its trigger supplies.
fn supplied_by(event: &str) -> Option<&'static str> {
    match event {
        "model_disabled" => Some("model"),
        "deployment_changed" => Some("deployment"),
        "route_changed" => Some("route"),
        "metadata_changed" => Some("metadata"),
        _ => None,
    }
}

pub(crate) fn check(doc: &Value, errors: &mut Vec<ConfigError>) {
    let Some(rules) = doc
        .pointer("/cache/invalidation_rules")
        .and_then(Value::as_object)
    else {
        return;
    };
    for (name, rule) in rules {
        let at = DocPath::root().key("cache").key("invalidation_rules").key(name);
        let trigger = at.key("trigger");
        let action = at.key("action");
        let event = rule
            .pointer("/trigger/event")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut err = |path: &DocPath, code: DetailCode, message: &str| {
            errors.push(ConfigError::new(path, code, message));
        };
        match (event, rule.pointer("/trigger/schedule").and_then(Value::as_str)) {
            ("schedule", None) => err(
                &trigger.key("schedule"),
                DetailCode::Required,
                "a schedule trigger needs schedule",
            ),
            ("schedule", Some(s)) => {
                if let Err(why) = Cron::parse(s) {
                    err(
                        &trigger.key("schedule"),
                        DetailCode::InvalidFormat,
                        &format!("schedule is not a five-field cron expression: {why}"),
                    );
                }
            }
            (_, Some(_)) => err(
                &trigger.key("schedule"),
                DetailCode::Conflict,
                "schedule is only for the schedule trigger",
            ),
            _ => {}
        }
        let metadata_key = rule.pointer("/trigger/metadata_key").and_then(Value::as_str);
        match (event == "metadata_changed", metadata_key) {
            (true, None) => err(
                &trigger.key("metadata_key"),
                DetailCode::Required,
                "a metadata_changed trigger needs metadata_key",
            ),
            (false, Some(_)) => err(
                &trigger.key("metadata_key"),
                DetailCode::Conflict,
                "metadata_key is only for the metadata_changed trigger",
            ),
            _ => {}
        }
        let drop = rule
            .pointer("/action/drop")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let value = rule.pointer("/action/value").and_then(Value::as_str);
        let older_than = rule.pointer("/action/older_than").is_some();
        match (drop == "older_than", older_than) {
            (true, false) => err(
                &action.key("older_than"),
                DetailCode::Required,
                "drop older_than needs older_than",
            ),
            (false, true) => err(
                &action.key("older_than"),
                DetailCode::Conflict,
                "older_than is only for drop older_than",
            ),
            _ => {}
        }
        if drop == "older_than" {
            if value.is_some() {
                err(
                    &action.key("value"),
                    DetailCode::Conflict,
                    "drop older_than takes no value",
                );
            }
            continue;
        }
        let from_trigger = supplied_by(event) == Some(drop);
        match value {
            None if !from_trigger => err(
                &action.key("value"),
                DetailCode::Required,
                &format!("drop {drop} needs a value unless the trigger names it"),
            ),
            Some(v) if drop == "metadata" && !from_trigger && !v.contains('=') => err(
                &action.key("value"),
                DetailCode::InvalidFormat,
                "drop metadata needs <key>=<value> unless a metadata_changed trigger supplies the value",
            ),
            Some(v) if drop == "metadata" && from_trigger && !v.contains('=') && Some(v) != metadata_key => {
                err(
                    &action.key("value"),
                    DetailCode::Conflict,
                    "the metadata key dropped must be the trigger's metadata_key",
                )
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn paths(rules: Value) -> Vec<String> {
        let mut errors = Vec::new();
        check(&json!({"cache": {"invalidation_rules": rules}}), &mut errors);
        errors.into_iter().map(|e| e.path).collect()
    }

    #[test]
    fn valid_rules_pass() {
        assert!(paths(json!({
            "a": {"trigger": {"event": "model_disabled"}, "action": {"drop": "model"}},
            "b": {"trigger": {"event": "schedule", "schedule": "0 3 * * *"}, "action": {"drop": "older_than", "older_than": "7d"}},
            "c": {"trigger": {"event": "metadata_changed", "metadata_key": "v"}, "action": {"drop": "metadata", "value": "v"}},
            "d": {"trigger": {"event": "manual"}, "action": {"drop": "end_user", "value": "u1"}},
            "e": {"trigger": {"event": "manual"}, "action": {"drop": "metadata", "value": "v=2"}}
        }))
        .is_empty());
    }

    #[test]
    fn missing_and_misplaced_fields_are_named() {
        let got = paths(json!({
            "a": {"trigger": {"event": "schedule"}, "action": {"drop": "older_than"}},
            "b": {"trigger": {"event": "schedule", "schedule": "every day"}, "action": {"drop": "key"}},
            "c": {"trigger": {"event": "manual", "metadata_key": "v"}, "action": {"drop": "metadata", "value": "v"}},
            "d": {"trigger": {"event": "route_changed"}, "action": {"drop": "model"}}
        }));
        for want in [
            "cache.invalidation_rules.a.trigger.schedule",
            "cache.invalidation_rules.a.action.older_than",
            "cache.invalidation_rules.b.trigger.schedule",
            "cache.invalidation_rules.b.action.value",
            "cache.invalidation_rules.c.trigger.metadata_key",
            "cache.invalidation_rules.c.action.value",
            "cache.invalidation_rules.d.action.value",
        ] {
            assert!(got.iter().any(|p| p == want), "{want} not in {got:?}");
        }
    }
}
