//! Which refusal a request reports, and its error: status, `antirion.limit`, `Retry-After`.

use std::time::Duration;

use antirion_core::{Stage, UtcMicros};
use antirion_error::{Code, GatewayError, Message};
use serde_json::{Map, Value};

use crate::counter::Rule;
use crate::level::Level;
use crate::money::Nusd;
use crate::plan::{PlannedHold, Unit};
use crate::reasons;
use crate::store::{CounterView, RefusalKind, StoreRefusal};

/// An amount in a hold's unit, as `antirion.limit` shows it: USD decimals for USD budgets.
fn amount_text(unit: &Unit, v: u128) -> String {
    match unit {
        Unit::Usd => Nusd::from_units(v).to_usd_string(),
        _ => v.to_string(),
    }
}

/// An amount as a message shows it: USD with at least two decimals (`1.00`, `0.125`).
fn amount_words(unit: &Unit, v: u128) -> String {
    let text = amount_text(unit, v);
    if *unit != Unit::Usd {
        return text;
    }
    match text.split_once('.') {
        None => format!("{text}.00"),
        Some((_, frac)) if frac.len() < 2 => format!("{text}0"),
        Some(_) => text,
    }
}

fn quota(h: &PlannedHold) -> u128 {
    match &h.hold.rule {
        Rule::Gcra(g) => u128::from(g.limit),
        Rule::Window(w) => u128::from(w.limit),
        Rule::Concurrency { limit } => u128::from(*limit),
        Rule::Budget { amount, .. } => *amount,
        Rule::Rolling(w) => u128::from(w.limit),
    }
}

fn kind_name(h: &PlannedHold) -> &'static str {
    match h.info.unit {
        Unit::Concurrency => "concurrency",
        Unit::Usd | Unit::BudgetTokens(_) => "budget",
        _ => "rate_limit",
    }
}

fn whole_seconds(us: u64) -> u64 {
    us.div_ceil(1_000_000).max(1)
}

fn tie_key(h: &PlannedHold) -> (Level, &str) {
    (h.info.level, h.info.name.as_str())
}

/// The `antirion.limit` member for a refusing hold.
fn limit_json(h: &PlannedHold, view: Option<&CounterView>, reset: Option<(UtcMicros, u64)>) -> Value {
    let mut m = Map::new();
    m.insert("kind".into(), Value::from(kind_name(h)));
    m.insert("level".into(), Value::from(h.info.level.as_str()));
    m.insert("name".into(), Value::from(h.info.name.clone()));
    m.insert("unit".into(), Value::from(h.info.unit.name()));
    m.insert("limit".into(), Value::from(amount_text(&h.info.unit, quota(h))));
    let remaining = view.map_or(0, |v| v.remaining);
    m.insert(
        "remaining".into(),
        Value::from(amount_text(&h.info.unit, remaining)),
    );
    m.insert(
        "requested".into(),
        Value::from(amount_text(&h.info.unit, h.hold.amount)),
    );
    if let Some((at, secs)) = reset {
        m.insert("reset_at".into(), Value::from(at.to_rfc3339_seconds()));
        m.insert("retry_after_s".into(), Value::from(secs));
    }
    Value::Object(m)
}

/// The error for an admission that some counters refused: `budget_exceeded` (402) over
/// `request_exceeds_limit` (429) over `rate_limited` (429). Among rate refusals the largest wait is
/// reported; ties go to the broadest level, then the declaration name.
#[must_use]
pub fn refusal_error(
    holds: &[PlannedHold],
    refusals: &[StoreRefusal],
    views: &[CounterView],
    now: UtcMicros,
) -> GatewayError {
    let with = |kind: RefusalKind| -> Vec<(&PlannedHold, &StoreRefusal, Option<&CounterView>)> {
        refusals
            .iter()
            .filter(|r| r.kind == kind)
            .filter_map(|r| holds.get(r.index).map(|h| (h, r, views.get(r.index))))
            .collect()
    };
    let budgets = with(RefusalKind::Budget);
    if !budgets.is_empty() {
        let end_of = |h: &PlannedHold, r: &StoreRefusal| match &h.hold.rule {
            Rule::Budget { period, .. } => period.end,
            Rule::Rolling(_) => Some(UtcMicros(
                now.0
                    .saturating_add(i64::try_from(r.wait_us.unwrap_or(0)).unwrap_or(i64::MAX)),
            )),
            _ => None,
        };
        let chosen = budgets
            .iter()
            .filter(|(h, r, _)| end_of(h, r).is_none())
            .min_by(|a, b| tie_key(a.0).cmp(&tie_key(b.0)))
            .or_else(|| {
                budgets.iter().max_by(|a, b| {
                    end_of(a.0, a.1)
                        .cmp(&end_of(b.0, b.1))
                        .then_with(|| tie_key(b.0).cmp(&tie_key(a.0)))
                })
            });
        let Some((h, r, view)) = chosen else {
            return GatewayError::new(
                Code::BudgetExceeded,
                Stage::Admit,
                Message::Fixed("Budget exceeded."),
            );
        };
        let reset = end_of(h, r).map(|end| {
            let secs = whole_seconds(u64::try_from(end.0.saturating_sub(now.0)).unwrap_or(0));
            (end, secs)
        });
        let remaining = view.map_or(0, |v| v.remaining);
        let unit = if h.info.unit == Unit::Usd {
            " USD"
        } else {
            " tokens"
        };
        let message = format!(
            "The request reserves {}{unit}, more than the {}{unit} remaining in budget {}.{}; lower max_tokens or the request's size.",
            amount_words(&h.info.unit, h.hold.amount),
            amount_words(&h.info.unit, remaining),
            h.info.level.as_str(),
            h.info.name,
        );
        let mut e = GatewayError::new(Code::BudgetExceeded, Stage::Admit, Message::built(message));
        e.limit = Some(Box::new(limit_json(h, *view, reset)));
        e.retry_after = reset.map(|(_, secs)| Duration::from_secs(secs));
        return e;
    }
    let exceeds = with(RefusalKind::Exceeds);
    if let Some((h, _, view)) = exceeds.iter().min_by(|a, b| tie_key(a.0).cmp(&tie_key(b.0))) {
        let capacity = match &h.hold.rule {
            Rule::Gcra(g) => u128::from(g.burst),
            Rule::Window(w) | Rule::Rolling(w) => u128::from(w.limit),
            Rule::Concurrency { limit } => u128::from(*limit),
            Rule::Budget { amount, .. } => *amount,
        };
        let message = format!(
            "The request reserves {} {} but limit {}.{} admits at most {}; lower max_tokens or n.",
            h.hold.amount,
            h.info.unit.name(),
            h.info.level.as_str(),
            h.info.name,
            capacity,
        );
        let mut e = GatewayError::new(Code::RequestExceedsLimit, Stage::Admit, Message::built(message));
        e.limit = Some(Box::new(limit_json(h, *view, None)));
        return e;
    }
    let rate = with(RefusalKind::Rate);
    let chosen = rate.iter().max_by(|a, b| {
        a.1.wait_us
            .cmp(&b.1.wait_us)
            .then_with(|| tie_key(b.0).cmp(&tie_key(a.0)))
    });
    let Some((h, r, view)) = chosen else {
        return GatewayError::new(
            Code::RateLimited,
            Stage::Admit,
            Message::Fixed("Rate limit exceeded."),
        );
    };
    let wait_us = r.wait_us.unwrap_or(1_000_000);
    let secs = whole_seconds(wait_us);
    let reset_at = now.saturating_add(Duration::from_micros(wait_us));
    let message = if h.info.unit == Unit::Concurrency {
        format!(
            "Concurrency limit {}.{} is full.",
            h.info.level.as_str(),
            h.info.name
        )
    } else {
        format!("Rate limit {}.{} exceeded.", h.info.level.as_str(), h.info.name)
    };
    let mut e = GatewayError::new(Code::RateLimited, Stage::Admit, Message::built(message));
    e.limit = Some(Box::new(limit_json(h, *view, Some((reset_at, secs)))));
    e.retry_after = Some(Duration::from_secs(secs));
    e
}

/// 503 when the counter store is unavailable and a counter fails closed.
#[must_use]
pub fn unavailable_error() -> GatewayError {
    let mut e = GatewayError::with_reason(
        reasons::LIMITS_UNAVAILABLE,
        Stage::Admit,
        Message::Fixed("Limits cannot be checked right now."),
    );
    e.retry_after = Some(Duration::from_secs(1));
    e
}

/// 429 when every candidate deployment is ineligible because of its own limits.
#[must_use]
pub fn deployment_limits_error(smallest_wait: Duration) -> GatewayError {
    let mut e = GatewayError::with_reason(
        reasons::DEPLOYMENT_LIMITS,
        Stage::Route,
        Message::Fixed("Every deployment for this request is at its rate limit."),
    );
    e.retry_after = Some(Duration::from_secs(whole_seconds(
        u64::try_from(smallest_wait.as_micros()).unwrap_or(u64::MAX),
    )));
    e
}

/// 400 for an explicit output cap or choice count out of range.
#[must_use]
pub fn output_cap_error(field: &str) -> GatewayError {
    GatewayError::with_reason(
        reasons::OUTPUT_CAP_OUT_OF_RANGE,
        Stage::Admit,
        Message::built(format!("{field} is out of range.")),
    )
    .param(field.to_owned())
}

#[cfg(test)]
mod tests {
    use antirion_core::ResourceUid;

    use super::*;
    use crate::counter::{CounterId, Hold, HoldMode};
    use crate::decl::OnUnavailable;
    use crate::math::Gcra;
    use crate::period::PeriodBounds;
    use crate::plan::HoldInfo;

    fn hold(level: Level, name: &str, rule: Rule, unit: Unit, amount: u128) -> PlannedHold {
        PlannedHold {
            hold: Hold {
                counter: CounterId::derive("t", level, ResourceUid(1), "", name, "", &[]),
                group: 0,
                rule,
                amount,
                mode: HoldMode::Enforce,
            },
            info: HoldInfo {
                level,
                owner: "o".into(),
                name: name.into(),
                unit,
                on_unavailable: OnUnavailable::LocalShare,
                per_us: 3_600_000_000,
                alert_at: Vec::new(),
                partition: Vec::new(),
                identity: CounterId([0; 16]),
                declared: 0,
                bypass_on_hit: false,

                class: None,
            },
        }
    }

    fn rpm(level: Level, name: &str) -> PlannedHold {
        hold(
            level,
            name,
            Rule::Gcra(Gcra {
                limit: 1,
                per_us: 3_600_000_000,
                burst: 1,
            }),
            Unit::Requests {
                successful_only: false,
            },
            1,
        )
    }

    fn refusal(index: usize, kind: RefusalKind, wait_us: Option<u64>) -> StoreRefusal {
        StoreRefusal { index, kind, wait_us }
    }

    #[test]
    fn largest_wait_then_broadest_level() {
        let holds = [rpm(Level::Key, "k"), rpm(Level::Gateway, "g")];
        let now = UtcMicros(0);
        let e = refusal_error(
            &holds,
            &[
                refusal(0, RefusalKind::Rate, Some(5)),
                refusal(1, RefusalKind::Rate, Some(5)),
            ],
            &[],
            now,
        );
        assert_eq!(e.limit.as_ref().unwrap()["level"], "gateway");
        assert_eq!(e.retry_after, Some(Duration::from_secs(1)));
        let e = refusal_error(
            &holds,
            &[
                refusal(0, RefusalKind::Rate, Some(7_000_001)),
                refusal(1, RefusalKind::Rate, Some(5)),
            ],
            &[],
            now,
        );
        assert_eq!(e.limit.as_ref().unwrap()["level"], "key");
        assert_eq!(e.retry_after, Some(Duration::from_secs(8)));
        assert_eq!(e.limit.as_ref().unwrap()["retry_after_s"], 8);
    }

    #[test]
    fn precedence_and_budgets() {
        let lifetime = hold(
            Level::Key,
            "b",
            Rule::Budget {
                amount: 500_000_000,
                period: PeriodBounds {
                    start: UtcMicros(0),
                    end: None,
                },
            },
            Unit::Usd,
            1_000_000_000,
        );
        let daily = hold(
            Level::Team,
            "d",
            Rule::Budget {
                amount: 5,
                period: PeriodBounds {
                    start: UtcMicros(0),
                    end: Some(UtcMicros(86_400_000_000)),
                },
            },
            Unit::Usd,
            1,
        );
        let tpm = hold(
            Level::Key,
            "tpm",
            Rule::Gcra(Gcra {
                limit: 100,
                per_us: 60_000_000,
                burst: 100,
            }),
            Unit::Tokens(Default::default()),
            500,
        );
        let holds = [lifetime, daily.clone(), tpm.clone(), rpm(Level::Key, "rpm")];
        let views = [CounterView {
            remaining: 500_000_000,
            ..CounterView::default()
        }];
        let all = [
            refusal(0, RefusalKind::Budget, None),
            refusal(1, RefusalKind::Budget, None),
            refusal(2, RefusalKind::Exceeds, None),
            refusal(3, RefusalKind::Rate, Some(1)),
        ];
        let e = refusal_error(&holds, &all, &views, UtcMicros(0));
        assert_eq!(e.code, Code::BudgetExceeded);
        assert_eq!(e.retry_after, None);
        assert!(e.message.as_str().contains("1.00 USD") && e.message.as_str().contains("0.50 USD"));
        assert_eq!(e.limit.as_ref().unwrap()["remaining"], "0.5");
        let e = refusal_error(&holds, &all[1..], &[], UtcMicros(86_000_000_000));
        assert_eq!(e.retry_after, Some(Duration::from_secs(400)));
        assert_eq!(e.limit.as_ref().unwrap()["kind"], "budget");
        let e = refusal_error(&holds, &all[2..], &[], UtcMicros(0));
        assert_eq!(e.code, Code::RequestExceedsLimit);
        assert_eq!(e.retry_after, None);
        assert!(e.message.as_str().contains("500") && e.message.as_str().contains("100"));
    }

    #[test]
    fn helpers() {
        assert_eq!(unavailable_error().reason.unwrap().as_str(), "limits_unavailable");
        assert_eq!(
            deployment_limits_error(Duration::from_millis(10)).retry_after,
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            output_cap_error("max_tokens").param.as_deref(),
            Some("max_tokens")
        );
    }
}
