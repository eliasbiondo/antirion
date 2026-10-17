//! The alerting part of a generation, compiled from the resolved document: rules, channels,
//! service-level objectives, audit sinks and OTLP metric export, with every default applied.

use std::collections::BTreeMap;
use std::time::Duration;

use antirion_config::scalar::parse_duration;
use antirion_config::{ConfigError, DetailCode, DocPath};
use serde_json::{Map, Value};

/// Alert severities, lowest first.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

impl Severity {
    #[must_use]
    pub fn parse(s: &str) -> Option<Severity> {
        match s {
            "info" => Some(Severity::Info),
            "warning" => Some(Severity::Warning),
            "critical" => Some(Severity::Critical),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Critical => "critical",
        }
    }
}

/// `gt`, `gte`, `lt` or `lte`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Operator {
    Gt,
    Gte,
    Lt,
    Lte,
}

impl Operator {
    fn parse(s: &str) -> Operator {
        match s {
            "gte" => Operator::Gte,
            "lt" => Operator::Lt,
            "lte" => Operator::Lte,
            _ => Operator::Gt,
        }
    }

    #[must_use]
    pub fn holds(self, value: f64, threshold: f64) -> bool {
        match self {
            Operator::Gt => value > threshold,
            Operator::Gte => value >= threshold,
            Operator::Lt => value < threshold,
            Operator::Lte => value <= threshold,
        }
    }

    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match self {
            Operator::Gt => ">",
            Operator::Gte => "≥",
            Operator::Lt => "<",
            Operator::Lte => "≤",
        }
    }
}

/// What makes a rule fire.
#[derive(Clone, Debug, PartialEq)]
pub enum Trigger {
    Metric {
        metric: String,
        operator: Operator,
        threshold: f64,
    },
    Event {
        events: Vec<String>,
    },
}

/// One `observability.alerts.rules` entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Rule {
    pub name: String,
    pub enabled: bool,
    pub trigger: Trigger,
    pub category: String,
    pub severity: Severity,
    /// Scope dimension → value, ordered by dimension.
    pub scope: BTreeMap<String, String>,
    pub window_us: i64,
    pub for_us: i64,
    pub cooldown_us: i64,
    pub resolve_after_us: i64,
    pub channels: Vec<String>,
    pub escalation: Vec<String>,
    pub owner_team: Option<String>,
    pub runbook_url: Option<String>,
    pub description: Option<String>,
    /// Instances that fire during a maintenance window are created snoozed until its end.
    pub silence_during_maintenance: bool,
}

/// Where a channel sends, secrets resolved.
#[derive(Clone, PartialEq, Eq)]
pub enum Target {
    Slack { webhook_url: String },
    PagerDuty { routing_key: String, events_url: String },
    Email { to: Vec<String> },
    Webhook { webhook: String },
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.kind())
    }
}

impl Target {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Target::Slack { .. } => "slack",
            Target::PagerDuty { .. } => "pagerduty",
            Target::Email { .. } => "email",
            Target::Webhook { .. } => "webhook",
        }
    }
}

/// One `observability.alerts.channels` entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    pub name: String,
    pub enabled: bool,
    pub min_severity: Severity,
    pub target: Target,
}

/// One `observability.slos` entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Slo {
    pub name: String,
    /// `availability` or `latency`.
    pub kind: String,
    pub objective: f64,
    pub latency_threshold_ms: Option<f64>,
    pub window: String,
    pub window_us: i64,
    pub scope: BTreeMap<String, String>,
}

/// One `governance.audit_sinks` entry, secrets resolved.
#[derive(Clone, PartialEq, Eq)]
pub struct AuditSink {
    pub name: String,
    pub enabled: bool,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub json_array: bool,
    pub batch_max_records: usize,
    pub flush_interval: Duration,
    pub max_retry_us: i64,
}

impl std::fmt::Debug for AuditSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditSink")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// `observability.metrics.otlp` when enabled, secrets resolved.
#[derive(Clone, PartialEq, Eq)]
pub struct OtlpMetrics {
    pub endpoint: String,
    /// `grpc`, `http/protobuf` or `http/json`.
    pub protocol: String,
    pub interval: Duration,
    pub headers: Vec<(String, String)>,
}

impl std::fmt::Debug for OtlpMetrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OtlpMetrics")
            .field("protocol", &self.protocol)
            .finish_non_exhaustive()
    }
}

/// The alerting settings of one generation.
#[derive(Clone, Debug, PartialEq)]
pub struct AlertingConfig {
    pub evaluation_interval_us: i64,
    pub instance_retention_us: i64,
    pub rules: Vec<Rule>,
    pub channels: BTreeMap<String, Channel>,
    pub slos: Vec<Slo>,
    pub audit_sinks: Vec<AuditSink>,
    pub otlp_metrics: Option<OtlpMetrics>,
    /// `auth.admin.public_origin`, for links to the console.
    pub console_origin: Option<String>,
}

impl Default for AlertingConfig {
    fn default() -> AlertingConfig {
        AlertingConfig::compile(&Value::Null)
    }
}

pub(crate) fn micros(d: Duration) -> i64 {
    i64::try_from(d.as_micros()).unwrap_or(i64::MAX)
}

fn duration_us(v: &Value, key: &str, default: &str) -> i64 {
    let d = v
        .get(key)
        .and_then(Value::as_str)
        .and_then(parse_duration)
        .or_else(|| parse_duration(default))
        .unwrap_or_default();
    micros(d)
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn scope(v: Option<&Value>) -> BTreeMap<String, String> {
    v.and_then(Value::as_object)
        .into_iter()
        .flat_map(Map::iter)
        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
        .collect()
}

pub(crate) fn headers(v: Option<&Value>) -> Vec<(String, String)> {
    v.and_then(Value::as_object)
        .into_iter()
        .flat_map(Map::iter)
        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
        .collect()
}

fn entries<'a>(doc: &'a Value, pointer: &str) -> impl Iterator<Item = (&'a String, &'a Value)> {
    doc.pointer(pointer)
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(Map::iter)
}

fn rule(name: &str, r: &Value) -> Rule {
    let trigger = if r.get("kind").and_then(Value::as_str) == Some("event") {
        Trigger::Event {
            events: strings(r.get("events")),
        }
    } else {
        Trigger::Metric {
            metric: text(r, "metric").unwrap_or_default(),
            operator: Operator::parse(r.get("operator").and_then(Value::as_str).unwrap_or("gt")),
            threshold: r.get("threshold").and_then(Value::as_f64).unwrap_or(0.0),
        }
    };
    Rule {
        name: name.to_owned(),
        enabled: r.get("enabled").and_then(Value::as_bool).unwrap_or(true),
        trigger,
        category: text(r, "category").unwrap_or_default(),
        severity: r
            .get("severity")
            .and_then(Value::as_str)
            .and_then(Severity::parse)
            .unwrap_or(Severity::Warning),
        scope: scope(r.get("scope")),
        window_us: duration_us(r, "window", "5m"),
        for_us: duration_us(r, "for", "0s"),
        cooldown_us: duration_us(r, "cooldown", "15m"),
        resolve_after_us: duration_us(r, "resolve_after", "5m"),
        channels: strings(r.get("channels")),
        escalation: strings(r.get("escalation")),
        owner_team: text(r, "owner_team"),
        runbook_url: text(r, "runbook_url"),
        description: text(r, "description"),
        silence_during_maintenance: r
            .get("silence_during_maintenance")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

fn channel(name: &str, c: &Value) -> Option<Channel> {
    let target = match c.get("type")?.as_str()? {
        "slack" => Target::Slack {
            webhook_url: c.pointer("/slack/webhook_url")?.as_str()?.to_owned(),
        },
        "pagerduty" => Target::PagerDuty {
            routing_key: c.pointer("/pagerduty/routing_key")?.as_str()?.to_owned(),
            events_url: c
                .pointer("/pagerduty/events_url")
                .and_then(Value::as_str)
                .unwrap_or("https://events.pagerduty.com/v2/enqueue")
                .to_owned(),
        },
        "email" => Target::Email {
            to: strings(c.pointer("/email/to")),
        },
        "webhook" => Target::Webhook {
            webhook: c.pointer("/webhook/webhook")?.as_str()?.to_owned(),
        },
        _ => return None,
    };
    Some(Channel {
        name: name.to_owned(),
        enabled: c.get("enabled").and_then(Value::as_bool).unwrap_or(true),
        min_severity: c
            .get("min_severity")
            .and_then(Value::as_str)
            .and_then(Severity::parse)
            .unwrap_or(Severity::Info),
        target,
    })
}

fn audit_sink(name: &str, s: &Value) -> Option<AuditSink> {
    Some(AuditSink {
        name: name.to_owned(),
        enabled: s.get("enabled").and_then(Value::as_bool).unwrap_or(true),
        url: s.get("url")?.as_str()?.to_owned(),
        headers: headers(s.get("headers")),
        json_array: s.get("format").and_then(Value::as_str) == Some("json_array"),
        batch_max_records: s
            .get("batch_max_records")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(500)
            .max(1),
        flush_interval: s
            .get("flush_interval")
            .and_then(Value::as_str)
            .and_then(parse_duration)
            .unwrap_or(Duration::from_secs(5)),
        max_retry_us: duration_us(s, "max_retry_duration", "72h"),
    })
}

impl AlertingConfig {
    /// Compiles the alerting settings of a resolved document.
    #[must_use]
    pub fn compile(resolved: &Value) -> AlertingConfig {
        let alerts = resolved.pointer("/observability/alerts").unwrap_or(&Value::Null);
        let mut rules: Vec<Rule> = entries(resolved, "/observability/alerts/rules")
            .map(|(n, r)| rule(n, r))
            .collect();
        rules.sort_by(|a, b| a.name.cmp(&b.name));
        let slos = entries(resolved, "/observability/slos")
            .filter_map(|(n, s)| {
                let window = text(s, "window").unwrap_or_else(|| "30d".to_owned());
                Some(Slo {
                    name: n.clone(),
                    kind: text(s, "kind").unwrap_or_else(|| "availability".to_owned()),
                    objective: s.get("objective")?.as_f64()?,
                    latency_threshold_ms: s.get("latency_threshold_ms").and_then(Value::as_f64),
                    window_us: duration_us(s, "window", "30d"),
                    window,
                    scope: scope(s.get("scope")),
                })
            })
            .collect();
        let otlp = resolved.pointer("/observability/metrics/otlp");
        let otlp_metrics = otlp
            .filter(|o| o.get("enabled") == Some(&Value::Bool(true)))
            .and_then(|o| {
                Some(OtlpMetrics {
                    endpoint: o.get("endpoint")?.as_str()?.to_owned(),
                    protocol: text(o, "protocol").unwrap_or_else(|| "http/protobuf".to_owned()),
                    interval: o
                        .get("interval")
                        .and_then(Value::as_str)
                        .and_then(parse_duration)
                        .filter(|d| !d.is_zero())
                        .unwrap_or(Duration::from_secs(15)),
                    headers: headers(o.get("headers")),
                })
            });
        AlertingConfig {
            evaluation_interval_us: duration_us(alerts, "evaluation_interval", "1m"),
            instance_retention_us: duration_us(alerts, "instance_retention", "90d"),
            rules,
            channels: entries(resolved, "/observability/alerts/channels")
                .filter_map(|(n, c)| Some((n.clone(), channel(n, c)?)))
                .collect(),
            slos,
            audit_sinks: entries(resolved, "/governance/audit_sinks")
                .filter_map(|(n, s)| audit_sink(n, s))
                .collect(),
            otlp_metrics,
            console_origin: resolved
                .pointer("/auth/admin/public_origin")
                .and_then(Value::as_str)
                .map(|s| s.trim_end_matches('/').to_owned()),
        }
    }

    #[must_use]
    pub fn rule(&self, name: &str) -> Option<&Rule> {
        self.rules.iter().find(|r| r.name == name)
    }
}

/// Metrics that are ratios of requests: their thresholds lie between 0 and 1 (0 % and 100 %).
const SHARES: [&str; 4] = [
    "error_rate",
    "upstream_5xx_rate",
    "fallback_rate",
    "cache_hit_rate",
];

/// The rules of `observability.alerts` the schema cannot state: a share's threshold is at most 1,
/// and a Slack incoming webhook written as a value (not a secret reference) is an http(s) URL.
#[must_use]
pub fn check_document(doc: &Value) -> Vec<ConfigError> {
    let at = || DocPath::root().key("observability").key("alerts");
    let mut errors = Vec::new();
    for (name, rule) in doc
        .pointer("/observability/alerts/rules")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let metric = rule.get("metric").and_then(Value::as_str).unwrap_or_default();
        if let Some(t) = rule.get("threshold").and_then(Value::as_f64)
            && SHARES.contains(&metric)
            && !(0.0..=1.0).contains(&t)
        {
            errors.push(ConfigError::new(
                &at().key("rules").key(name).key("threshold"),
                DetailCode::OutOfRange,
                format!("{metric} is a share of requests: the threshold is between 0 and 1 (0 % and 100 %)"),
            ));
        }
    }
    for (name, channel) in doc
        .pointer("/observability/alerts/channels")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let Some(url) = channel.pointer("/slack/webhook_url").and_then(Value::as_str) else {
            continue;
        };
        if url.starts_with("${") || url == "[REDACTED]" {
            continue;
        }
        let web = url.parse::<http::Uri>().is_ok_and(|u| {
            matches!(u.scheme_str(), Some("https" | "http")) && u.host().is_some_and(|h| !h.is_empty())
        });
        if !web {
            errors.push(ConfigError::new(
                &at().key("channels").key(name).key("slack").key("webhook_url"),
                DetailCode::InvalidFormat,
                "must be a URL: the incoming webhook Slack gives the channel",
            ));
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn defaults_and_triggers() {
        let doc = json!({"observability": {"alerts": {
            "evaluation_interval": "10s",
            "rules": {
                "lat": {"category": "latency", "metric": "p95_latency_ms", "threshold": 4000,
                        "scope": {"provider": "p"}, "window": "1m", "channels": ["c"]},
                "brk": {"kind": "event", "category": "availability", "severity": "critical",
                        "events": ["deployment.state_changed"], "enabled": false}
            },
            "channels": {
                "c": {"type": "slack", "slack": {"webhook_url": "https://hooks/x"}, "min_severity": "warning"},
                "pd": {"type": "pagerduty", "pagerduty": {"routing_key": "rk"}}
            }
        }}, "governance": {"audit_sinks": {"siem": {"url": "https://s"}}}});
        let c = AlertingConfig::compile(&doc);
        assert_eq!(c.evaluation_interval_us, 10_000_000);
        assert_eq!(c.instance_retention_us, 90 * 86_400_000_000);
        let lat = c.rule("lat").unwrap();
        assert!(lat.enabled);
        assert_eq!(lat.severity, Severity::Warning);
        assert_eq!(lat.window_us, 60_000_000);
        assert_eq!(lat.cooldown_us, 15 * 60_000_000);
        assert_eq!(lat.scope.get("provider").map(String::as_str), Some("p"));
        assert!(
            matches!(&lat.trigger, Trigger::Metric { operator: Operator::Gt, threshold, .. } if *threshold == 4000.0)
        );
        let brk = c.rule("brk").unwrap();
        assert!(!brk.enabled);
        assert!(matches!(&brk.trigger, Trigger::Event { events } if events == &["deployment.state_changed"]));
        assert_eq!(c.channels["c"].min_severity, Severity::Warning);
        assert!(matches!(&c.channels["pd"].target,
            Target::PagerDuty { events_url, .. } if events_url == "https://events.pagerduty.com/v2/enqueue"));
        let sink = &c.audit_sinks[0];
        assert_eq!(
            (sink.batch_max_records, sink.max_retry_us),
            (500, 72 * 3_600_000_000)
        );
        assert!(!sink.json_array);
        assert!(c.otlp_metrics.is_none());
    }

    #[test]
    fn operators_and_severities_order() {
        assert!(Operator::Gt.holds(2.0, 1.0) && !Operator::Gt.holds(1.0, 1.0));
        assert!(Operator::Gte.holds(1.0, 1.0) && Operator::Lte.holds(1.0, 1.0));
        assert!(Operator::Lt.holds(0.5, 1.0));
        assert!(Severity::Critical > Severity::Warning && Severity::Warning > Severity::Info);
    }

    #[test]
    fn shares_stay_within_one_and_slack_webhooks_are_urls() {
        let doc = serde_json::json!({"observability": {"alerts": {
            "rules": {
                "too_high": {"category": "errors", "metric": "error_rate", "threshold": 1.5},
                "fine": {"category": "errors", "metric": "error_rate", "threshold": 0.05},
                "baseline": {"category": "latency", "metric": "latency_baseline_ratio", "threshold": 3}
            },
            "channels": {
                "bad": {"type": "slack", "slack": {"webhook_url": "not-a-url"}},
                "relative": {"type": "slack", "slack": {"webhook_url": "/services/T/B/x"}},
                "relay": {"type": "slack", "slack": {"webhook_url": "http://relay.internal/slack"}},
                "good": {"type": "slack", "slack": {"webhook_url": "https://hooks.slack.com/services/T/B/x"}},
                "secret": {"type": "slack", "slack": {"webhook_url": "${env:SLACK_HOOK}"}}
            }
        }}});
        let paths: Vec<String> = check_document(&doc).into_iter().map(|e| e.path).collect();
        assert_eq!(
            paths,
            [
                "observability.alerts.rules.too_high.threshold",
                "observability.alerts.channels.bad.slack.webhook_url",
                "observability.alerts.channels.relative.slack.webhook_url",
            ]
        );
    }
}
