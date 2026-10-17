//! Priority classes of a rate limit (`priority_shares`, `priority_mode`): each class, taken from
//! the request's key, owns a share of the limit. In `strict` mode a class never uses more than its
//! share, and the classes without a share of their own count together in the remainder, the share
//! no class claims. In `borrow` mode a class may use the rest of the limit except what the classes
//! with a share have not used of it; the remainder is free to all.

use std::collections::BTreeMap;

use antirion_core::ResourceUid;
use serde_json::Value;

use crate::counter::{CounterId, HoldMode, Rule};
use crate::decl::{DeclError, field};
use crate::math::{Gcra, Window};
use crate::money::{DEC_ONE, Dec};
use crate::plan::PlannedHold;

/// The bucket of classes without a share of their own.
pub const REMAINDER: &str = "*";

/// A limit's priority shares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PriorityShares {
    /// Class → share of the limit.
    pub shares: BTreeMap<String, Dec>,
    /// `borrow` mode; `strict` otherwise.
    pub borrow: bool,
}

impl PriorityShares {
    /// Reads `priority_shares` and `priority_mode`: shares are ratios whose sum is at most 1, and
    /// a mode needs shares.
    pub fn parse(
        shares: &BTreeMap<String, Value>,
        mode: Option<&str>,
    ) -> Result<Option<PriorityShares>, DeclError> {
        let borrow = match mode {
            None | Some("strict") => false,
            Some("borrow") => true,
            Some(other) => return Err(field("priority_mode", format!("unknown mode {other:?}"))),
        };
        if shares.is_empty() {
            return match mode {
                Some(_) => Err(field("priority_mode", "applies only with priority_shares")),
                None => Ok(None),
            };
        }
        let mut out = BTreeMap::new();
        let mut sum = 0u128;
        for (class, v) in shares {
            let share = v
                .as_f64()
                .filter(|_| v.is_number())
                .and_then(Dec::from_ratio)
                .ok_or_else(|| field(&format!("priority_shares.{class}"), "a ratio in [0, 1]"))?;
            sum = sum.saturating_add(share.0);
            out.insert(class.clone(), share);
        }
        if sum > DEC_ONE {
            return Err(field("priority_shares", "shares add up to more than 1"));
        }
        Ok(Some(PriorityShares { shares: out, borrow }))
    }

    /// The share of the classes without one of their own.
    #[must_use]
    pub fn remainder(&self) -> Dec {
        let claimed = self.shares.values().fold(0u128, |a, s| a.saturating_add(s.0));
        Dec(DEC_ONE.saturating_sub(claimed))
    }

    /// The bucket a request of `class` counts in, and its share.
    #[must_use]
    pub fn bucket<'a>(&'a self, class: Option<&'a str>) -> (&'a str, Dec) {
        match class.and_then(|c| self.shares.get_key_value(c)) {
            Some((name, share)) => (name.as_str(), *share),
            None => (REMAINDER, self.remainder()),
        }
    }

    /// Every bucket with its share: each class, then the remainder when it is not empty.
    #[must_use]
    pub fn buckets(&self) -> Vec<(&str, Dec)> {
        let mut out: Vec<(&str, Dec)> = self.shares.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        let rest = self.remainder();
        if !rest.is_zero() {
            out.push((REMAINDER, rest));
        }
        out
    }
}

/// The counter of one bucket of the limit `total` counts in, with its share of the rule.
fn bucket_hold(total: &PlannedHold, bucket: &str, share: Dec, mode: HoldMode) -> PlannedHold {
    let mut h = total.clone();
    h.hold.counter = CounterId::derive(
        "priority",
        total.info.level,
        ResourceUid(0),
        &total.hold.counter.hex(),
        bucket,
        "",
        &[],
    );
    h.hold.rule = match &total.hold.rule {
        Rule::Gcra(g) => Rule::Gcra(Gcra {
            limit: part(share, g.limit),
            burst: part(share, g.burst),
            ..*g
        }),
        Rule::Window(w) => Rule::Window(Window {
            limit: part(share, w.limit),
            ..*w
        }),
        Rule::Concurrency { limit } => Rule::Concurrency {
            limit: part(share, *limit),
        },
        other => other.clone(),
    };
    h.hold.mode = mode;
    h.info.class = Some(bucket.to_owned());
    h.info.declared = u128::from(part(
        share,
        u64::try_from(total.info.declared).unwrap_or(u64::MAX),
    ));
    h
}

/// The holds a request of `class` takes beside `total`, the hold of the whole limit, in the order
/// the store reads them: in `strict` mode its bucket's counter, enforced; in `borrow` mode the
/// counters of the other classes with a share as headroom of `total`, then its own (when it has a
/// share), charged but never refusing. A limit that is not enforced keeps no headroom.
#[must_use]
pub fn class_holds(total: &PlannedHold, p: &PriorityShares, class: Option<&str>) -> Vec<PlannedHold> {
    let (own, share) = p.bucket(class);
    if !p.borrow {
        let mode = if total.hold.mode == HoldMode::Enforce {
            HoldMode::Enforce
        } else {
            HoldMode::Shadow
        };
        return vec![bucket_hold(total, own, share, mode)];
    }
    let mut out = Vec::new();
    if total.hold.mode == HoldMode::Enforce {
        out.extend(p.shares.iter().filter(|(b, _)| b.as_str() != own).map(|(b, s)| {
            let mut h = bucket_hold(total, b, *s, HoldMode::Headroom);
            h.hold.amount = 0;
            h
        }));
    }
    if own != REMAINDER {
        out.push(bucket_hold(total, own, share, HoldMode::Shadow));
    }
    out
}

/// `floor(share × n)`.
#[must_use]
pub fn part(share: Dec, n: u64) -> u64 {
    let v = u128::from(n).saturating_mul(share.0) / DEC_ONE;
    u64::try_from(v).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn shares(v: Value) -> BTreeMap<String, Value> {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn parsing_and_buckets() {
        let p = PriorityShares::parse(&shares(json!({"prod": 0.9, "dev": 0.05})), Some("borrow"))
            .unwrap()
            .unwrap();
        assert!(p.borrow);
        assert_eq!(p.bucket(Some("prod")).0, "prod");
        assert_eq!(p.bucket(Some("batch")).0, REMAINDER);
        assert_eq!(p.bucket(None), (REMAINDER, Dec::parse("0.05").unwrap()));
        assert_eq!(p.buckets().len(), 3);
        assert_eq!(part(p.bucket(Some("prod")).1, 1000), 900);
        assert_eq!(part(Dec::parse("0.1").unwrap(), 1000), 100);
        assert!(PriorityShares::parse(&shares(json!({})), None).unwrap().is_none());
    }

    #[test]
    fn invalid_shares() {
        let e =
            |v: Value, mode: Option<&str>| PriorityShares::parse(&shares(v), mode).unwrap_err().to_string();
        assert!(e(json!({"a": 0.7, "b": 0.4}), None).starts_with("priority_shares:"));
        assert!(e(json!({"a": 1.5}), None).starts_with("priority_shares.a:"));
        assert!(e(json!({"a": "0.5"}), None).starts_with("priority_shares.a:"));
        assert!(e(json!({}), Some("strict")).starts_with("priority_mode:"));
        assert!(e(json!({"a": 0.5}), Some("fair")).starts_with("priority_mode:"));
        let full = PriorityShares::parse(&shares(json!({"a": 0.5, "b": 0.5})), None)
            .unwrap()
            .unwrap();
        assert_eq!(
            full.buckets().len(),
            2,
            "no remainder when the shares add up to 1"
        );
    }
}
