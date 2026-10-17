//! The retry and timeout policy of a request (`policies.resilience`, merged over the levels that
//! apply), and the backoff before a retry of the same deployment.

use std::time::Duration;

use antirion_config::scalar::parse_duration;
use serde_json::Value;

use crate::class::Class;

/// Backoff settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Backoff {
    pub initial: Duration,
    pub max: Duration,
    pub multiplier: f64,
    pub jitter: f64,
}

impl Backoff {
    /// The delay before the `n`-th retry of the same deployment (1-based), for a uniform draw `u`
    /// in [0, 1): `min(max, initial × multiplier^(n−1)) × (1 − jitter × u)`, saturating at `max`.
    #[must_use]
    pub fn delay(&self, n: u32, u: f64) -> Duration {
        let mut base = self.initial.min(self.max);
        for _ in 1..n {
            if base >= self.max || base.is_zero() || self.multiplier <= 1.0 {
                break;
            }
            base = Duration::try_from_secs_f64(base.as_secs_f64() * self.multiplier)
                .unwrap_or(self.max)
                .min(self.max);
        }
        let factor = 1.0 - self.jitter.clamp(0.0, 1.0) * u.clamp(0.0, 1.0);
        Duration::try_from_secs_f64(base.as_secs_f64() * factor).unwrap_or(base)
    }
}

/// The `retry` member.
#[derive(Clone, Debug, PartialEq)]
pub struct Retry {
    pub max_attempts: u32,
    pub tier_retries: u32,
    pub min_attempt_time: Duration,
    pub retry_after_max_wait: Duration,
    pub backoff: Backoff,
    pub fallback: bool,
    /// The Model a `context_length` verdict moves to, once per request.
    pub context_length_fallback: Option<String>,
    /// The Model a `content_policy` verdict moves to, once per request.
    pub content_policy_fallback: Option<String>,
    /// `retry.classes`: per class, the actions it is restricted or extended to, and whether a
    /// timeout may be retried on the same key.
    pub classes: Vec<(Class, ClassOverride)>,
    /// `retry.retry_on_response`: 2xx answers that are failures of this route.
    pub on_response: Vec<crate::predicate::ResponsePredicate>,
}

/// One class's `retry.classes` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassOverride {
    /// `actions`, when given: rotate key, next deployment, retry same.
    pub actions: Option<(bool, bool, bool)>,
    pub retry_same_any_key: bool,
}

fn class_overrides(v: &Value) -> Vec<(Class, ClassOverride)> {
    at(v, &["retry", "classes"])
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(name, o)| {
            let class = Class::parse(name)?;
            let actions = o.get("actions").and_then(Value::as_array).map(|list| {
                let has = |a: &str| list.iter().any(|x| x.as_str() == Some(a));
                (has("rotate_key"), has("next_deployment"), has("retry_same"))
            });
            let any_key = o
                .get("retry_same_any_key")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Some((
                class,
                ClassOverride {
                    actions,
                    retry_same_any_key: any_key,
                },
            ))
        })
        .collect()
}

impl Retry {
    /// The actions a failure of `class` allows for a request of `kind`: the decision table's,
    /// as `retry.classes` restricts or extends them.
    #[must_use]
    pub fn actions(&self, class: Class, kind: crate::class::RequestKind) -> crate::class::Actions {
        use crate::class::{Actions, RetrySame};
        let table = class.actions();
        let configured = match self.classes.iter().find(|(c, _)| *c == class) {
            None => table,
            Some((_, o)) => {
                let mut a = match o.actions {
                    None => table,
                    Some((rotate_key, next_deployment, retry_same)) => Actions {
                        rotate_key,
                        next_deployment,
                        retry_same: match (retry_same, table.retry_same) {
                            (false, _) => RetrySame::No,
                            (true, RetrySame::No) => RetrySame::Backoff,
                            (true, r) => r,
                        },
                    },
                };
                if o.retry_same_any_key && a.retry_same == RetrySame::UntriedKey {
                    a.retry_same = RetrySame::Backoff;
                }
                a
            }
        };
        match kind {
            crate::class::RequestKind::SideEffecting if !class.pre_send() => Actions::FAIL,
            crate::class::RequestKind::ResourceBound => Actions {
                next_deployment: false,
                ..configured
            },
            _ => configured,
        }
    }

    /// The Model a verdict of `class` falls back to, when one is configured.
    #[must_use]
    pub fn class_fallback(&self, class: Class) -> Option<&str> {
        match class {
            Class::ContextLength => self.context_length_fallback.as_deref(),
            Class::ContentPolicy => self.content_policy_fallback.as_deref(),
            _ => None,
        }
    }
}

/// The `timeouts` member; `None` is a disabled timer.
#[derive(Clone, Debug, PartialEq)]
pub struct Timeouts {
    pub connect: Duration,
    pub response_headers: Option<Duration>,
    pub ttft: Option<Duration>,
    pub stream_idle: Option<Duration>,
    pub attempt: Option<Duration>,
    pub deadline: Option<Duration>,
    pub stream_total: Option<Duration>,
    /// The merged block states `ttft`; otherwise a model's catalog hint replaces the default.
    pub ttft_set: bool,
}

/// The time-to-first-token timeout when nothing sets one.
pub const DEFAULT_TTFT: Duration = Duration::from_secs(300);

impl Timeouts {
    /// The time-to-first-token timeout of an attempt on a model whose catalog entry has `hint`:
    /// the hint replaces the default, but not a configured value nor a client's shorter read
    /// timeout.
    #[must_use]
    pub fn ttft_with_hint(&self, hint: Option<Duration>) -> Option<Duration> {
        match (hint, self.ttft) {
            (Some(h), Some(t)) if !self.ttft_set => Some(if t < DEFAULT_TTFT { t.min(h) } else { h }),
            _ => self.ttft,
        }
    }
}

/// A merged `policies.resilience` block.
#[derive(Clone, Debug, PartialEq)]
pub struct Policy {
    pub retry: Retry,
    pub timeouts: Timeouts,
    /// `hedging`, when enabled.
    pub hedging: Option<crate::hedging::Hedging>,
}

fn at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(v, |cur, seg| cur.get(*seg))
}

fn duration(v: &Value, path: &[&str], default: Option<Duration>) -> Option<Duration> {
    match at(v, path) {
        Some(Value::String(s)) if s == "none" => None,
        Some(Value::String(s)) => parse_duration(s).or(default),
        _ => default,
    }
}

fn int(v: &Value, path: &[&str], default: u32) -> u32 {
    at(v, path)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(default)
}

fn model(v: &Value, class: &str) -> Option<String> {
    at(v, &["retry", "class_fallbacks", class])
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn float(v: &Value, path: &[&str], default: f64) -> f64 {
    at(v, path).and_then(Value::as_f64).unwrap_or(default)
}

impl Policy {
    /// Reads a merged `policies.resilience` value; absent fields take their defaults.
    #[must_use]
    pub fn from_value(v: &Value) -> Policy {
        let secs = |n| Some(Duration::from_secs(n));
        let ms = |n| Some(Duration::from_millis(n));
        let max_attempts = int(v, &["retry", "max_attempts"], 5).clamp(1, 10);
        Policy {
            retry: Retry {
                max_attempts,
                tier_retries: int(v, &["retry", "tier_retries"], 2).min(max_attempts.saturating_sub(1)),
                min_attempt_time: duration(v, &["retry", "min_attempt_time"], secs(1)).unwrap_or_default(),
                retry_after_max_wait: duration(v, &["retry", "retry_after_max_wait"], secs(10))
                    .unwrap_or_default(),
                backoff: Backoff {
                    initial: duration(v, &["retry", "backoff", "initial"], ms(500)).unwrap_or_default(),
                    max: duration(v, &["retry", "backoff", "max"], secs(8)).unwrap_or_default(),
                    multiplier: float(v, &["retry", "backoff", "multiplier"], 2.0),
                    jitter: float(v, &["retry", "backoff", "jitter"], 0.5),
                },
                fallback: at(v, &["retry", "fallback"])
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                context_length_fallback: model(v, "context_length"),
                content_policy_fallback: model(v, "content_policy"),
                classes: class_overrides(v),
                on_response: crate::predicate::predicates(v),
            },
            timeouts: Timeouts {
                connect: duration(v, &["timeouts", "connect"], secs(10)).unwrap_or(Duration::from_secs(10)),
                response_headers: duration(v, &["timeouts", "response_headers"], secs(60)),
                ttft: duration(v, &["timeouts", "ttft"], Some(DEFAULT_TTFT)),
                stream_idle: duration(v, &["timeouts", "stream_idle"], secs(120)),
                attempt: duration(v, &["timeouts", "attempt"], secs(600)),
                deadline: duration(v, &["timeouts", "deadline"], secs(900)),
                stream_total: duration(v, &["timeouts", "stream_total"], None),
                ttft_set: at(v, &["timeouts", "ttft"]).is_some(),
            },
            hedging: crate::hedging::Hedging::from_value(v),
        }
    }
}

impl Default for Policy {
    fn default() -> Policy {
        Policy::from_value(&Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn class_overrides_restrict_and_extend() {
        use crate::class::{RequestKind, RetrySame};
        let p = Policy::from_value(&json!({"retry": {"classes": {
            "rate_limited": {"actions": []},
            "timeout": {"retry_same_any_key": true},
            "tls_error": {"actions": ["retry_same"]}
        }}}));
        let g = RequestKind::Generation;
        let rl = p.retry.actions(Class::RateLimited, g);
        assert_eq!(
            (rl.rotate_key, rl.next_deployment, rl.retry_same),
            (false, false, RetrySame::No)
        );
        assert_eq!(p.retry.actions(Class::Timeout, g).retry_same, RetrySame::Backoff);
        assert_eq!(p.retry.actions(Class::TlsError, g).retry_same, RetrySame::Backoff);
        assert!(!p.retry.actions(Class::TlsError, g).next_deployment);
        assert_eq!(
            p.retry.actions(Class::ServerError, g),
            Class::ServerError.actions()
        );
    }

    #[test]
    fn defaults() {
        let p = Policy::default();
        assert_eq!(p.retry.max_attempts, 5);
        assert_eq!(p.retry.tier_retries, 2);
        assert_eq!(p.timeouts.stream_total, None);
        assert_eq!(p.timeouts.deadline, Some(Duration::from_secs(900)));
        let none = Policy::from_value(&json!({"timeouts": {"stream_idle": "none"}}));
        assert_eq!(none.timeouts.stream_idle, None);
        let fb = Policy::from_value(&json!({"retry": {"class_fallbacks": {"context_length": "big"}}}));
        assert_eq!(fb.retry.class_fallback(Class::ContextLength), Some("big"));
        assert_eq!(fb.retry.class_fallback(Class::ContentPolicy), None);
        assert_eq!(fb.retry.class_fallback(Class::ServerError), None);
    }

    #[test]
    fn backoff_follows_the_formula_and_saturates() {
        let b = Policy::default().retry.backoff;
        assert_eq!(b.delay(1, 0.0), Duration::from_millis(500));
        assert_eq!(b.delay(2, 0.0), Duration::from_secs(1));
        assert_eq!(
            b.delay(1, 0.999_999),
            Duration::from_secs_f64(0.5 * (1.0 - 0.5 * 0.999_999))
        );
        assert_eq!(b.delay(60, 0.0), Duration::from_secs(8));
        assert_eq!(b.delay(u32::MAX, 0.0), Duration::from_secs(8));
        let huge = Backoff {
            initial: Duration::from_millis(100),
            max: Duration::from_millis(400),
            multiplier: 10.0,
            jitter: 0.0,
        };
        assert_eq!(huge.delay(9, 0.0), Duration::from_millis(400));
    }
}
