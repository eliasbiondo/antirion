//! Planning an admission: the applicable counters of a request and the reservation `ρ` on each.

use antirion_core::{Stage, UtcMicros};
use antirion_error::{GatewayError, Message};

use crate::applicable::{Applicable, RequestAttrs, Scope, applicable};
use crate::cost::{FixedUnits, worst_case};
use crate::counter::{CounterId, Hold, HoldMode, Rule};
use crate::decl::{Algorithm, BudgetUnit, LimitDecl, LimitUnit, OnUnavailable};
use crate::estimate::Estimate;
use crate::level::Level;
use crate::math::{Gcra, Window};
use crate::money::{Dec, Nusd};
use crate::policy::{DeclKind, Policies};
use crate::price::Price;
use crate::reasons;
use crate::settings::{HeaderSettings, OnUnpriced};
use crate::store::ReservationId;
use crate::weights::{Incurred, Weights};

/// How a hold's actual amount is computed at settlement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unit {
    Requests {
        successful_only: bool,
    },
    Tokens(Weights),
    Concurrency,
    /// A budget in billed nUSD.
    Usd,
    /// A budget in weighted tokens.
    BudgetTokens(Weights),
}

impl Unit {
    /// The unit as `antirion.limit.unit` names it.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Unit::Requests { .. } => "requests",
            Unit::Tokens(_) | Unit::BudgetTokens(_) => "tokens",
            Unit::Concurrency => "concurrency",
            Unit::Usd => "usd",
        }
    }

    #[must_use]
    pub const fn is_budget(&self) -> bool {
        matches!(self, Unit::Usd | Unit::BudgetTokens(_))
    }
}

/// What the engine keeps about a hold beyond the store's view of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HoldInfo {
    pub level: Level,
    pub owner: String,
    pub name: String,
    pub unit: Unit,
    pub on_unavailable: OnUnavailable,
    /// The declared window in microseconds (rate limits), for headers.
    pub per_us: u64,
    /// Budget alert ratios.
    pub alert_at: Vec<Dec>,
    /// Partition values, for alert payloads only.
    pub partition: Vec<(String, String)>,
    /// The counter without its window, which ledger lines attribute budget spend to.
    pub identity: CounterId,
    /// The declared quota of this partition. A budget's rule may hold more while overage grants
    /// run; alert ratios are of this amount.
    pub declared: u128,
    /// A budget whose refusal waits for the cache lookup (`cache_hits_bypass_budgets`).
    pub bypass_on_hit: bool,
    /// The priority class (or the remainder) of a class counter of a limit with priority shares.
    pub class: Option<String>,
}

/// A hold with the declaration it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedHold {
    pub hold: Hold,
    pub info: HoldInfo,
}

/// The holds of one admission, in canonical order, ready for the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmitPlan {
    pub uid: ReservationId,
    pub group: u128,
    pub holds: Vec<PlannedHold>,
    pub estimate: Estimate,
    pub hedge: u32,
    /// `ρ_usd` when a USD budget applies.
    pub usd: Option<Nusd>,
    pub admitted_at: UtcMicros,
    pub headers: HeaderSettings,
}

impl AdmitPlan {
    /// The holds as the store takes them.
    #[must_use]
    pub fn store_holds(&self) -> Vec<Hold> {
        self.holds.iter().map(|h| h.hold.clone()).collect()
    }
}

/// Everything admission needs to know about a request.
#[derive(Clone, Debug)]
pub struct AdmitInput<'a> {
    pub uid: ReservationId,
    pub attrs: RequestAttrs<'a>,
    pub estimate: Estimate,
    /// `H = 1 + h`, the attempts the route may keep outstanding at once.
    pub hedge: u32,
    pub incurs: Incurred,
    /// The effective price of every deployment the route can reach; `None` when unpriced.
    pub candidates: &'a [Option<&'a Price>],
    pub fixed: FixedUnits,
    pub multiplier: Dec,
    pub now: UtcMicros,
}

fn tokens_rho(w: &Weights, input: &AdmitInput<'_>) -> u128 {
    w.reservation(
        &input.incurs,
        input.estimate.input,
        input.estimate.output,
        input.hedge.max(1),
    )
    .unwrap_or(u128::MAX)
}

fn usd_rho(policies: &Policies, input: &AdmitInput<'_>) -> Result<u128, GatewayError> {
    let mut worst: u128 = 0;
    for price in input.candidates {
        let one = match price {
            Some(p) => worst_case(
                p,
                input.estimate.input,
                input.estimate.output,
                &input.incurs,
                &input.fixed,
                input.multiplier,
            )
            .unwrap_or(u128::MAX),
            None if policies.pricing.on_unpriced == OnUnpriced::Zero => 0,
            None => {
                return Err(GatewayError::with_reason(
                    reasons::MODEL_UNPRICED,
                    Stage::Admit,
                    Message::Fixed("A budget applies, and a deployment this request may use has no price."),
                ));
            }
        };
        worst = worst.max(one);
    }
    Ok(worst.saturating_mul(u128::from(input.hedge.max(1))))
}

/// How a declaration's counter counts: its rule, the unit settlement charges in, its mode and
/// what it does without the store.
struct Shape {
    rule: Rule,
    unit: Unit,
    mode: HoldMode,
    on_unavailable: OnUnavailable,
    per_us: u64,
    alert_at: Vec<Dec>,
}

fn shape(a: &Applicable<'_>, now: UtcMicros) -> Shape {
    match &a.decl.kind {
        DeclKind::Limit(l) => {
            let limit = u64::try_from(a.quota).unwrap_or(u64::MAX);
            let burst = match limit {
                0 => 0,
                q if q == l.limit => l.burst,
                q => q,
            };
            let unit = match &l.unit {
                LimitUnit::Concurrency => Unit::Concurrency,
                LimitUnit::Tokens(w) => Unit::Tokens(w.clone()),
                LimitUnit::Requests { successful_only } => Unit::Requests {
                    successful_only: *successful_only,
                },
            };
            let rule = match (&l.unit, l.algorithm) {
                (LimitUnit::Concurrency, _) => Rule::Concurrency { limit },
                (_, Algorithm::Gcra) => Rule::Gcra(Gcra {
                    limit,
                    per_us: l.per_us,
                    burst,
                }),
                (_, Algorithm::SlidingWindow { precision }) => Rule::Window(Window {
                    limit,
                    per_us: l.per_us,
                    precision,
                }),
            };
            Shape {
                rule,
                unit,
                mode: if l.enforce {
                    HoldMode::Enforce
                } else {
                    HoldMode::Shadow
                },
                on_unavailable: l.on_unavailable,
                per_us: l.per_us,
                alert_at: Vec::new(),
            }
        }
        DeclKind::Budget(b) => Shape {
            rule: b.rule(a.quota, now),
            unit: match &b.unit {
                BudgetUnit::Usd => Unit::Usd,
                BudgetUnit::Tokens(w) => Unit::BudgetTokens(w.clone()),
            },
            mode: if b.hard {
                HoldMode::Enforce
            } else {
                HoldMode::Track
            },
            on_unavailable: b.on_unavailable,
            per_us: 0,
            alert_at: b.alert_at.clone(),
        },
    }
}

/// A hold of `amount` on an applicable declaration's counter.
fn planned(a: &Applicable<'_>, amount: u128, now: UtcMicros) -> PlannedHold {
    let s = shape(a, now);
    PlannedHold {
        hold: Hold {
            counter: a.counter,
            group: a.group,
            rule: s.rule,
            amount,
            mode: s.mode,
        },
        info: HoldInfo {
            level: a.decl.level,
            owner: a.decl.owner.clone(),
            name: a.decl.name.clone(),
            unit: s.unit,
            on_unavailable: s.on_unavailable,
            per_us: s.per_us,
            alert_at: s.alert_at,
            partition: a.partition.clone(),
            identity: a.identity,
            declared: a.quota,
            bypass_on_hit: matches!(&a.decl.kind, DeclKind::Budget(b) if b.bypass_on_hit),

            class: None,
        },
    }
}

/// A hold that reserves nothing, to read a counter's state.
#[must_use]
pub fn reading(a: &Applicable<'_>, now: UtcMicros) -> PlannedHold {
    planned(a, 0, now)
}

/// A held amount and its counting rule for one applicable declaration; `None` when `ρ` is zero.
fn hold_of(
    a: &Applicable<'_>,
    input: &AdmitInput<'_>,
    usd: &mut Option<u128>,
    policies: &Policies,
) -> Result<Option<PlannedHold>, GatewayError> {
    let amount = match &a.decl.kind {
        DeclKind::Limit(l) => match &l.unit {
            LimitUnit::Concurrency | LimitUnit::Requests { .. } => 1,
            LimitUnit::Tokens(w) => tokens_rho(w, input),
        },
        DeclKind::Budget(b) => match &b.unit {
            BudgetUnit::Usd => match usd {
                Some(v) => *v,
                None => {
                    let v = usd_rho(policies, input)?;
                    *usd = Some(v);
                    v
                }
            },
            BudgetUnit::Tokens(w) => tokens_rho(w, input),
        },
    };
    if amount == 0 {
        return Ok(None);
    }
    Ok(Some(planned(a, amount, input.now)))
}

fn missing(e: &crate::applicable::MissingAttribute) -> GatewayError {
    GatewayError::with_reason(
        reasons::MISSING_PARTITION_ATTRIBUTE,
        Stage::Admit,
        Message::built(format!(
            "The request has no {}, which a limit partitions on.",
            e.attribute
        )),
    )
    .param(e.attribute.clone())
}

/// The holds of a request's applicable set in a scope. Counters with `ρ = 0` are left out: they
/// can neither refuse nor be charged.
pub fn plan_holds(
    policies: &Policies,
    input: &AdmitInput<'_>,
    scope: Scope<'_>,
) -> Result<(Vec<PlannedHold>, Option<Nusd>), GatewayError> {
    let set = applicable(policies, &input.attrs, scope).map_err(|e| missing(&e))?;
    let mut usd = None;
    let mut groups: Vec<Vec<PlannedHold>> = Vec::with_capacity(set.len());
    for a in &set {
        if let Some(h) = hold_of(a, input, &mut usd, policies)? {
            let classes = match &a.decl.kind {
                DeclKind::Limit(LimitDecl {
                    priority: Some(p), ..
                }) => crate::priority::class_holds(&h, p, input.attrs.priority_class),
                _ => Vec::new(),
            };
            let mut group = vec![h];
            group.extend(classes);
            groups.push(group);
        }
    }
    let lead = |g: &Vec<PlannedHold>| g.first().map(|h| h.hold.counter);
    groups.sort_by_key(lead);
    groups.dedup_by(|a, b| lead(a) == lead(b));
    Ok((groups.into_iter().flatten().collect(), usd.map(Nusd::from_units)))
}

/// Plans the admission of a request (stage `admit`): gateway through model levels.
pub fn plan(policies: &Policies, input: &AdmitInput<'_>) -> Result<AdmitPlan, GatewayError> {
    let (holds, usd) = plan_holds(policies, input, Scope::Request)?;
    Ok(AdmitPlan {
        uid: input.uid,
        group: policies.organization_uid(input.attrs.organization),
        holds,
        estimate: input.estimate,
        hedge: input.hedge.max(1),
        usd,
        admitted_at: input.now,
        headers: policies.limits.headers.clone(),
    })
}

#[cfg(test)]
mod tests {
    use antirion_core::ResourceUid;
    use antirion_error::Code;
    use serde_json::json;

    use super::*;
    use crate::estimate::OutSource;

    fn policies(doc: serde_json::Value) -> Policies {
        Policies::compile(&doc, &|_, n| ResourceUid(n.len() as u128 + 1)).unwrap()
    }

    fn input<'a>(candidates: &'a [Option<&'a Price>], out: u64) -> AdmitInput<'a> {
        AdmitInput {
            uid: ReservationId(1),
            attrs: RequestAttrs {
                organization: "default",
                team: "default",
                project: "default",
                key: "k",
                model: Some("m"),
                endpoint: "chat_completions",
                ..RequestAttrs::default()
            },
            estimate: Estimate {
                input: 50,
                output: out,
                out_source: OutSource::Request,
                clamp_to: None,
            },
            hedge: 1,
            incurs: Incurred::default(),
            candidates,
            fixed: FixedUnits::default(),
            multiplier: Dec::ONE,
            now: UtcMicros(1_790_000_000_000_000),
        }
    }

    #[test]
    fn reservations_per_unit() {
        let p = policies(json!({"keys": {"k": {"policies": {
            "limits": {"rpm": {"unit": "requests", "limit": 10, "per": "1h"},
                       "tpd": {"unit": "tokens", "tokens": "output", "limit": 1000, "per": "1d"},
                       "inflight": {"unit": "concurrency", "limit": 2}},
            "budgets": {"usd": {"unit": "usd", "amount": "3.50"}}
        }}}}));
        let price = Price::parse(&json!({"input": "0", "output": "10000.00"})).unwrap();
        let cands = [Some(&price)];
        let plan = plan(&p, &input(&cands, 100)).unwrap();
        let by_name = |n: &str| plan.holds.iter().find(|h| h.info.name == n).unwrap();
        assert_eq!(by_name("rpm").hold.amount, 1);
        assert_eq!(by_name("tpd").hold.amount, 100);
        assert_eq!(by_name("inflight").hold.amount, 1);
        assert_eq!(by_name("usd").hold.amount, 1_000_000_000);
        assert_eq!(plan.usd, Some(Nusd(1_000_000_000)));
        assert!(
            plan.holds
                .windows(2)
                .all(|w| w[0].hold.counter < w[1].hold.counter)
        );
    }

    #[test]
    fn priority_classes_follow_their_limit() {
        let limit = |mode: &str| {
            json!({"keys": {"k": {"policies": {"limits": {
                "tpd": {"unit": "tokens", "tokens": "output", "limit": 1000, "per": "1d",
                        "priority_shares": {"prod": 0.5, "dev": 0.2}, "priority_mode": mode},
                "rpm": {"unit": "requests", "limit": 10, "per": "1h"}}}}}})
        };
        let price = Price::parse(&json!({"input": "0", "output": "1"})).unwrap();
        let cands = [Some(&price)];
        let modes = |doc, class| {
            let p = policies(doc);
            let mut i = input(&cands, 100);
            i.attrs.priority_class = class;
            let plan = plan(&p, &i).unwrap();
            let tpd: Vec<_> = plan
                .holds
                .iter()
                .filter(|h| h.info.name == "tpd")
                .cloned()
                .collect();
            assert!(
                plan.holds
                    .windows(2)
                    .all(|w| w[0].info.name == w[1].info.name || w[1].info.class.is_none()),
                "a limit's class holds follow it"
            );
            tpd.iter()
                .map(|h| (h.info.class.clone(), h.hold.mode, h.hold.amount))
                .collect::<Vec<_>>()
        };
        let dev = || Some("dev".to_owned());
        assert_eq!(
            modes(limit("strict"), Some("dev")),
            vec![(None, HoldMode::Enforce, 100), (dev(), HoldMode::Enforce, 100)]
        );
        assert_eq!(
            modes(limit("strict"), Some("batch"))[1].0.as_deref(),
            Some(crate::priority::REMAINDER)
        );
        assert_eq!(
            modes(limit("borrow"), Some("dev")),
            vec![
                (None, HoldMode::Enforce, 100),
                (Some("prod".to_owned()), HoldMode::Headroom, 0),
                (dev(), HoldMode::Shadow, 100)
            ]
        );
        assert_eq!(
            modes(limit("borrow"), None).len(),
            3,
            "no share: both classes are headroom"
        );
    }

    #[test]
    fn zero_reservations_are_not_consulted() {
        let p = policies(
            json!({"keys": {"k": {"policies": {"budgets": {"usd": {"unit": "usd", "amount": "0"}}}}}}),
        );
        let free = Price::parse(&json!({"input": "0", "output": "0"})).unwrap();
        let cands = [Some(&free)];
        assert!(plan(&p, &input(&cands, 100)).unwrap().holds.is_empty());
    }

    #[test]
    fn unpriced_candidates_with_a_budget() {
        let p = policies(
            json!({"keys": {"k": {"policies": {"budgets": {"usd": {"unit": "usd", "amount": "1"}}}}}}),
        );
        let cands = [None];
        let e = plan(&p, &input(&cands, 100)).unwrap_err();
        assert_eq!(e.code, Code::Forbidden);
        assert_eq!(e.reason.unwrap().as_str(), "model_unpriced");
        let zero = policies(
            json!({"pricing": {"on_unpriced": "zero"}, "keys": {"k": {"policies": {"budgets": {"usd": {"unit": "usd", "amount": "1"}}}}}}),
        );
        assert!(plan(&zero, &input(&cands, 100)).unwrap().holds.is_empty());
        let none = policies(json!({}));
        assert!(plan(&none, &input(&cands, 100)).is_ok());
    }

    #[test]
    fn hedging_overrides_and_missing_attributes() {
        let p = policies(json!({"keys": {"k": {"policies": {"limits": {
            "tpd": {"unit": "tokens", "tokens": "output", "limit": 1000, "per": "1d"},
            "eu": {"unit": "requests", "limit": 2, "per": "1h", "partition_by": ["end_user"], "on_missing": "reject"}
        }}}}}));
        let mut i = input(&[], 300);
        i.hedge = 2;
        i.attrs.end_user = Some("alice");
        let plan = plan(&p, &i).unwrap();
        assert_eq!(
            plan.holds
                .iter()
                .find(|h| h.info.name == "tpd")
                .unwrap()
                .hold
                .amount,
            600
        );
        i.attrs.end_user = None;
        let e = super::plan(&p, &i).unwrap_err();
        assert_eq!(e.code, Code::InvalidRequest);
        assert_eq!(e.param.as_deref(), Some("end_user"));
    }
}
