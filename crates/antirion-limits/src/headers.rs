//! Header values: `RateLimit-Policy` / `RateLimit`, the OpenAI-compatible set, `Retry-After`,
//! the reservation, cost and budget headers. Partition values never appear in any of them.

use std::cmp::Ordering;

use antirion_core::UtcMicros;

use crate::cost::CostComponents;
use crate::counter::{HoldMode, Rule};
use crate::estimate::Estimate;
use crate::money::Nusd;
use crate::plan::{PlannedHold, Unit};
use crate::store::CounterView;

pub const RATELIMIT_POLICY: &str = "ratelimit-policy";
pub const RATELIMIT: &str = "ratelimit";
pub const RESERVATION: &str = "x-antirion-reservation";
pub const COST: &str = "x-antirion-cost";
pub const COST_DETAILS: &str = "x-antirion-cost-details";
pub const BUDGET_REMAINING: &str = "x-antirion-budget-remaining";
pub const BUDGET_RESET: &str = "x-antirion-budget-reset";

/// One rate limit as the headers show it, after this request's reservation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyState {
    /// `"<level>.<name>"`.
    pub name: String,
    pub quota: u128,
    /// The window in whole seconds (rounded up); absent for concurrency.
    pub window_s: Option<u64>,
    /// `tokens` or `concurrent-requests`; absent for requests, the default unit.
    pub quota_unit: Option<&'static str>,
    pub remaining: u128,
    /// Seconds until the counter is full again; absent for concurrency and a zero quota.
    pub reset_s: Option<u64>,
    /// Microseconds until full, for the OpenAI-style reset.
    pub reset_us: u64,
    pub unit: &'static str,
    /// For token limits, the side of the usage they count: `input`, `output` or `total`.
    pub side: Option<&'static str>,
}

/// The rate limits among `holds` (budgets excluded), most constrained first: smallest
/// `remaining / quota`, then smallest `remaining`.
#[must_use]
pub fn policy_states(holds: &[PlannedHold], views: &[CounterView]) -> Vec<PolicyState> {
    let mut out: Vec<PolicyState> = holds
        .iter()
        .zip(views)
        .filter(|(h, _)| !h.info.unit.is_budget() && h.hold.mode != HoldMode::Headroom)
        .map(|(h, v)| {
            let (quota, window_s) = match &h.hold.rule {
                Rule::Gcra(g) => (u128::from(g.limit), Some(g.per_us.div_ceil(1_000_000))),
                Rule::Window(w) => (u128::from(w.limit), Some(w.per_us.div_ceil(1_000_000))),
                Rule::Concurrency { limit } => (u128::from(*limit), None),
                Rule::Budget { amount, .. } => (*amount, None),
                Rule::Rolling(w) => (u128::from(w.limit), None),
            };
            let quota_unit = match h.info.unit {
                Unit::Tokens(_) => Some("tokens"),
                Unit::Concurrency => Some("concurrent-requests"),
                _ => None,
            };
            let side = match &h.info.unit {
                Unit::Tokens(w) => Some(w.side()),
                _ => None,
            };
            let reset_s = (window_s.is_some() && quota > 0).then(|| v.full_in_us.div_ceil(1_000_000));
            PolicyState {
                name: match &h.info.class {
                    Some(c) => format!("{}.{}.{c}", h.info.level.as_str(), h.info.name),
                    None => format!("{}.{}", h.info.level.as_str(), h.info.name),
                },
                quota,
                window_s,
                quota_unit,
                remaining: v.remaining.min(quota),
                reset_s,
                reset_us: v.full_in_us,
                unit: h.info.unit.name(),
                side,
            }
        })
        .collect();
    out.sort_by(constrained);
    out
}

fn constrained(a: &PolicyState, b: &PolicyState) -> Ordering {
    let lhs = a.remaining.saturating_mul(b.quota);
    let rhs = b.remaining.saturating_mul(a.quota);
    match (a.quota, b.quota) {
        (0, 0) => Ordering::Equal,
        (0, _) => Ordering::Less,
        (_, 0) => Ordering::Greater,
        _ => lhs.cmp(&rhs),
    }
    .then(a.remaining.cmp(&b.remaining))
}

fn sf_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// `RateLimit-Policy` and `RateLimit` values for up to `max` policies; `None` when no rate limit
/// applies.
#[must_use]
pub fn ietf(states: &[PolicyState], max: usize) -> Option<(String, String)> {
    let shown: Vec<&PolicyState> = states.iter().take(max).collect();
    if shown.is_empty() {
        return None;
    }
    let policy = shown
        .iter()
        .map(|s| {
            let mut v = format!("{};q={}", sf_string(&s.name), s.quota);
            if let Some(w) = s.window_s {
                v.push_str(&format!(";w={w}"));
            }
            if let Some(qu) = s.quota_unit {
                v.push_str(&format!(";qu={}", sf_string(qu)));
            }
            v
        })
        .collect::<Vec<_>>()
        .join(", ");
    let current = shown
        .iter()
        .map(|s| {
            let mut v = format!("{};r={}", sf_string(&s.name), s.remaining);
            if let Some(t) = s.reset_s {
                v.push_str(&format!(";t={t}"));
            }
            v
        })
        .collect::<Vec<_>>()
        .join(", ");
    Some((policy, current))
}

/// A duration as OpenAI writes resets: `250ms`, `6s`, `1m30s`, `1h0m0s`.
#[must_use]
pub fn openai_duration(us: u64) -> String {
    if us < 1_000_000 {
        return format!("{}ms", us.div_ceil(1_000));
    }
    let ms = us.div_ceil(1_000);
    let (h, rest) = (ms / 3_600_000, ms % 3_600_000);
    let (m, rest) = (rest / 60_000, rest % 60_000);
    let secs = if rest % 1_000 == 0 {
        format!("{}s", rest / 1_000)
    } else {
        let frac = format!("{:03}", rest % 1_000);
        format!("{}.{}s", rest / 1_000, frac.trim_end_matches('0'))
    };
    match (h, m) {
        (0, 0) => secs,
        (0, m) => format!("{m}m{secs}"),
        (h, m) => format!("{h}h{m}m{secs}"),
    }
}

/// `x-ratelimit-{limit,remaining,reset}-{requests,tokens}` from the most constrained policy of
/// each unit.
#[must_use]
pub fn openai(states: &[PolicyState]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for unit in ["requests", "tokens"] {
        if let Some(s) = states.iter().find(|s| s.unit == unit) {
            out.push((format!("x-ratelimit-limit-{unit}"), s.quota.to_string()));
            out.push((format!("x-ratelimit-remaining-{unit}"), s.remaining.to_string()));
            out.push((format!("x-ratelimit-reset-{unit}"), openai_duration(s.reset_us)));
        }
    }
    out
}

/// `anthropic-ratelimit-{requests,tokens,input-tokens,output-tokens}-{limit,remaining,reset}`: each
/// from the most constrained policy of its unit and side; the reset is when that counter is full
/// again, as RFC 3339 UTC.
#[must_use]
pub fn anthropic(states: &[PolicyState], now: UtcMicros) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (name, unit, side) in [
        ("requests", "requests", None),
        ("tokens", "tokens", Some("total")),
        ("input-tokens", "tokens", Some("input")),
        ("output-tokens", "tokens", Some("output")),
    ] {
        if let Some(s) = states.iter().find(|s| s.unit == unit && s.side == side) {
            let full = now
                .0
                .saturating_add(i64::try_from(s.reset_us).unwrap_or(i64::MAX))
                .saturating_add(999_999);
            let reset = UtcMicros(full - full.rem_euclid(1_000_000));
            out.push((format!("anthropic-ratelimit-{name}-limit"), s.quota.to_string()));
            out.push((
                format!("anthropic-ratelimit-{name}-remaining"),
                s.remaining.to_string(),
            ));
            out.push((
                format!("anthropic-ratelimit-{name}-reset"),
                reset.to_rfc3339_seconds(),
            ));
        }
    }
    out
}

/// `Retry-After`: whole seconds, at least 1.
#[must_use]
pub fn retry_after(wait_us: u64) -> String {
    wait_us.div_ceil(1_000_000).max(1).to_string()
}

/// `x-antirion-reservation: in=<E_in·H>, out=<E_out·H>, h=<H>, usd="<ρ_usd>"`: the tokens held for
/// every attempt the route may keep outstanding at once.
#[must_use]
pub fn reservation(e: &Estimate, hedge: u32, usd: Option<Nusd>) -> String {
    let h = u64::from(hedge.max(1));
    let mut v = format!(
        "in={}, out={}, h={h}",
        e.input.saturating_mul(h),
        e.output.saturating_mul(h)
    );
    if let Some(u) = usd {
        v.push_str(&format!(", usd=\"{}\"", u.to_usd_string()));
    }
    v
}

/// `x-antirion-cost`: the billed cost as a canonical USD decimal.
#[must_use]
pub fn cost(billed: Nusd) -> String {
    billed.to_usd_string()
}

/// `x-antirion-cost-details`: a structured-field dictionary of the non-zero components as
/// sf-strings, summing exactly to `x-antirion-cost`.
#[must_use]
pub fn cost_details(c: &CostComponents) -> String {
    c.named()
        .iter()
        .filter(|(_, n)| n.0 != 0)
        .map(|(k, n)| format!("{k}={}", sf_string(&n.to_usd_string())))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `x-antirion-budget-remaining` and `x-antirion-budget-reset` for the most constrained hard
/// budget; the reset is absent for a lifetime budget.
#[must_use]
pub fn budget(holds: &[PlannedHold], views: &[CounterView]) -> Option<(String, Option<String>)> {
    let (h, v) = holds
        .iter()
        .zip(views)
        .filter(|(h, _)| h.info.unit.is_budget() && h.hold.mode == crate::counter::HoldMode::Enforce)
        .min_by(|(ha, va), (hb, vb)| {
            let qa = match &ha.hold.rule {
                Rule::Budget { amount, .. } => *amount,
                _ => 0,
            };
            let qb = match &hb.hold.rule {
                Rule::Budget { amount, .. } => *amount,
                _ => 0,
            };
            va.remaining
                .saturating_mul(qb)
                .cmp(&vb.remaining.saturating_mul(qa))
        })?;
    let remaining = match h.info.unit {
        Unit::Usd => Nusd::from_units(v.remaining).to_usd_string(),
        _ => v.remaining.to_string(),
    };
    let reset = match &h.hold.rule {
        Rule::Budget { period, .. } => period.end.map(|e: UtcMicros| e.to_rfc3339_seconds()),
        _ => None,
    };
    Some((remaining, reset))
}

#[cfg(test)]
mod tests {
    use antirion_core::ResourceUid;

    use super::*;
    use crate::counter::{CounterId, Hold, HoldMode};
    use crate::decl::OnUnavailable;
    use crate::estimate::OutSource;
    use crate::level::Level;
    use crate::math::Gcra;
    use crate::period::PeriodBounds;
    use crate::plan::HoldInfo;

    fn h(name: &str, rule: Rule, unit: Unit) -> PlannedHold {
        PlannedHold {
            hold: Hold {
                counter: CounterId::derive("t", Level::Key, ResourceUid(1), "", name, "", &[]),
                group: 0,
                rule,
                amount: 1,
                mode: HoldMode::Enforce,
            },
            info: HoldInfo {
                level: Level::Key,
                owner: "k".into(),
                name: name.into(),
                unit,
                on_unavailable: OnUnavailable::LocalShare,
                per_us: 60_000_000,
                alert_at: vec![],
                partition: vec![("end_user".into(), "secret".into())],
                identity: CounterId([0; 16]),
                declared: 0,
                bypass_on_hit: false,

                class: None,
            },
        }
    }

    fn view(remaining: u128, full_in_us: u64) -> CounterView {
        CounterView {
            remaining,
            full_in_us,
            ..CounterView::default()
        }
    }

    #[test]
    fn ietf_headers() {
        let rpm = h(
            "rpm",
            Rule::Gcra(Gcra {
                limit: 10,
                per_us: 60_000_000,
                burst: 10,
            }),
            Unit::Requests {
                successful_only: false,
            },
        );
        let states = policy_states(&[rpm], &[view(9, 6_000_000)]);
        let (p, r) = ietf(&states, 10).unwrap();
        assert_eq!(p, "\"key.rpm\";q=10;w=60");
        assert_eq!(r, "\"key.rpm\";r=9;t=6");
        assert!(!p.contains("secret") && !r.contains("secret"));
        assert!(ietf(&[], 10).is_none());
    }

    #[test]
    fn ordering_units_and_openai() {
        let rpm = h(
            "rpm",
            Rule::Gcra(Gcra {
                limit: 5,
                per_us: 60_000_000,
                burst: 5,
            }),
            Unit::Requests {
                successful_only: false,
            },
        );
        let tpm = h(
            "tpm",
            Rule::Gcra(Gcra {
                limit: 100_000,
                per_us: 60_000_000,
                burst: 100_000,
            }),
            Unit::Tokens(Default::default()),
        );
        let conc = h("c", Rule::Concurrency { limit: 2 }, Unit::Concurrency);
        let states = policy_states(
            &[rpm, tpm, conc],
            &[view(4, 12_000_000), view(99_000, 600_000), view(0, 0)],
        );
        assert_eq!(states[0].name, "key.c");
        let (p, r) = ietf(&states, 2).unwrap();
        assert_eq!(
            p,
            "\"key.c\";q=2;qu=\"concurrent-requests\", \"key.rpm\";q=5;w=60"
        );
        assert_eq!(r, "\"key.c\";r=0, \"key.rpm\";r=4;t=12");
        let o = openai(&states);
        assert!(o.contains(&("x-ratelimit-limit-requests".into(), "5".into())));
        assert!(o.contains(&("x-ratelimit-remaining-requests".into(), "4".into())));
        assert!(o.contains(&("x-ratelimit-reset-tokens".into(), "600ms".into())));
        assert!(o.contains(&("x-ratelimit-reset-requests".into(), "12s".into())));
    }

    #[test]
    fn durations_and_values() {
        assert_eq!(openai_duration(250_000), "250ms");
        assert_eq!(openai_duration(90_000_000), "1m30s");
        assert_eq!(openai_duration(3_600_000_000), "1h0m0s");
        assert_eq!(openai_duration(1_500_000), "1.5s");
        assert_eq!(retry_after(0), "1");
        assert_eq!(retry_after(6_000_001), "7");
        let e = Estimate {
            input: 12,
            output: 300,
            out_source: OutSource::Request,
            clamp_to: None,
        };
        assert_eq!(reservation(&e, 1, None), "in=12, out=300, h=1");
        assert_eq!(
            reservation(&e, 2, Some(Nusd(1_000_000_000))),
            "in=24, out=600, h=2, usd=\"1\""
        );
        let c = CostComponents {
            input: Nusd(1_000_000),
            output: Nusd(1_000_000),
            markup: Nusd(-500_000),
            ..CostComponents::default()
        };
        assert_eq!(
            cost_details(&c),
            "input=\"0.001\", output=\"0.001\", markup=\"-0.0005\""
        );
        assert_eq!(cost(Nusd(0)), "0");
    }

    #[test]
    fn budget_headers() {
        let monthly = h(
            "m",
            Rule::Budget {
                amount: 10_000_000_000,
                period: PeriodBounds {
                    start: UtcMicros(0),
                    end: Some(UtcMicros(2_592_000_000_000)),
                },
            },
            Unit::Usd,
        );
        let (rem, reset) = budget(&[monthly], &[view(9_500_000_000, 0)]).unwrap();
        assert_eq!(rem, "9.5");
        assert_eq!(reset.as_deref(), Some("1970-01-31T00:00:00Z"));
    }
}
