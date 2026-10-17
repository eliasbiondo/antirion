//! Alert webhooks: their settings, subscriptions (exact types, `*` and `<prefix>.*`), the event body
//! and its signature. Delivery is `crate::alerting`'s.

use std::time::Duration;

use antirion_config::scalar::parse_duration;
use hmac::{KeyInit, Mac};
use serde_json::{Value, json};
use sha2::Sha256;

/// One entry of `observability.alerts.webhooks`, its secrets resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlertWebhook {
    pub name: String,
    pub url: String,
    /// Subscribed event types; `*` subscribes to all.
    pub events: Vec<String>,
    pub signing_secret: Option<String>,
    pub headers: Vec<(String, String)>,
    pub timeout: Duration,
    pub max_retry_duration: Duration,
    pub queue_capacity: usize,
    /// `false` pauses delivery; events wait until it is resumed or their retry time runs out.
    pub enabled: bool,
}

/// Whether the subscription `pattern` (an exact type, `*` or `<prefix>.*`) covers `kind`.
#[must_use]
pub fn matches(pattern: &str, kind: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some("") => true,
        Some(prefix) if prefix.ends_with('.') => kind.starts_with(prefix),
        _ => pattern == kind,
    }
}

impl AlertWebhook {
    pub(crate) fn subscribed(&self, kind: &str) -> bool {
        self.events.iter().any(|t| matches(t, kind))
    }
}

/// The alert webhooks of a resolved document, by name.
#[must_use]
pub fn compile(resolved: &Value) -> Vec<AlertWebhook> {
    let duration = |w: &Value, name: &str, default: Duration| {
        w.get(name)
            .and_then(Value::as_str)
            .and_then(parse_duration)
            .unwrap_or(default)
    };
    let mut out: Vec<AlertWebhook> = resolved
        .pointer("/observability/alerts/webhooks")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(name, w)| {
            Some(AlertWebhook {
                name: name.clone(),
                url: w.get("url")?.as_str()?.to_owned(),
                events: w
                    .get("events")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                signing_secret: w
                    .get("signing_secret")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
                headers: w
                    .get("headers")
                    .and_then(Value::as_object)
                    .into_iter()
                    .flatten()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                    .collect(),
                timeout: duration(w, "timeout", Duration::from_secs(5)),
                max_retry_duration: duration(w, "max_retry_duration", Duration::from_secs(3600)),
                queue_capacity: w
                    .get("queue_capacity")
                    .and_then(Value::as_u64)
                    .and_then(|n| usize::try_from(n).ok())
                    .unwrap_or(1000),
                enabled: w.get("enabled").and_then(Value::as_bool).unwrap_or(true),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The webhooks subscribed to `kind`; only `only` when it names one.
pub(crate) fn targets<'a>(
    hooks: &'a [AlertWebhook],
    kind: &str,
    only: Option<&str>,
) -> Vec<&'a AlertWebhook> {
    hooks
        .iter()
        .filter(|w| only.is_none_or(|o| o == w.name))
        .filter(|w| w.subscribed(kind))
        .collect()
}

/// The event body.
pub(crate) fn event(id: &str, kind: &str, time: &str, node: &str, data: Value) -> Value {
    json!({"id": id, "type": kind, "time": time, "node_id": node, "data": data})
}

/// `x-antirion-signature`: `t=<unix seconds>,v1=<hex HMAC-SHA256(secret, "<t>.<raw body>")>`.
pub(crate) fn signature(secret: &str, t: i64, body: &[u8]) -> Option<String> {
    let mut mac = <hmac::Hmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes()).ok()?;
    mac.update(t.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    let hex: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Some(format!("t={t},v1={hex}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhooks_and_their_subscriptions() {
        let doc = json!({"observability": {"alerts": {"webhooks": {
            "a": {"url": "http://a/e", "events": ["budget.threshold_crossed"], "timeout": "2s",
                  "signing_secret": "s3", "max_retry_duration": "1m", "queue_capacity": 5},
            "b": {"url": "http://b/e", "events": ["*"]},
            "c": {"url": "http://c/e", "events": ["config.applied"]}
        }}}});
        let hooks = compile(&doc);
        let all = targets(&hooks, "budget.threshold_crossed", None);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].timeout, Duration::from_secs(2));
        assert_eq!(all[0].max_retry_duration, Duration::from_secs(60));
        assert_eq!((all[0].queue_capacity, all[1].queue_capacity), (5, 1000));
        assert_eq!(all[0].signing_secret.as_deref(), Some("s3"));
        assert_eq!(targets(&hooks, "budget.threshold_crossed", Some("b")).len(), 1);
        assert!(targets(&hooks, "budget.threshold_crossed", Some("c")).is_empty());
        let e = event("1", "t", "now", "n", json!({"x": 1}));
        assert_eq!(e["data"]["x"], 1);
    }

    #[test]
    fn signatures_follow_the_published_scheme() {
        let sig = signature("key", 1_700_000_000, b"{}").unwrap();
        let mut mac = <hmac::Hmac<Sha256> as KeyInit>::new_from_slice(b"key").unwrap();
        mac.update(b"1700000000.{}");
        let hex: String = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(sig, format!("t=1700000000,v1={hex}"));
    }

    #[test]
    fn prefixes_cover_their_types() {
        assert!(matches("*", "alert.fired"));
        assert!(matches("alert.*", "alert.fired"));
        assert!(!matches("alert.*", "alerts.fired"));
        assert!(!matches("alert.*", "budget.exhausted"));
        assert!(matches("budget.exhausted", "budget.exhausted"));
        assert!(!matches("budget", "budget.exhausted"));
    }
}
