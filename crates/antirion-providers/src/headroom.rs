//! Upstream rate-limit headroom: the most recent rate-limit headers each provider credential's
//! answers carried (OpenAI and Azure `x-ratelimit-*`, Anthropic `anthropic-ratelimit-*`), kept per
//! node for the Admin API and utilization alerts.

use std::collections::HashMap;
use std::time::Duration;

use antirion_core::UtcMicros;
use http::HeaderMap;
use parking_lot::Mutex;
use serde_json::{Value, json};

/// Observations older than this are reported as stale.
pub const STALE_AFTER: Duration = Duration::from_secs(300);

/// One limit's last reading.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Window {
    pub limit: Option<u64>,
    pub remaining: Option<u64>,
    pub reset_at: Option<UtcMicros>,
}

impl Window {
    fn known(&self) -> bool {
        self.limit.is_some() || self.remaining.is_some() || self.reset_at.is_some()
    }

    fn json(&self) -> Value {
        json!({
            "limit": self.limit,
            "remaining": self.remaining,
            "reset_at": self.reset_at.map(UtcMicros::to_rfc3339_seconds),
        })
    }
}

/// The rate limits one answer reported.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct RateLimits {
    pub requests: Window,
    pub tokens: Window,
    pub input_tokens: Window,
    pub output_tokens: Window,
    pub observed_at: UtcMicros,
}

impl RateLimits {
    /// The Admin API's form; `stale` when observed longer than [`STALE_AFTER`] ago.
    #[must_use]
    pub fn json(&self, now: UtcMicros) -> Value {
        let part = |w: &Window| if w.known() { w.json() } else { Value::Null };
        json!({
            "requests": part(&self.requests),
            "tokens": part(&self.tokens),
            "input_tokens": part(&self.input_tokens),
            "output_tokens": part(&self.output_tokens),
            "observed_at": self.observed_at.to_rfc3339_seconds(),
            "stale": now.since(self.observed_at) > STALE_AFTER,
        })
    }
}

fn header<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok()).map(str::trim)
}

fn number(h: &HeaderMap, name: &str) -> Option<u64> {
    header(h, name).and_then(|v| v.parse().ok())
}

/// A reset given as a duration (`1s`, `6m0s`, `20ms`, `1h2m3.5s`, or bare seconds).
fn duration(s: &str) -> Option<Duration> {
    if let Ok(secs) = s.parse::<f64>() {
        return (secs >= 0.0).then(|| Duration::from_secs_f64(secs));
    }
    let mut total = 0.0;
    let mut num = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
            continue;
        }
        let unit = match c {
            'h' => 3600.0,
            'm' if chars.peek() == Some(&'s') => {
                chars.next();
                0.001
            }
            'm' => 60.0,
            's' => 1.0,
            _ => return None,
        };
        total += num.parse::<f64>().ok()? * unit;
        num.clear();
    }
    (num.is_empty() && total >= 0.0).then(|| Duration::from_secs_f64(total))
}

/// A reset given as an RFC 3339 time or as a duration from now.
fn reset(h: &HeaderMap, name: &str, now: UtcMicros) -> Option<UtcMicros> {
    let v = header(h, name)?;
    UtcMicros::parse_rfc3339(v)
        .ok()
        .or_else(|| duration(v).map(|d| now.saturating_add(d)))
}

/// The rate limits an answer's headers report, if any.
#[must_use]
pub fn read(h: &HeaderMap, now: UtcMicros) -> Option<RateLimits> {
    let openai = |kind: &str| Window {
        limit: number(h, &format!("x-ratelimit-limit-{kind}")),
        remaining: number(h, &format!("x-ratelimit-remaining-{kind}")),
        reset_at: reset(h, &format!("x-ratelimit-reset-{kind}"), now),
    };
    let anthropic = |kind: &str| Window {
        limit: number(h, &format!("anthropic-ratelimit-{kind}-limit")),
        remaining: number(h, &format!("anthropic-ratelimit-{kind}-remaining")),
        reset_at: reset(h, &format!("anthropic-ratelimit-{kind}-reset"), now),
    };
    let pick = |a: Window, b: Window| if a.known() { a } else { b };
    let r = RateLimits {
        requests: pick(openai("requests"), anthropic("requests")),
        tokens: pick(openai("tokens"), anthropic("tokens")),
        input_tokens: anthropic("input-tokens"),
        output_tokens: anthropic("output-tokens"),
        observed_at: now,
    };
    [r.requests, r.tokens, r.input_tokens, r.output_tokens]
        .iter()
        .any(Window::known)
        .then_some(r)
}

/// The last readings of every provider credential on this node.
#[derive(Debug, Default)]
pub struct Headroom {
    by_credential: Mutex<HashMap<(String, String), RateLimits>>,
    /// The same readings as minutes of use, for the pressure history.
    pub pressure: crate::pressure::Pressure,
}

impl Headroom {
    /// Keeps what an answer from `provider` with `credential` reported, if anything.
    pub fn observe(&self, provider: &str, credential: &str, headers: &HeaderMap, now: UtcMicros) {
        let quick = headers.contains_key("x-ratelimit-remaining-requests")
            || headers.contains_key("x-ratelimit-remaining-tokens")
            || headers.contains_key("x-ratelimit-limit-requests")
            || headers.contains_key("anthropic-ratelimit-requests-remaining")
            || headers.contains_key("anthropic-ratelimit-tokens-remaining")
            || headers.contains_key("anthropic-ratelimit-input-tokens-remaining");
        if !quick {
            return;
        }
        if let Some(r) = read(headers, now) {
            self.pressure.note(provider, credential, &r);
            self.by_credential
                .lock()
                .insert((provider.to_owned(), credential.to_owned()), r);
        }
    }

    /// Every credential of `provider` with its last reading, by credential name.
    #[must_use]
    pub fn of(&self, provider: &str) -> Vec<(String, RateLimits)> {
        let mut out: Vec<(String, RateLimits)> = self
            .by_credential
            .lock()
            .iter()
            .filter(|((p, _), _)| p == provider)
            .map(|((_, c), r)| (c.clone(), *r))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    pub fn clear(&self) {
        self.by_credential.lock().clear();
        self.pressure.clear();
    }
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;

    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_static(v));
        }
        h
    }

    #[test]
    fn openai_and_anthropic_headers_are_read() {
        let now = UtcMicros(1_000_000);
        let r = read(
            &headers(&[
                ("x-ratelimit-limit-requests", "500"),
                ("x-ratelimit-remaining-requests", "120"),
                ("x-ratelimit-reset-requests", "6m0s"),
                ("x-ratelimit-remaining-tokens", "9000"),
            ]),
            now,
        )
        .unwrap();
        assert_eq!((r.requests.limit, r.requests.remaining), (Some(500), Some(120)));
        assert_eq!(r.requests.reset_at, Some(UtcMicros(361_000_000)));
        assert_eq!(r.tokens.remaining, Some(9000));
        assert_eq!(r.input_tokens, Window::default());
        let a = read(
            &headers(&[
                ("anthropic-ratelimit-requests-limit", "50"),
                ("anthropic-ratelimit-requests-remaining", "49"),
                ("anthropic-ratelimit-requests-reset", "2026-01-01T00:00:00Z"),
                ("anthropic-ratelimit-output-tokens-remaining", "7"),
            ]),
            now,
        )
        .unwrap();
        assert_eq!(a.requests.limit, Some(50));
        assert!(a.requests.reset_at.is_some());
        assert_eq!(a.output_tokens.remaining, Some(7));
        assert!(read(&HeaderMap::new(), now).is_none());
        assert_eq!(duration("20ms"), Some(Duration::from_millis(20)));
        assert_eq!(duration("1h2m3s"), Some(Duration::from_secs(3723)));
    }

    #[test]
    fn readings_are_kept_per_credential_and_age() {
        let book = Headroom::default();
        let h = headers(&[("x-ratelimit-remaining-requests", "3")]);
        book.observe("p", "k1", &h, UtcMicros(0));
        book.observe("p", "k2", &h, UtcMicros(0));
        book.observe("q", "k1", &HeaderMap::new(), UtcMicros(0));
        let of = book.of("p");
        assert_eq!(of.len(), 2);
        assert!(book.of("q").is_empty());
        let json = of[0].1.json(UtcMicros(301_000_000));
        assert_eq!(json["stale"], true);
        assert_eq!(json["requests"]["remaining"], 3);
        assert!(json["input_tokens"].is_null());
    }
}
