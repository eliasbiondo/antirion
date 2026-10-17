//! What each channel receives for an instance: a Slack incoming-webhook message, a PagerDuty Events
//! API v2 event, an email, or the `alert.*` event data of the event webhooks.

use antirion_limits::level::Level;
use serde_json::{Value, json};

use super::config::{AlertingConfig, Trigger};
use super::instance::Instance;
use super::summaries;
use crate::wording;

/// How an instance reads in a channel's human text: its rule's metric gives the unit of its value
/// and threshold, and the configuration gives the display names of its scope.
#[derive(Clone, Copy, Debug)]
pub struct Wording<'a> {
    pub doc: &'a Value,
    pub metric: Option<&'a str>,
}

impl<'a> Wording<'a> {
    /// The wording of `i` under the serving configuration.
    #[must_use]
    pub fn of(doc: &'a Value, alerting: &'a AlertingConfig, i: &Instance) -> Wording<'a> {
        let metric = alerting.rule(&i.rule).and_then(|r| match &r.trigger {
            Trigger::Metric { metric, .. } => Some(metric.as_str()),
            Trigger::Event { .. } => None,
        });
        Wording { doc, metric }
    }

    fn value(&self, v: Option<f64>) -> String {
        match (v, self.metric) {
            (None, _) => "—".to_owned(),
            (Some(v), Some(m)) => summaries::shown(m, v),
            (Some(v), None) => wording::number(v),
        }
    }

    /// The scope by display names: `team Search`, a budget as its label, `gateway` when empty.
    fn scope(&self, i: &Instance) -> String {
        if i.scope.is_empty() {
            return "gateway".to_owned();
        }
        i.scope
            .iter()
            .map(|(k, v)| {
                let budget = (k == "budget")
                    .then(|| {
                        let mut parts = v.splitn(3, '/');
                        let level = Level::parse(parts.next()?)?;
                        Some(wording::budget_label(
                            self.doc,
                            level,
                            parts.next()?,
                            parts.next()?,
                        ))
                    })
                    .flatten();
                budget.unwrap_or_else(|| format!("{k} {}", wording::display_name(self.doc, k, v)))
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Why an instance is being sent.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Fired,
    Refired,
    Escalated,
    Acknowledged,
    Resolved,
    Test,
}

impl Phase {
    /// The `alert.*` event type of the phase.
    #[must_use]
    pub const fn event_type(self) -> &'static str {
        match self {
            Phase::Fired | Phase::Refired | Phase::Escalated => "alert.fired",
            Phase::Acknowledged => "alert.acknowledged",
            Phase::Resolved => "alert.resolved",
            Phase::Test => "test.ping",
        }
    }

    const fn word(self) -> &'static str {
        match self {
            Phase::Fired => "Firing",
            Phase::Refired => "Still firing",
            Phase::Escalated => "Escalated",
            Phase::Acknowledged => "Acknowledged",
            Phase::Resolved => "Resolved",
            Phase::Test => "Test",
        }
    }
}

/// The event data of `alert.fired`, `alert.acknowledged` and `alert.resolved`: the instance without
/// its timeline.
#[must_use]
pub fn event_data(i: &Instance, phase: Phase) -> Value {
    json!({
        "id": i.id,
        "rule": i.rule,
        "fingerprint": i.fingerprint,
        "category": i.category,
        "severity": i.severity,
        "status": i.status,
        "summary": i.summary,
        "scope": i.scope,
        "value": i.value,
        "threshold": i.threshold,
        "fired_at": i.fired_at,
        "acknowledged_by": i.acknowledged_by,
        "resolved_at": i.resolved_at,
        "runbook_url": i.runbook_url,
        "escalated": phase == Phase::Escalated,
    })
}

/// A Slack incoming-webhook message: `text` for notifications, Block Kit blocks for the client.
#[must_use]
pub fn slack(i: &Instance, phase: Phase, link: Option<&str>, w: Wording<'_>) -> Value {
    let title = format!("[{}] {}: {}", i.severity.to_uppercase(), phase.word(), i.summary);
    let mut blocks = vec![
        json!({"type": "header", "text": {"type": "plain_text", "text": title.chars().take(150).collect::<String>()}}),
        json!({"type": "section", "fields": [
            {"type": "mrkdwn", "text": format!("*Severity*\n{}", i.severity)},
            {"type": "mrkdwn", "text": format!("*Rule*\n{}", i.rule)},
            {"type": "mrkdwn", "text": format!("*Scope*\n{}", w.scope(i))},
            {"type": "mrkdwn", "text": format!("*Value*\n{} (threshold {})", w.value(i.value), w.value(i.threshold))},
        ]}),
        json!({"type": "section", "text": {"type": "mrkdwn", "text": i.summary}}),
    ];
    let mut links = Vec::new();
    if let Some(l) = link {
        links.push(format!("<{l}|Open in the console>"));
    }
    if let Some(r) = &i.runbook_url {
        links.push(format!("<{r}|Runbook>"));
    }
    if !links.is_empty() {
        blocks.push(json!({"type": "context", "elements": [{"type": "mrkdwn", "text": links.join(" · ")}]}));
    }
    json!({"text": title, "blocks": blocks})
}

/// A PagerDuty Events API v2 event; `None` for a phase PagerDuty has no action for.
#[must_use]
pub fn pagerduty(i: &Instance, phase: Phase, routing_key: &str, link: Option<&str>) -> Option<Value> {
    let action = match phase {
        Phase::Fired | Phase::Refired | Phase::Escalated | Phase::Test => "trigger",
        Phase::Acknowledged => "acknowledge",
        Phase::Resolved => "resolve",
    };
    let mut event = json!({
        "routing_key": routing_key,
        "event_action": action,
        "dedup_key": i.fingerprint,
    });
    if action == "trigger"
        && let Some(obj) = event.as_object_mut()
    {
        obj.insert(
            "payload".into(),
            json!({
                "summary": i.summary.chars().take(1024).collect::<String>(),
                "source": "antirion",
                "severity": i.severity,
                "component": i.scope.get("provider").or_else(|| i.scope.get("deployment")),
                "group": i.rule,
                "class": i.category,
                "custom_details": {"value": i.value, "threshold": i.threshold, "scope": i.scope,
                                   "instance": i.id, "escalated": phase == Phase::Escalated},
            }),
        );
        if let Some(l) = link {
            obj.insert("links".into(), json!([{"href": l, "text": "Antirion console"}]));
        }
    }
    Some(event)
}

/// An email's subject and plain-text body.
#[must_use]
pub fn email(i: &Instance, phase: Phase, link: Option<&str>, w: Wording<'_>) -> (String, String) {
    let subject = format!("[Antirion {}] {}: {}", i.severity, phase.word(), i.summary);
    let mut body = format!(
        "{}\n\nRule: {}\nSeverity: {}\nCategory: {}\nScope: {}\nValue: {} (threshold {})\nFired at: {}\n",
        i.summary,
        i.rule,
        i.severity,
        i.category,
        w.scope(i),
        w.value(i.value),
        w.value(i.threshold),
        i.fired_at,
    );
    if let Some(at) = &i.resolved_at {
        body.push_str(&format!("Resolved at: {at}\n"));
    }
    if let Some(r) = &i.runbook_url {
        body.push_str(&format!("Runbook: {r}\n"));
    }
    if let Some(l) = link {
        body.push_str(&format!("\n{l}\n"));
    }
    (subject.chars().take(200).collect(), body)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;
    use crate::alerting::instance::Impact;

    fn instance() -> Instance {
        Instance {
            id: "a1".into(),
            rule: "lat".into(),
            fingerprint: "fp".into(),
            category: "latency".into(),
            severity: "warning".into(),
            status: "firing".into(),
            snoozed_until: None,
            fired_at: "2026-10-01T00:00:00Z".into(),
            acknowledged_at: None,
            acknowledged_by: None,
            resolved_at: None,
            resolved_by: None,
            summary: "p95_latency_ms 5000 > 4000".into(),
            scope: BTreeMap::from([("provider".to_owned(), "p".to_owned())]),
            value: Some(5000.0),
            threshold: Some(4000.0),
            owner_team: None,
            runbook_url: Some("https://runbooks/lat".into()),
            impact: Impact::default(),
            timeline: Vec::new(),
        }
    }

    #[test]
    fn pagerduty_actions_share_the_fingerprint() {
        let i = instance();
        let t = pagerduty(&i, Phase::Fired, "rk", Some("https://c/ui/alerts/a1")).unwrap();
        assert_eq!(t["event_action"], "trigger");
        assert_eq!(t["dedup_key"], "fp");
        assert_eq!(t["payload"]["severity"], "warning");
        assert_eq!(t["payload"]["component"], "p");
        let r = pagerduty(&i, Phase::Resolved, "rk", None).unwrap();
        assert_eq!(
            (r["event_action"].as_str(), r["dedup_key"].as_str()),
            (Some("resolve"), Some("fp"))
        );
        assert!(r.get("payload").is_none());
        assert_eq!(
            pagerduty(&i, Phase::Acknowledged, "rk", None).unwrap()["event_action"],
            "acknowledge"
        );
    }

    #[test]
    fn slack_and_email_carry_the_facts() {
        let i = instance();
        let doc = json!({"providers": {"p": {"display_name": "Primary"}}});
        let w = Wording {
            doc: &doc,
            metric: Some("p95_latency_ms"),
        };
        let s = slack(&i, Phase::Fired, Some("https://c/ui/alerts/a1"), w);
        let text = s.to_string();
        for fact in [
            "WARNING",
            "provider Primary",
            "5,000 ms (threshold 4,000 ms)",
            "Runbook",
            "https://c/ui/alerts/a1",
        ] {
            assert!(text.contains(fact), "{fact} missing in {text}");
        }
        let (subject, body) = email(&i, Phase::Resolved, None, w);
        assert!(subject.contains("Resolved") && body.contains("Scope: provider Primary"));
        assert!(body.contains("Value: 5,000 ms (threshold 4,000 ms)"), "{body}");
        assert_eq!(event_data(&i, Phase::Fired)["fingerprint"], "fp");
        assert_eq!(Phase::Escalated.event_type(), "alert.fired");
    }

    #[test]
    fn money_ratios_and_budgets_read_as_people_write_them() {
        let doc = json!({"projects": {"zen": {"display_name": "Zendesk assistant"}}});
        let mut i = instance();
        i.value = Some(2_067.443_506_045);
        i.threshold = Some(1_200.0);
        i.scope = BTreeMap::from([("budget".to_owned(), "project/zen/monthly".to_owned())]);
        let spend = Wording {
            doc: &doc,
            metric: Some("spend_usd"),
        };
        let (_, body) = email(&i, Phase::Fired, None, spend);
        assert!(body.contains("Value: $2,067.44 (threshold $1,200.00)"), "{body}");
        assert!(
            body.contains("Scope: Budget for project Zendesk assistant (monthly)"),
            "{body}"
        );
        i.value = Some(0.0725);
        i.threshold = Some(0.05);
        let rate = Wording {
            doc: &doc,
            metric: Some("error_rate"),
        };
        assert!(
            slack(&i, Phase::Fired, None, rate)
                .to_string()
                .contains("7.3% (threshold 5%)")
        );
        i.value = Some(2.123_456);
        let unknown = Wording {
            doc: &doc,
            metric: None,
        };
        assert!(email(&i, Phase::Fired, None, unknown).1.contains("Value: 2.12 "));
    }
}
