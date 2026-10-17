//! The `x-antirion-cache-control` request header: its grammar, and the checks against the policy and
//! the key's permissions.

use std::time::Duration;

use crate::policy::CachePolicy;

/// The directives of one request.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Directives {
    pub no_cache: bool,
    pub no_store: bool,
    pub bypass: bool,
    pub max_age: Option<u64>,
    pub ttl: Option<u64>,
    pub namespace: Option<String>,
    pub threshold: Option<f64>,
    pub opt_in: bool,
    /// The directive names in the order given, in the header's spelling (`no-cache`).
    pub given: Vec<&'static str>,
}

/// Why the directives were refused, with the directive the message names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Unknown, malformed, out of range or repeated: 400 `invalid_cache_directive`.
    Invalid(String),
    /// Not in `request_overridable`, or changes a locked field: 403 `override_not_allowed`.
    NotOverridable(&'static str),
    /// A relaxing value from a key without `override:relax`: 403 `permission_required`.
    NeedsRelax(&'static str),
    /// `ttl` above the effective `max_ttl`: 400 `cache_ttl_above_max`.
    TtlAboveMax,
}

const NAMES: &[&str] = &[
    "no-cache",
    "no-store",
    "bypass",
    "max-age",
    "ttl",
    "namespace",
    "threshold",
    "use",
];

fn trim(s: &str) -> &str {
    s.trim_matches(|c| c == ' ' || c == '\t')
}

fn integer(v: &str) -> Option<u64> {
    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    v.parse().ok()
}

fn ratio(v: &str) -> Option<f64> {
    let ok = !v.is_empty()
        && v.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && v.bytes().filter(|b| *b == b'.').count() <= 1
        && v.bytes().next().is_some_and(|b| b.is_ascii_digit());
    let x: f64 = if ok { v.parse().ok()? } else { return None };
    (0.0..=1.0).contains(&x).then_some(x)
}

fn namespace(v: &str) -> bool {
    (1..=64).contains(&v.len())
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

impl Directives {
    /// Parses the header's value (several header lines joined with `,`).
    pub fn parse(value: &str) -> Result<Directives, Refusal> {
        let mut d = Directives::default();
        for part in value.split(',') {
            let part = trim(part);
            let (name, arg) = match part.split_once('=') {
                Some((n, a)) => (trim(n).to_ascii_lowercase(), Some(trim(a))),
                None => (part.to_ascii_lowercase(), None),
            };
            let Some(&known) = NAMES.iter().find(|n| **n == name) else {
                return Err(Refusal::Invalid(if name.is_empty() {
                    part.to_owned()
                } else {
                    name
                }));
            };
            if d.given.contains(&known) {
                return Err(Refusal::Invalid(known.to_owned()));
            }
            d.given.push(known);
            let bad = || Refusal::Invalid(known.to_owned());
            match (known, arg) {
                ("no-cache", None) => d.no_cache = true,
                ("no-store", None) => d.no_store = true,
                ("bypass", None) => d.bypass = true,
                ("use", None) => d.opt_in = true,
                ("max-age", Some(a)) => d.max_age = Some(integer(a).ok_or_else(bad)?),
                ("ttl", Some(a)) => d.ttl = Some(integer(a).ok_or_else(bad)?),
                ("namespace", Some(a)) if namespace(a) => d.namespace = Some(a.to_owned()),
                ("threshold", Some(a)) => d.threshold = Some(ratio(a).ok_or_else(bad)?),
                _ => return Err(bad()),
            }
        }
        Ok(d)
    }

    /// Neither lookup nor store.
    #[must_use]
    pub fn bypasses(&self) -> bool {
        self.bypass || (self.no_cache && self.no_store)
    }

    /// Checks the directives against the effective policy, the fields its levels lock and the key's
    /// `override:relax`, in the order the refusals take precedence.
    pub fn check(&self, policy: &CachePolicy, locked: &[String], can_relax: bool) -> Result<(), Refusal> {
        let is_locked = |field: &str| locked.iter().any(|l| l == field);
        for name in &self.given {
            let policy_name = name.replace('-', "_");
            let locked_field = match *name {
                "ttl" => is_locked("ttl"),
                "threshold" => is_locked("semantic.threshold") || is_locked("semantic"),
                _ => false,
            };
            if locked_field || !policy.overridable.contains(&policy_name) {
                return Err(Refusal::NotOverridable(name));
            }
        }
        for name in &self.given {
            let relaxing = match *name {
                "ttl" => self.ttl.is_some_and(|t| Duration::from_secs(t) > policy.ttl),
                "threshold" => self.threshold.is_some_and(|t| t < policy.threshold),
                _ => false,
            };
            if relaxing && !can_relax {
                return Err(Refusal::NeedsRelax(name));
            }
        }
        if self.ttl.is_some_and(|t| Duration::from_secs(t) > policy.max_ttl) {
            return Err(Refusal::TtlAboveMax);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn invalid(v: &str) -> String {
        match Directives::parse(v) {
            Err(Refusal::Invalid(n)) => n,
            other => panic!("{v}: {other:?}"),
        }
    }

    #[test]
    fn grammar() {
        let d = Directives::parse("NO-STORE , max-age = 3600,\tnamespace=a.b_C-9").unwrap();
        assert!(d.no_store && !d.no_cache);
        assert_eq!(d.max_age, Some(3600));
        assert_eq!(d.namespace.as_deref(), Some("a.b_C-9"));
        assert_eq!(d.given, vec!["no-store", "max-age", "namespace"]);
        assert!(Directives::parse("no-cache, no-store").unwrap().bypasses());
        assert_eq!(Directives::parse("threshold=0.97").unwrap().threshold, Some(0.97));
        for (v, named) in [
            ("lookback=10", "lookback"),
            ("ttl=abc", "ttl"),
            ("ttl=1.5", "ttl"),
            ("ttl=-1", "ttl"),
            ("ttl", "ttl"),
            ("max-age=", "max-age"),
            ("max-age=1e3", "max-age"),
            ("no-store=1", "no-store"),
            ("namespace=", "namespace"),
            ("namespace=has/slash", "namespace"),
            ("no-store, no-store", "no-store"),
            ("max-age=1, MAX-AGE=2", "max-age"),
            ("threshold=5", "threshold"),
        ] {
            assert_eq!(invalid(v), named, "{v}");
        }
        assert_eq!(invalid(&format!("namespace={}", "n".repeat(65))), "namespace");
        assert!(Directives::parse("no-cache;no-store").is_err());
        assert!(Directives::parse("no-store,").is_err());
    }

    #[test]
    fn checks_follow_precedence() {
        let policy = CachePolicy::from_value(&json!({"enabled": true, "ttl": "10s", "max_ttl": "1h",
            "request_overridable": ["no_cache", "ttl"]}));
        let d = |v: &str| Directives::parse(v).unwrap();
        assert_eq!(
            d("no-store").check(&policy, &[], true),
            Err(Refusal::NotOverridable("no-store"))
        );
        assert_eq!(
            d("ttl=11").check(&policy, &[], false),
            Err(Refusal::NeedsRelax("ttl"))
        );
        assert_eq!(d("ttl=10").check(&policy, &[], false), Ok(()));
        assert_eq!(d("ttl=3601").check(&policy, &[], true), Err(Refusal::TtlAboveMax));
        assert_eq!(
            d("ttl=5").check(&policy, &["ttl".to_owned()], true),
            Err(Refusal::NotOverridable("ttl"))
        );
    }
}
