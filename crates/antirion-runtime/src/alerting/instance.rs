//! Alert instances: what a firing records, its timeline, and the transitions of its status
//! (`firing → acknowledged → resolved`, with snoozes, notes and escalations on the way).

use std::collections::BTreeMap;

use antirion_core::UtcMicros;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// What a firing affected in its window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Impact {
    pub requests: u64,
    pub errors: u64,
    pub throttled: u64,
}

/// One timeline entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub time: String,
    pub kind: String,
    pub actor: Option<String>,
    pub detail: Value,
}

/// An alert instance as the Admin API shows it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Instance {
    pub id: String,
    pub rule: String,
    pub fingerprint: String,
    pub category: String,
    pub severity: String,
    pub status: String,
    pub snoozed_until: Option<String>,
    pub fired_at: String,
    pub acknowledged_at: Option<String>,
    pub acknowledged_by: Option<String>,
    pub resolved_at: Option<String>,
    pub resolved_by: Option<String>,
    pub summary: String,
    pub scope: BTreeMap<String, String>,
    pub value: Option<f64>,
    pub threshold: Option<f64>,
    pub owner_team: Option<String>,
    pub runbook_url: Option<String>,
    pub impact: Impact,
    pub timeline: Vec<Entry>,
}

/// Evaluation state kept with an instance, never shown.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// The last fire or refire.
    pub last_fired_us: i64,
    /// Since when the condition has been clear, while it is.
    pub clear_since_us: Option<i64>,
    pub snoozed_until_us: Option<i64>,
}

/// An instance with its evaluation state: the stored body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stored {
    pub instance: Instance,
    pub state: State,
}

/// A rule's identity for one set of scope values: the hex SHA-256 of the rule name and the values.
#[must_use]
pub fn fingerprint(rule: &str, scope: &BTreeMap<String, String>) -> String {
    let mut h = Sha256::new();
    h.update(rule.as_bytes());
    for (k, v) in scope {
        h.update(b"\0");
        h.update(k.as_bytes());
        h.update(b"=");
        h.update(v.as_bytes());
    }
    h.finalize().iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// Why an action does not apply to an instance in its current status.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    Resolved,
}

impl Stored {
    fn entry(&mut self, now: UtcMicros, kind: &str, actor: Option<&str>, detail: Value) {
        self.instance.timeline.push(Entry {
            time: now.to_rfc3339(),
            kind: kind.to_owned(),
            actor: actor.map(str::to_owned),
            detail,
        });
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        self.instance.status != "resolved"
    }

    /// Whether notifications are held at `now`.
    #[must_use]
    pub fn snoozed(&self, now: UtcMicros) -> bool {
        self.state.snoozed_until_us.is_some_and(|u| now.0 < u)
    }

    /// A refire of the open instance: the latest value, and a timeline entry.
    pub fn refire(&mut self, now: UtcMicros, value: Option<f64>, summary: &str) {
        self.instance.value = value;
        summary.clone_into(&mut self.instance.summary);
        self.state.last_fired_us = now.0;
        self.state.clear_since_us = None;
        self.entry(now, "refired", None, json!({"value": value}));
    }

    pub fn acknowledge(&mut self, now: UtcMicros, actor: &str) -> Result<bool, Refusal> {
        match self.instance.status.as_str() {
            "resolved" => Err(Refusal::Resolved),
            "acknowledged" => Ok(false),
            _ => {
                "acknowledged".clone_into(&mut self.instance.status);
                self.instance.acknowledged_at = Some(now.to_rfc3339());
                self.instance.acknowledged_by = Some(actor.to_owned());
                self.entry(now, "acknowledged", Some(actor), json!({}));
                Ok(true)
            }
        }
    }

    /// Resolves the instance; `actor` is `None` when the condition cleared on its own.
    pub fn resolve(&mut self, now: UtcMicros, actor: Option<&str>) -> Result<(), Refusal> {
        if !self.is_open() {
            return Err(Refusal::Resolved);
        }
        "resolved".clone_into(&mut self.instance.status);
        self.instance.resolved_at = Some(now.to_rfc3339());
        self.instance.resolved_by = Some(actor.unwrap_or("system").to_owned());
        let detail = if actor.is_none() {
            json!({"reason": "condition_cleared"})
        } else {
            json!({})
        };
        self.entry(now, "resolved", actor, detail);
        Ok(())
    }

    pub fn snooze(&mut self, now: UtcMicros, until: UtcMicros, actor: &str) -> Result<(), Refusal> {
        if !self.is_open() {
            return Err(Refusal::Resolved);
        }
        self.state.snoozed_until_us = Some(until.0);
        self.instance.snoozed_until = Some(until.to_rfc3339());
        self.entry(now, "snoozed", Some(actor), json!({"until": until.to_rfc3339()}));
        Ok(())
    }

    pub fn unsnooze(&mut self, now: UtcMicros, actor: &str) -> Result<(), Refusal> {
        if !self.is_open() {
            return Err(Refusal::Resolved);
        }
        self.state.snoozed_until_us = None;
        self.instance.snoozed_until = None;
        self.entry(now, "snoozed", Some(actor), json!({"until": null}));
        Ok(())
    }

    pub fn note(&mut self, now: UtcMicros, actor: &str, text: &str) {
        self.entry(now, "note", Some(actor), json!({"text": text}));
    }

    pub fn escalated(&mut self, now: UtcMicros, actor: &str, channels: &[String]) -> Result<(), Refusal> {
        if !self.is_open() {
            return Err(Refusal::Resolved);
        }
        self.entry(now, "escalated", Some(actor), json!({"channels": channels}));
        Ok(())
    }

    /// Records one notification attempt's outcome on the timeline.
    pub fn notified(&mut self, now: UtcMicros, channel: &str, ok: bool, detail: Value) {
        let mut d = json!({"channel": channel});
        if let (Value::Object(into), Value::Object(from)) = (&mut d, detail) {
            into.extend(from);
        }
        self.entry(now, if ok { "notified" } else { "notification_failed" }, None, d);
    }
}

/// What a new instance starts from.
#[derive(Debug)]
pub struct Firing<'a> {
    pub id: String,
    pub rule: &'a super::config::Rule,
    pub scope: BTreeMap<String, String>,
    pub value: Option<f64>,
    pub threshold: Option<f64>,
    pub summary: String,
    pub impact: Impact,
}

impl Firing<'_> {
    #[must_use]
    pub fn into_stored(self, now: UtcMicros) -> Stored {
        let fp = fingerprint(&self.rule.name, &self.scope);
        let mut s = Stored {
            instance: Instance {
                id: self.id,
                rule: self.rule.name.clone(),
                fingerprint: fp,
                category: self.rule.category.clone(),
                severity: self.rule.severity.as_str().to_owned(),
                status: "firing".to_owned(),
                snoozed_until: None,
                fired_at: now.to_rfc3339(),
                acknowledged_at: None,
                acknowledged_by: None,
                resolved_at: None,
                resolved_by: None,
                summary: self.summary,
                scope: self.scope,
                value: self.value,
                threshold: self.threshold,
                owner_team: self.rule.owner_team.clone(),
                runbook_url: self.rule.runbook_url.clone(),
                impact: self.impact,
                timeline: Vec::new(),
            },
            state: State {
                last_fired_us: now.0,
                clear_since_us: None,
                snoozed_until_us: None,
            },
        };
        let value = s.instance.value;
        s.entry(
            now,
            "fired",
            None,
            json!({"value": value, "threshold": self.threshold}),
        );
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alerting::config::AlertingConfig;

    fn stored() -> Stored {
        let cfg = AlertingConfig::compile(&json!({"observability": {"alerts": {"rules": {
            "r": {"category": "latency", "metric": "requests", "threshold": 1, "owner_team": "t"}}}}}));
        let rule = cfg.rule("r").cloned().unwrap();
        Firing {
            id: "a1".into(),
            rule: &rule,
            scope: BTreeMap::from([("provider".to_owned(), "p".to_owned())]),
            value: Some(2.0),
            threshold: Some(1.0),
            summary: "s".into(),
            impact: Impact::default(),
        }
        .into_stored(UtcMicros(1_000_000))
    }

    #[test]
    fn fingerprints_depend_on_rule_and_scope_values() {
        let s = BTreeMap::from([("provider".to_owned(), "p".to_owned())]);
        assert_eq!(fingerprint("r", &s), fingerprint("r", &s.clone()));
        assert_ne!(fingerprint("r", &s), fingerprint("q", &s));
        assert_ne!(fingerprint("r", &s), fingerprint("r", &BTreeMap::new()));
        assert_eq!(fingerprint("r", &s).len(), 32);
    }

    #[test]
    fn lifecycle() {
        let mut s = stored();
        assert_eq!(s.instance.status, "firing");
        assert_eq!(s.instance.owner_team.as_deref(), Some("t"));
        assert_eq!(s.instance.timeline[0].kind, "fired");
        assert_eq!(s.acknowledge(UtcMicros(2), "alice"), Ok(true));
        assert_eq!(s.acknowledge(UtcMicros(3), "alice"), Ok(false));
        assert_eq!(s.instance.acknowledged_by.as_deref(), Some("alice"));
        s.snooze(UtcMicros(4), UtcMicros(100), "alice").unwrap();
        assert!(s.snoozed(UtcMicros(50)) && !s.snoozed(UtcMicros(100)));
        s.note(UtcMicros(5), "bob", "looking");
        s.notified(UtcMicros(6), "pd", false, json!({"status_code": 500}));
        s.resolve(UtcMicros(7), None).unwrap();
        assert_eq!(s.instance.resolved_by.as_deref(), Some("system"));
        assert_eq!(s.resolve(UtcMicros(8), Some("x")), Err(Refusal::Resolved));
        assert_eq!(s.acknowledge(UtcMicros(8), "x"), Err(Refusal::Resolved));
        let kinds: Vec<&str> = s.instance.timeline.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(
            kinds,
            [
                "fired",
                "acknowledged",
                "snoozed",
                "note",
                "notification_failed",
                "resolved"
            ]
        );
        assert_eq!(s.instance.timeline[4].detail["channel"], "pd");
    }
}
