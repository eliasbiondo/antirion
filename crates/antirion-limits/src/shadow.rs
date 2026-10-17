//! Shadow (mirrored) calls: never reserved and never refused, they are charged after the fact to
//! the gateway- and project-level budgets that apply to the request they copy.

use antirion_core::UtcMicros;

use crate::applicable::{MissingAttribute, RequestAttrs, Scope, applicable};
use crate::counter::{Hold, HoldMode, Rule};
use crate::decl::BudgetUnit;
use crate::level::Level;
use crate::money::Nusd;
use crate::plan::{HoldInfo, PlannedHold, Unit};
use crate::policy::{DeclKind, Policies};
use crate::usage::Usage;

/// The budget counters a shadow call is charged to, each holding the amount it adds: `billed` for
/// USD budgets, the call's weighted tokens for token budgets. Zero amounts are left out.
pub fn charges(
    policies: &Policies,
    attrs: &RequestAttrs<'_>,
    now: UtcMicros,
    usage: &Usage,
    billed: Nusd,
) -> Result<Vec<PlannedHold>, MissingAttribute> {
    charges_at(
        policies,
        attrs,
        now,
        usage,
        billed,
        &[Level::Gateway, Level::Project],
    )
}

/// The budget counters of the given levels a call settled after its request is charged to (a
/// batch's usage, a Realtime response), each holding the amount it adds.
pub fn charges_at(
    policies: &Policies,
    attrs: &RequestAttrs<'_>,
    now: UtcMicros,
    usage: &Usage,
    billed: Nusd,
    levels: &[Level],
) -> Result<Vec<PlannedHold>, MissingAttribute> {
    let set = applicable(policies, attrs, Scope::Request)?;
    let mut out: Vec<PlannedHold> = set
        .iter()
        .filter(|a| levels.contains(&a.decl.level))
        .filter_map(|a| {
            let DeclKind::Budget(b) = &a.decl.kind else {
                return None;
            };
            let (amount, unit) = match &b.unit {
                BudgetUnit::Usd => (u128::try_from(billed.0).unwrap_or(0), Unit::Usd),
                BudgetUnit::Tokens(w) => (
                    w.charge(usage).unwrap_or(u128::MAX),
                    Unit::BudgetTokens(w.clone()),
                ),
            };
            (amount > 0).then(|| PlannedHold {
                hold: Hold {
                    counter: a.counter,
                    group: a.group,
                    rule: Rule::Budget {
                        amount: a.quota,
                        period: b.period.bounds(now),
                    },
                    amount,
                    mode: HoldMode::Track,
                },
                info: HoldInfo {
                    level: a.decl.level,
                    owner: a.decl.owner.clone(),
                    name: a.decl.name.clone(),
                    unit,
                    on_unavailable: b.on_unavailable,
                    per_us: 0,
                    alert_at: b.alert_at.clone(),
                    partition: a.partition.clone(),
                    identity: a.identity,
                    declared: a.quota,
                    bypass_on_hit: false,

                    class: None,
                },
            })
        })
        .collect();
    out.sort_by_key(|h| h.hold.counter);
    out.dedup_by(|a, b| a.hold.counter == b.hold.counter);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use antirion_core::ResourceUid;
    use serde_json::json;

    use super::*;

    fn policies(doc: serde_json::Value) -> Policies {
        Policies::compile(&doc, &|_, n| ResourceUid(n.len() as u128 + 1)).unwrap()
    }

    fn attrs() -> RequestAttrs<'static> {
        RequestAttrs {
            organization: "default",
            team: "default",
            project: "p",
            key: "k",
            model: Some("m"),
            endpoint: "chat_completions",
            ..RequestAttrs::default()
        }
    }

    #[test]
    fn only_gateway_and_project_budgets_are_charged() {
        let p = policies(json!({
            "projects": {"p": {"policies": {"budgets": {
                "pb": {"unit": "usd", "amount": "0.01"},
                "tok": {"unit": "tokens", "amount": 1000}}}}},
            "keys": {"k": {"project": "p", "policies": {"budgets": {"kb": {"unit": "usd", "amount": "1"}}}}}
        }));
        let usage = Usage {
            input: 1,
            output: 5,
            ..Usage::default()
        };
        let got = charges(
            &p,
            &attrs(),
            UtcMicros(1_790_000_000_000_000),
            &usage,
            Nusd(50_000_000),
        )
        .unwrap();
        let by_name = |n: &str| got.iter().find(|h| h.info.name == n);
        assert_eq!(by_name("pb").unwrap().hold.amount, 50_000_000);
        assert_eq!(by_name("tok").unwrap().hold.amount, 6);
        assert!(
            by_name("kb").is_none(),
            "key budgets are not charged for shadow calls"
        );
        let free = charges(
            &p,
            &attrs(),
            UtcMicros(1_790_000_000_000_000),
            &Usage::default(),
            Nusd(0),
        )
        .unwrap();
        assert!(free.is_empty());
    }
}
