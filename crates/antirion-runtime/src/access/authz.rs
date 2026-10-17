//! `auth.external_authz`: the hook's settings and the node's cache of its answers. Allow and deny
//! answers are kept for their TTLs per `(hook, key, method, path, model)`; errors never are.

use std::collections::HashMap;
use std::time::Duration;

use antirion_core::{ResourceUid, UtcMicros};
use parking_lot::Mutex;
use serde_json::Value;

/// Answers kept at once; beyond it, answers are not cached until expired ones are swept.
const MAX_ENTRIES: usize = 100_000;

/// The hook's settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalAuthz {
    pub url: String,
    pub timeout: Duration,
    /// Lowercase names of the request headers sent to the hook.
    pub send_headers: Vec<String>,
    /// `fail_mode: open`: an unanswered call lets the request continue.
    pub fail_open: bool,
    pub allow_ttl: Duration,
    pub deny_ttl: Duration,
}

impl ExternalAuthz {
    pub(crate) fn of(v: &Value) -> Option<ExternalAuthz> {
        let duration = |k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .and_then(antirion_config::scalar::parse_duration)
        };
        Some(ExternalAuthz {
            url: v.get("url")?.as_str()?.to_owned(),
            timeout: duration("timeout")
                .filter(|d| !d.is_zero())
                .unwrap_or(Duration::from_millis(200)),
            send_headers: v
                .get("send_headers")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_ascii_lowercase)
                .collect(),
            fail_open: v.get("fail_mode").and_then(Value::as_str) == Some("open"),
            allow_ttl: duration("allow_ttl").unwrap_or(Duration::ZERO),
            deny_ttl: duration("deny_ttl").unwrap_or(Duration::ZERO),
        })
    }
}

/// What the hook answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// Denied, with the reason the hook gave.
    Deny(String),
}

/// The identity of a cached answer.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AuthzKey {
    pub url: String,
    pub key: ResourceUid,
    pub method: String,
    pub path: String,
    pub model: String,
}

/// Cached answers of the hook.
#[derive(Debug, Default)]
pub struct AuthzCache {
    answers: Mutex<HashMap<AuthzKey, (Verdict, i64)>>,
}

impl AuthzCache {
    /// The cached answer for `key`, while its TTL lasts.
    pub fn get(&self, key: &AuthzKey, now: UtcMicros) -> Option<Verdict> {
        let map = self.answers.lock();
        let (v, until) = map.get(key)?;
        (now.0 < *until).then(|| v.clone())
    }

    /// Keeps an answer for its TTL in `hook`; a zero TTL keeps nothing.
    pub fn put(&self, key: AuthzKey, verdict: Verdict, hook: &ExternalAuthz, now: UtcMicros) {
        let ttl = match verdict {
            Verdict::Allow => hook.allow_ttl,
            Verdict::Deny(_) => hook.deny_ttl,
        };
        if ttl.is_zero() {
            return;
        }
        let until = now
            .0
            .saturating_add(i64::try_from(ttl.as_micros()).unwrap_or(i64::MAX));
        let mut map = self.answers.lock();
        if map.len() >= MAX_ENTRIES {
            map.retain(|_, (_, u)| *u > now.0);
            if map.len() >= MAX_ENTRIES {
                return;
            }
        }
        map.insert(key, (verdict, until));
    }

    pub(crate) fn reset(&self) {
        self.answers.lock().clear();
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn answers_are_kept_for_their_ttl() {
        let hook = ExternalAuthz::of(&json!({"url": "https://authz.example/", "deny_ttl": "10s"})).unwrap();
        assert_eq!(hook.timeout, Duration::from_millis(200));
        assert!(!hook.fail_open);
        let cache = AuthzCache::default();
        let key = AuthzKey {
            url: hook.url.clone(),
            key: ResourceUid(1),
            method: "POST".into(),
            path: "/v1/chat/completions".into(),
            model: "m".into(),
        };
        cache.put(key.clone(), Verdict::Allow, &hook, UtcMicros(0));
        assert_eq!(cache.get(&key, UtcMicros(1)), None);
        cache.put(key.clone(), Verdict::Deny("no".into()), &hook, UtcMicros(0));
        assert_eq!(
            cache.get(&key, UtcMicros(9_999_999)),
            Some(Verdict::Deny("no".into()))
        );
        assert_eq!(cache.get(&key, UtcMicros(10_000_000)), None);
    }
}
