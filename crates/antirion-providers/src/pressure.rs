//! Rate-limit pressure over time: from each rate-limit reading a provider credential's answers
//! carry, the share of its upstream quota already used (`limit − remaining` of `limit`), kept per
//! minute at its peak. A node collects its minutes in memory until the store takes them; reads put
//! every node's minutes into buckets, per provider.

use std::collections::{BTreeMap, HashMap};

use parking_lot::Mutex;

use crate::headroom::{RateLimits, Window};

const MINUTE_US: i64 = 60_000_000;

/// The peak use of one quota: the most used, and the largest limit, seen in a minute.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Used {
    pub used: Option<u64>,
    pub limit: Option<u64>,
}

impl Used {
    fn of(w: &Window) -> Option<Used> {
        let limit = w.limit.filter(|l| *l > 0)?;
        let remaining = w.remaining?;
        Some(Used {
            used: Some(limit - remaining.min(limit)),
            limit: Some(limit),
        })
    }

    #[allow(clippy::cast_precision_loss)]
    fn ratio(self) -> f64 {
        match (self.used, self.limit) {
            (Some(u), Some(l)) if l > 0 => u as f64 / l as f64,
            _ => 0.0,
        }
    }

    /// The larger of each part.
    #[must_use]
    pub fn merge(self, other: Used) -> Used {
        Used {
            used: self.used.max(other.used),
            limit: self.limit.max(other.limit),
        }
    }
}

/// What tokens a reading counts: the combined window, else whichever of the input and output
/// windows is used more.
fn tokens(r: &RateLimits) -> Option<Used> {
    Used::of(&r.tokens).or_else(|| match (Used::of(&r.input_tokens), Used::of(&r.output_tokens)) {
        (Some(i), Some(o)) => Some(if o.ratio() > i.ratio() { o } else { i }),
        (i, o) => i.or(o),
    })
}

/// One credential's peak use in one minute.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sample {
    pub provider: String,
    pub credential: String,
    /// The minute's start, in microseconds since the epoch.
    pub minute_us: i64,
    pub requests: Used,
    pub tokens: Used,
}

/// `(provider, credential, minute)`.
type MinuteKey = (String, String, i64);

/// The minutes a node has seen and not yet handed to the store.
#[derive(Debug, Default)]
pub struct Pressure {
    minutes: Mutex<HashMap<MinuteKey, (Used, Used)>>,
    /// When the stored history was last trimmed.
    trimmed_us: Mutex<i64>,
}

impl Pressure {
    /// Adds a reading of `provider`'s `credential`; readings without a limit and a remaining
    /// count say nothing about use and are skipped.
    pub fn note(&self, provider: &str, credential: &str, r: &RateLimits) {
        let requests = Used::of(&r.requests);
        let toks = tokens(r);
        if requests.is_none() && toks.is_none() {
            return;
        }
        let minute = r.observed_at.0 - r.observed_at.0.rem_euclid(MINUTE_US);
        let mut map = self.minutes.lock();
        let slot = map
            .entry((provider.to_owned(), credential.to_owned(), minute))
            .or_default();
        slot.0 = slot.0.merge(requests.unwrap_or_default());
        slot.1 = slot.1.merge(toks.unwrap_or_default());
    }

    /// Takes every collected minute.
    pub fn take(&self) -> Vec<Sample> {
        std::mem::take(&mut *self.minutes.lock())
            .into_iter()
            .map(|((provider, credential, minute_us), (requests, tokens))| Sample {
                provider,
                credential,
                minute_us,
                requests,
                tokens,
            })
            .collect()
    }

    pub fn clear(&self) {
        self.minutes.lock().clear();
    }

    /// Whether `every` has passed since the history was last trimmed; if so, now counts as the
    /// last trim.
    pub fn trim_due(&self, now_us: i64, every_us: i64) -> bool {
        let mut last = self.trimmed_us.lock();
        let due = now_us.saturating_sub(*last) >= every_us;
        if due {
            *last = now_us;
        }
        due
    }
}

/// One bucket of a provider's series.
#[derive(Clone, Debug, PartialEq)]
pub struct Bucket {
    pub start_us: i64,
    /// Used over limit, summed over the credentials with a reading; `None` without any.
    pub rpm_utilization: Option<f64>,
    pub tpm_utilization: Option<f64>,
    /// The summed limits of those credentials.
    pub requests_limit: Option<u64>,
    pub tokens_limit: Option<u64>,
}

/// A provider's buckets, in time order.
#[derive(Clone, Debug, PartialEq)]
pub struct Series {
    pub provider: String,
    pub buckets: Vec<Bucket>,
}

/// The bucket starts of `[from, to)`: multiples of `interval` from the one holding `from`, or one
/// bucket starting at `from` without an interval.
#[must_use]
pub fn starts(from: i64, to: i64, interval: Option<i64>) -> Vec<i64> {
    match interval.filter(|i| *i > 0) {
        None => vec![from],
        Some(i) => {
            let mut out = Vec::new();
            let mut s = from - from.rem_euclid(i);
            while s < to {
                out.push(s);
                s += i;
            }
            out
        }
    }
}

fn summed(per_credential: &BTreeMap<&str, Used>) -> (Option<f64>, Option<u64>) {
    let (mut used, mut limit, mut any) = (0u64, 0u64, false);
    for u in per_credential.values() {
        if let (Some(x), Some(l)) = (u.used, u.limit) {
            used = used.saturating_add(x);
            limit = limit.saturating_add(l);
            any = true;
        }
    }
    #[allow(clippy::cast_precision_loss)]
    let ratio = (any && limit > 0).then(|| used as f64 / limit as f64);
    (ratio, any.then_some(limit))
}

/// Every provider's series over `starts` (each bucket running to the next start, the last to
/// `to`), from the samples of every node. A credential's bucket is its peak over the bucket's
/// minutes and the nodes; a provider's is the sum of its credentials' use over the sum of their
/// limits.
#[must_use]
pub fn series(samples: &[Sample], starts: &[i64], to: i64) -> Vec<Series> {
    type Cell<'a> = (BTreeMap<&'a str, Used>, BTreeMap<&'a str, Used>);
    let mut by: BTreeMap<&str, Vec<Cell<'_>>> = BTreeMap::new();
    for s in samples.iter().filter(|s| s.minute_us < to) {
        let Some(first) = starts.first() else {
            break;
        };
        if s.minute_us + MINUTE_US <= *first {
            continue;
        }
        let idx = starts.partition_point(|b| *b <= s.minute_us).saturating_sub(1);
        let cells = by
            .entry(s.provider.as_str())
            .or_insert_with(|| vec![Cell::default(); starts.len()]);
        if let Some((req, tok)) = cells.get_mut(idx) {
            let r = req.entry(s.credential.as_str()).or_default();
            *r = r.merge(s.requests);
            let t = tok.entry(s.credential.as_str()).or_default();
            *t = t.merge(s.tokens);
        }
    }
    by.into_iter()
        .map(|(provider, cells)| Series {
            provider: provider.to_owned(),
            buckets: starts
                .iter()
                .zip(cells)
                .map(|(start, (req, tok))| {
                    let (rpm_utilization, requests_limit) = summed(&req);
                    let (tpm_utilization, tokens_limit) = summed(&tok);
                    Bucket {
                        start_us: *start,
                        rpm_utilization,
                        tpm_utilization,
                        requests_limit,
                        tokens_limit,
                    }
                })
                .collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use antirion_core::UtcMicros;

    use super::*;

    fn reading(at: i64, limit: u64, remaining: u64) -> RateLimits {
        RateLimits {
            requests: Window {
                limit: Some(limit),
                remaining: Some(remaining),
                reset_at: None,
            },
            observed_at: UtcMicros(at),
            ..RateLimits::default()
        }
    }

    #[test]
    fn a_minute_keeps_its_peak() {
        let p = Pressure::default();
        p.note("p", "a", &reading(MINUTE_US + 1, 500, 400));
        p.note("p", "a", &reading(MINUTE_US + 2, 500, 120));
        p.note("p", "a", &reading(MINUTE_US + 3, 500, 300));
        p.note("p", "a", &reading(2 * MINUTE_US, 500, 499));
        let mut got = p.take();
        got.sort_by_key(|s| s.minute_us);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].minute_us, MINUTE_US);
        assert_eq!(got[0].requests.used, Some(380));
        assert_eq!(got[0].tokens, Used::default());
        assert!(p.take().is_empty(), "taken minutes are gone");
    }

    #[test]
    fn readings_without_use_are_skipped() {
        let p = Pressure::default();
        let mut r = reading(0, 500, 1);
        r.requests.limit = None;
        p.note("p", "a", &r);
        assert!(p.take().is_empty());
    }

    #[test]
    fn anthropic_tokens_take_the_busier_window() {
        let r = RateLimits {
            input_tokens: Window {
                limit: Some(1000),
                remaining: Some(900),
                reset_at: None,
            },
            output_tokens: Window {
                limit: Some(100),
                remaining: Some(10),
                reset_at: None,
            },
            ..RateLimits::default()
        };
        assert_eq!(tokens(&r).and_then(|u| u.used), Some(90));
    }

    #[test]
    fn buckets_align_to_the_interval() {
        assert_eq!(starts(90, 400, Some(100)), [0, 100, 200, 300]);
        assert_eq!(starts(90, 400, None), [90]);
    }

    #[test]
    fn credentials_add_up_and_nodes_take_the_peak() {
        let s = |cred: &str, minute: i64, used: u64, limit: u64| Sample {
            provider: "p".into(),
            credential: cred.into(),
            minute_us: minute * MINUTE_US,
            requests: Used {
                used: Some(used),
                limit: Some(limit),
            },
            tokens: Used::default(),
        };
        let samples = [
            s("a", 0, 100, 500),
            s("a", 0, 300, 500),
            s("b", 1, 100, 500),
            s("a", 5, 450, 500),
        ];
        let starts = starts(0, 10 * MINUTE_US, Some(5 * MINUTE_US));
        let got = series(&samples, &starts, 10 * MINUTE_US);
        assert_eq!(got.len(), 1);
        let b = &got[0].buckets;
        assert_eq!(b[0].rpm_utilization, Some(0.4), "(300 + 100) / (500 + 500)");
        assert_eq!(b[0].requests_limit, Some(1000));
        assert_eq!(b[1].rpm_utilization, Some(0.9));
        assert_eq!((b[0].tpm_utilization, b[0].tokens_limit), (None, None));
    }
}
