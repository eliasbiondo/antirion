//! A stored response: the client-visible body, the events of a stream, and the routing identity
//! that produced it.

use serde::{Deserialize, Serialize};

use crate::meta::EntryMeta;

/// One event of a stored stream, without its framing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    pub data: String,
}

/// A stored response: what a hit replays, and the facts its headers and record report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Gateway clock, milliseconds since the Unix epoch.
    pub written_at_ms: i64,
    pub ttl_ms: u64,
    pub content_type: String,
    /// The response in the client dialect's non-streaming shape, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// The data payloads of the stream it was written from, in order, without the terminator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<StoredEvent>>,
    pub provider: String,
    pub deployment: String,
    pub model: String,
    pub upstream_status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_dialect: Option<String>,
    /// The usage member as the client dialect reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<serde_json::Value>,
    /// What the request that wrote it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<EntryMeta>,
}

impl Entry {
    /// When the entry expires.
    #[must_use]
    pub fn expires_at_ms(&self) -> i64 {
        self.written_at_ms
            .saturating_add(i64::try_from(self.ttl_ms).unwrap_or(i64::MAX))
    }

    /// Not served at or after its expiry.
    #[must_use]
    pub fn fresh(&self, now_ms: i64) -> bool {
        now_ms < self.expires_at_ms()
    }

    /// Whole seconds since it was written.
    #[must_use]
    pub fn age_secs(&self, now_ms: i64) -> u64 {
        u64::try_from(now_ms.saturating_sub(self.written_at_ms)).unwrap_or(0) / 1000
    }

    /// The bytes the entry holds, for the memory bound.
    #[must_use]
    pub fn weight(&self) -> u64 {
        let events: usize = self
            .events
            .iter()
            .flatten()
            .map(|e| e.data.len() + e.event.as_ref().map_or(0, String::len) + 16)
            .sum();
        let fixed =
            self.content_type.len() + self.provider.len() + self.deployment.len() + self.model.len() + 128;
        u64::try_from(self.body.as_ref().map_or(0, String::len) + events + fixed).unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freshness_is_strict_at_the_ttl() {
        let e = Entry {
            written_at_ms: 1_000,
            ttl_ms: 3_000,
            content_type: "application/json".into(),
            body: Some("{}".into()),
            events: None,
            provider: "p".into(),
            deployment: "d".into(),
            model: "m".into(),
            upstream_status: 200,
            upstream_dialect: None,
            usage: None,
            meta: None,
        };
        assert!(e.fresh(3_999));
        assert!(!e.fresh(4_000));
        assert_eq!(e.age_secs(3_999), 2);
        let text = serde_json::to_string(&e).unwrap();
        assert_eq!(serde_json::from_str::<Entry>(&text).unwrap(), e);
    }
}
