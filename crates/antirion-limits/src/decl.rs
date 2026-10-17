//! Typed limit and budget declarations, read from a validated, secret-resolved document.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use crate::money::{Dec, Nusd};
use crate::period::{PeriodError, PeriodSpec};
use crate::price::PriceError;
use crate::settings::{BudgetsSettings, LimitsSettings};
use crate::weights::{Weights, WeightsError};

/// What a counter does when the counter store does not answer.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum OnUnavailable {
    /// Enforce locally with the node's share of the quota.
    LocalShare,
    /// Refuse with 503 `dependency_unavailable`.
    FailClosed,
    /// Ignore the counter.
    FailOpen,
}

impl OnUnavailable {
    pub(crate) fn parse(s: &str) -> Option<OnUnavailable> {
        Some(match s {
            "local_share" => OnUnavailable::LocalShare,
            "fail_closed" => OnUnavailable::FailClosed,
            "fail_open" => OnUnavailable::FailOpen,
            _ => return None,
        })
    }
}

/// A request attribute a declaration may partition on.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PartitionAttr {
    Key,
    Project,
    Team,
    Organization,
    User,
    EndUser,
    Model,
    Ip,
    Endpoint,
    Tag(String),
    McpServer,
    McpTool,
    A2aAgent,
}

impl PartitionAttr {
    #[must_use]
    pub fn parse(s: &str) -> Option<PartitionAttr> {
        Some(match s {
            "key" => PartitionAttr::Key,
            "project" => PartitionAttr::Project,
            "team" => PartitionAttr::Team,
            "organization" => PartitionAttr::Organization,
            "user" => PartitionAttr::User,
            "end_user" => PartitionAttr::EndUser,
            "model" => PartitionAttr::Model,
            "ip" => PartitionAttr::Ip,
            "endpoint" => PartitionAttr::Endpoint,
            "mcp_server" => PartitionAttr::McpServer,
            "mcp_tool" => PartitionAttr::McpTool,
            "a2a_agent" => PartitionAttr::A2aAgent,
            other => PartitionAttr::Tag(other.strip_prefix("tag:")?.to_owned()),
        })
    }

    /// The name used in config and in `param` of a refusal.
    #[must_use]
    pub fn name(&self) -> String {
        match self {
            PartitionAttr::Key => "key".into(),
            PartitionAttr::Project => "project".into(),
            PartitionAttr::Team => "team".into(),
            PartitionAttr::Organization => "organization".into(),
            PartitionAttr::User => "user".into(),
            PartitionAttr::EndUser => "end_user".into(),
            PartitionAttr::Model => "model".into(),
            PartitionAttr::Ip => "ip".into(),
            PartitionAttr::Endpoint => "endpoint".into(),
            PartitionAttr::Tag(t) => format!("tag:{t}"),
            PartitionAttr::McpServer => "mcp_server".into(),
            PartitionAttr::McpTool => "mcp_tool".into(),
            PartitionAttr::A2aAgent => "a2a_agent".into(),
        }
    }
}

/// What to do when a partition attribute is absent from the request.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum OnMissing {
    Shared,
    Skip,
    Reject,
}

/// `match` / `exclude` filters: every present list must match (AND); each list is OR.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct MatchSpec {
    pub models: Vec<String>,
    pub endpoints: Vec<String>,
    pub tags: Vec<String>,
    pub mcp_servers: Vec<String>,
    pub mcp_tools: Vec<String>,
    pub a2a_agents: Vec<String>,
}

/// Partitioning of a declaration into counters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Partition {
    pub attrs: Vec<PartitionAttr>,
    pub on_missing: OnMissing,
    /// Quota per value of the single partition attribute, in the declaration's units.
    pub overrides: BTreeMap<String, u128>,
}

/// The unit of a limit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LimitUnit {
    /// `count: successful` refunds the hold unless the request succeeded.
    Requests {
        successful_only: bool,
    },
    Tokens(Weights),
    Concurrency,
}

impl LimitUnit {
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            LimitUnit::Requests { .. } => "requests",
            LimitUnit::Tokens(_) => "tokens",
            LimitUnit::Concurrency => "concurrency",
        }
    }
}

/// The counting algorithm of a rate limit.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Algorithm {
    Gcra,
    SlidingWindow { precision: u32 },
}

/// A limit declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LimitDecl {
    pub unit: LimitUnit,
    pub limit: u64,
    /// The window in microseconds; zero for concurrency.
    pub per_us: u64,
    /// The burst; zero whenever `limit` is zero.
    pub burst: u64,
    pub algorithm: Algorithm,
    pub partition: Partition,
    pub matching: Option<MatchSpec>,
    pub exclude: Option<MatchSpec>,
    pub enforce: bool,
    pub on_unavailable: OnUnavailable,
    /// `priority_shares`: the limit divided between the keys' priority classes.
    pub priority: Option<crate::priority::PriorityShares>,
}

/// The unit of a budget.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BudgetUnit {
    /// Billed cost in nUSD.
    Usd,
    Tokens(Weights),
}

/// A budget declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct BudgetDecl {
    pub unit: BudgetUnit,
    /// nUSD for `usd`, tokens for `tokens`.
    pub amount: u128,
    pub period: PeriodSpec,
    /// The period text, timezone and week start as declared: part of the counter identity.
    pub period_key: String,
    pub hard: bool,
    /// `mode: approval`: refuses like `hard` and asks the owner for an overage.
    pub approval: bool,
    pub alert_at: Vec<Dec>,
    pub partition: Partition,
    pub matching: Option<MatchSpec>,
    pub exclude: Option<MatchSpec>,
    pub on_unavailable: OnUnavailable,
    /// The user who receives the budget's notifications and overage requests.
    pub owner: Option<String>,
    pub downgrade: Option<Downgrade>,
    /// `cache_hits_bypass_budgets`: a refusal waits for the cache lookup, and a hit is served.
    pub bypass_on_hit: bool,
    /// `algorithm: sliding_window`: the sub-windows of a rolling budget over its duration period.
    pub rolling: Option<u32>,
}

/// Where a budget sends the requests it covers once its spend reaches a ratio of its amount.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Downgrade {
    pub at: Dec,
    pub to: DowngradeTo,
}

/// The Model, or the tier, a downgraded request is served by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DowngradeTo {
    Model(String),
    Tier(String),
}

#[derive(Deserialize)]
struct RawDowngrade {
    at: Value,
    to_model: Option<String>,
    to_tier: Option<String>,
}

impl Downgrade {
    fn parse(raw: RawDowngrade) -> Result<Downgrade, DeclError> {
        let at = raw
            .at
            .as_f64()
            .and_then(Dec::from_ratio)
            .ok_or_else(|| field("downgrade.at", "a ratio in [0, 1]"))?;
        let to = match (raw.to_model, raw.to_tier) {
            (Some(m), None) => DowngradeTo::Model(m),
            (None, Some(t)) => DowngradeTo::Tier(t),
            _ => return Err(field("downgrade", "exactly one of to_model and to_tier")),
        };
        Ok(Downgrade { at, to })
    }
}

/// A declaration that cannot be compiled.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeclError {
    #[error("not a valid declaration: {0}")]
    Shape(String),
    #[error("{0}")]
    Weights(#[from] WeightsError),
    #[error("{0}")]
    Period(#[from] PeriodError),
    #[error("{0}")]
    Price(#[from] PriceError),
    #[error("{field}: {message}")]
    Field { field: String, message: String },
}

pub(crate) fn field(field: &str, message: impl Into<String>) -> DeclError {
    DeclError::Field {
        field: field.to_owned(),
        message: message.into(),
    }
}

/// Reads a quota or override in a declaration's unit.
type AmountOf<'a> = &'a dyn Fn(&Value) -> Option<u128>;

#[derive(Deserialize)]
struct RawLimit {
    unit: String,
    limit: u64,
    per: Option<String>,
    burst: Option<u64>,
    algorithm: Option<String>,
    precision: Option<u32>,
    tokens: Option<String>,
    #[serde(default)]
    weights: BTreeMap<String, String>,
    count: Option<String>,
    #[serde(default)]
    partition_by: Vec<String>,
    on_missing: Option<String>,
    #[serde(default)]
    overrides: BTreeMap<String, Value>,
    #[serde(rename = "match")]
    matching: Option<MatchSpec>,
    exclude: Option<MatchSpec>,
    enforce: Option<bool>,
    on_store_unavailable: Option<String>,
    #[serde(default)]
    priority_shares: BTreeMap<String, Value>,
    priority_mode: Option<String>,
}

#[derive(Deserialize)]
struct RawBudget {
    unit: String,
    amount: Value,
    period: Option<String>,
    timezone: Option<String>,
    week_start: Option<String>,
    mode: Option<String>,
    #[serde(default)]
    alert_at: Vec<Value>,
    tokens: Option<String>,
    #[serde(default)]
    weights: BTreeMap<String, String>,
    #[serde(default)]
    partition_by: Vec<String>,
    on_missing: Option<String>,
    #[serde(default)]
    overrides: BTreeMap<String, Value>,
    #[serde(rename = "match")]
    matching: Option<MatchSpec>,
    exclude: Option<MatchSpec>,
    on_store_unavailable: Option<String>,
    owner: Option<String>,
    downgrade: Option<RawDowngrade>,
    cache_hits_bypass_budgets: Option<bool>,
    algorithm: Option<String>,
    precision: Option<u32>,
}

fn partition(
    attrs: &[String],
    on_missing: Option<&str>,
    overrides: &BTreeMap<String, Value>,
    amount: AmountOf<'_>,
) -> Result<Partition, DeclError> {
    let attrs = attrs
        .iter()
        .map(|a| {
            PartitionAttr::parse(a).ok_or_else(|| field("partition_by", format!("unknown attribute {a:?}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let on_missing = match on_missing.unwrap_or("shared") {
        "shared" => OnMissing::Shared,
        "skip" => OnMissing::Skip,
        "reject" => OnMissing::Reject,
        other => return Err(field("on_missing", format!("unknown value {other:?}"))),
    };
    let mut out = BTreeMap::new();
    for (k, v) in overrides {
        let q = amount(v).ok_or_else(|| field(&format!("overrides.{k}"), "not a valid amount"))?;
        out.insert(k.clone(), q);
    }
    if !out.is_empty() && attrs.len() != 1 {
        return Err(field("overrides", "requires exactly one partition attribute"));
    }
    Ok(Partition {
        attrs,
        on_missing,
        overrides: out,
    })
}

fn on_unavailable(v: Option<&str>, default: OnUnavailable) -> Result<OnUnavailable, DeclError> {
    match v {
        None => Ok(default),
        Some(s) => OnUnavailable::parse(s)
            .ok_or_else(|| field("on_store_unavailable", format!("unknown value {s:?}"))),
    }
}

impl LimitDecl {
    /// Compiles a limit declaration with the settings' defaults.
    pub fn parse(v: &Value, settings: &LimitsSettings) -> Result<LimitDecl, DeclError> {
        let raw: RawLimit = serde_json::from_value(v.clone()).map_err(|e| DeclError::Shape(e.to_string()))?;
        let unit = match raw.unit.as_str() {
            "requests" => LimitUnit::Requests {
                successful_only: raw.count.as_deref() == Some("successful"),
            },
            "tokens" => LimitUnit::Tokens(Weights::from_preset(
                raw.tokens.as_deref().unwrap_or("total"),
                &raw.weights,
            )?),
            "concurrency" => LimitUnit::Concurrency,
            other => return Err(field("unit", format!("unknown unit {other:?}"))),
        };
        let per_us = match (&unit, raw.per.as_deref()) {
            (LimitUnit::Concurrency, _) => 0,
            (_, Some(per)) => antirion_config::scalar::duration_nanos(per)
                .and_then(|ns| u64::try_from(ns / 1_000).ok())
                .filter(|us| *us >= 1_000_000)
                .ok_or_else(|| field("per", "must be a duration of at least 1s"))?,
            (_, None) => return Err(field("per", "is required")),
        };
        let algorithm = match raw.algorithm.as_deref().unwrap_or(settings.algorithm.as_str()) {
            "gcra" => Algorithm::Gcra,
            "sliding_window" => Algorithm::SlidingWindow {
                precision: raw.precision.unwrap_or(60).clamp(1, 3600),
            },
            other => return Err(field("algorithm", format!("unknown algorithm {other:?}"))),
        };
        let burst = if raw.limit == 0 {
            0
        } else {
            raw.burst.unwrap_or(raw.limit)
        };
        Ok(LimitDecl {
            unit,
            limit: raw.limit,
            per_us,
            burst,
            algorithm,
            partition: partition(
                &raw.partition_by,
                raw.on_missing.as_deref(),
                &raw.overrides,
                &|v| v.as_u64().map(u128::from),
            )?,
            matching: raw.matching,
            exclude: raw.exclude,
            enforce: raw.enforce.unwrap_or(true),
            on_unavailable: on_unavailable(
                raw.on_store_unavailable.as_deref(),
                settings.on_store_unavailable,
            )?,
            priority: crate::priority::PriorityShares::parse(
                &raw.priority_shares,
                raw.priority_mode.as_deref(),
            )?,
        })
    }
}

impl BudgetDecl {
    /// Compiles a budget declaration with the settings' defaults.
    pub fn parse(v: &Value, settings: &BudgetsSettings) -> Result<BudgetDecl, DeclError> {
        let raw: RawBudget =
            serde_json::from_value(v.clone()).map_err(|e| DeclError::Shape(e.to_string()))?;
        let usd =
            |v: &Value| -> Option<u128> { v.as_str().and_then(|s| Nusd::parse_usd(s).ok()).map(Nusd::units) };
        let tokens = |v: &Value| -> Option<u128> { v.as_u64().map(u128::from) };
        let (unit, amount_of): (BudgetUnit, AmountOf<'_>) = match raw.unit.as_str() {
            "usd" => (BudgetUnit::Usd, &usd),
            "tokens" => (
                BudgetUnit::Tokens(Weights::from_preset(
                    raw.tokens.as_deref().unwrap_or("total"),
                    &raw.weights,
                )?),
                &tokens,
            ),
            other => return Err(field("unit", format!("unknown unit {other:?}"))),
        };
        let amount = amount_of(&raw.amount).ok_or_else(|| field("amount", "not a valid amount"))?;
        let period_text = raw.period.as_deref().unwrap_or("lifetime");
        let period = PeriodSpec::parse(period_text, raw.timezone.as_deref(), raw.week_start.as_deref())?;
        let rolling = rolling(&period, raw.algorithm.as_deref(), raw.precision)?;
        let mut period_key = format!(
            "{period_text}|{}|{}",
            raw.timezone.as_deref().unwrap_or("UTC"),
            raw.week_start.as_deref().unwrap_or("monday")
        );
        if let Some(p) = rolling {
            period_key.push_str(&format!("|sliding:{p}"));
        }
        let (hard, approval) = match raw.mode.as_deref().unwrap_or("hard") {
            "hard" => (true, false),
            "soft" => (false, false),
            "approval" => (true, true),
            other => return Err(field("mode", format!("unknown mode {other:?}"))),
        };
        let downgrade = raw.downgrade.map(Downgrade::parse).transpose()?;
        let alert_at = raw
            .alert_at
            .iter()
            .map(|r| {
                r.as_f64()
                    .filter(|_| r.is_number())
                    .and_then(Dec::from_ratio)
                    .ok_or_else(|| field("alert_at", "ratios are numbers in [0, 1]"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if alert_at.windows(2).any(|w| matches!(w, [a, b] if a >= b)) {
            return Err(field("alert_at", "ratios must be strictly increasing"));
        }
        let alert_at = if alert_at.is_empty() && !hard {
            vec![Dec::ONE]
        } else {
            alert_at
        };
        Ok(BudgetDecl {
            unit,
            amount,
            period,
            period_key,
            hard,
            approval,
            alert_at,
            partition: partition(
                &raw.partition_by,
                raw.on_missing.as_deref(),
                &raw.overrides,
                amount_of,
            )?,
            matching: raw.matching,
            exclude: raw.exclude,
            on_unavailable: on_unavailable(
                raw.on_store_unavailable.as_deref(),
                settings.on_store_unavailable,
            )?,
            owner: raw.owner,
            downgrade,
            bypass_on_hit: raw.cache_hits_bypass_budgets.unwrap_or(false),
            rolling,
        })
    }
}

impl BudgetDecl {
    /// Whether the budget's spend is projected to the end of its period: a rolling budget's window
    /// never ends, so it has no forecast.
    #[must_use]
    pub const fn forecasts(&self) -> bool {
        self.rolling.is_none()
    }

    /// The counting rule of a counter of this budget holding `amount`, at `now`: the period's
    /// budget, or the rolling window.
    #[must_use]
    pub fn rule(&self, amount: u128, now: antirion_core::UtcMicros) -> crate::counter::Rule {
        match (self.rolling, &self.period) {
            (Some(precision), PeriodSpec::Every { us }) => {
                crate::counter::Rule::Rolling(crate::math::Window {
                    limit: u64::try_from(amount).unwrap_or(u64::MAX),
                    per_us: u64::try_from(*us).unwrap_or(u64::MAX),
                    precision,
                })
            }
            _ => crate::counter::Rule::Budget {
                amount,
                period: self.period.bounds(now),
            },
        }
    }

    /// The period the budget's spend at `now` is reported for (see [`crate::counter::Rule::budget_period`]).
    #[must_use]
    pub fn bounds(&self, now: antirion_core::UtcMicros) -> crate::period::PeriodBounds {
        self.rule(0, now)
            .budget_period(now)
            .unwrap_or_else(|| self.period.bounds(now))
    }
}

/// Microseconds in an hour: a rolling budget's default sub-window.
const HOUR_US: i64 = 3_600_000_000;

/// The sub-windows of a rolling budget (`algorithm: sliding_window`): only over a duration period,
/// `precision` only with that algorithm, at least one second per sub-window; hourly by default.
fn rolling(
    period: &PeriodSpec,
    algorithm: Option<&str>,
    precision: Option<u32>,
) -> Result<Option<u32>, DeclError> {
    match algorithm {
        None => match precision {
            Some(_) => Err(field("precision", "applies only to algorithm sliding_window")),
            None => Ok(None),
        },
        Some("sliding_window") => {
            let PeriodSpec::Every { us } = period else {
                return Err(field("algorithm", "sliding_window needs a duration period"));
            };
            let p = match precision {
                Some(p) => p,
                None => u32::try_from((us / HOUR_US).max(1)).unwrap_or(u32::MAX),
            };
            if p == 0 || i64::from(p) > us / 1_000_000 {
                return Err(field("precision", "sub-windows must be at least one second long"));
            }
            if us % i64::from(p) != 0 {
                return Err(field(
                    "precision",
                    "must divide the period into whole microseconds",
                ));
            }
            Ok(Some(p))
        }
        Some(other) => Err(field("algorithm", format!("unknown algorithm {other:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn limit_defaults_and_zero_burst() {
        let s = LimitsSettings::default();
        let l = LimitDecl::parse(&json!({"unit": "requests", "limit": 10, "per": "1m"}), &s).unwrap();
        assert_eq!((l.limit, l.per_us, l.burst), (10, 60_000_000, 10));
        assert_eq!(l.algorithm, Algorithm::Gcra);
        assert_eq!(l.on_unavailable, OnUnavailable::LocalShare);
        let z = LimitDecl::parse(
            &json!({"unit": "requests", "limit": 0, "per": "1m", "burst": 5}),
            &s,
        )
        .unwrap();
        assert_eq!(z.burst, 0);
        let c = LimitDecl::parse(&json!({"unit": "concurrency", "limit": 2}), &s).unwrap();
        assert_eq!(c.per_us, 0);
        assert!(LimitDecl::parse(&json!({"unit": "tokens", "limit": 1}), &s).is_err());
        assert!(LimitDecl::parse(&json!({"unit": "tokens", "limit": 1, "per": "0s"}), &s).is_err());
    }

    #[test]
    fn limit_partition_and_overrides() {
        let s = LimitsSettings::default();
        let l = LimitDecl::parse(
            &json!({"unit": "requests", "limit": 2, "per": "1h", "partition_by": ["end_user"],
                    "on_missing": "reject", "overrides": {"vip": 5}, "algorithm": "sliding_window", "precision": 10}),
            &s,
        )
        .unwrap();
        assert_eq!(l.partition.attrs, vec![PartitionAttr::EndUser]);
        assert_eq!(l.partition.on_missing, OnMissing::Reject);
        assert_eq!(l.partition.overrides.get("vip"), Some(&5));
        assert_eq!(l.algorithm, Algorithm::SlidingWindow { precision: 10 });
        assert!(LimitDecl::parse(
            &json!({"unit": "requests", "limit": 2, "per": "1h", "partition_by": ["key", "team"], "overrides": {"a": 1}}),
            &s
        )
        .is_err());
        assert_eq!(
            PartitionAttr::parse("tag:env"),
            Some(PartitionAttr::Tag("env".into()))
        );
    }

    #[test]
    fn budgets() {
        let s = BudgetsSettings::from_value(None);
        let b = BudgetDecl::parse(
            &json!({"unit": "usd", "amount": "3.50", "period": "monthly", "overrides": {"c": "0.01"}, "partition_by": ["end_user"]}),
            &s,
        )
        .unwrap();
        assert_eq!(b.amount, 3_500_000_000);
        assert!(b.hard);
        assert_eq!(b.on_unavailable, OnUnavailable::FailClosed);
        assert_eq!(b.partition.overrides.get("c"), Some(&10_000_000));
        let soft = BudgetDecl::parse(&json!({"unit": "tokens", "amount": 100, "mode": "soft"}), &s).unwrap();
        assert_eq!(soft.alert_at, vec![Dec::ONE]);
        assert!(matches!(soft.period, PeriodSpec::Lifetime));
        assert!(BudgetDecl::parse(&json!({"unit": "usd", "amount": "1", "period": "0s"}), &s).is_err());
    }

    #[test]
    fn approval_owner_and_downgrade() {
        let s = BudgetsSettings::from_value(None);
        let b = BudgetDecl::parse(
            &json!({"unit": "usd", "amount": "10", "mode": "approval", "owner": "alice",
                    "downgrade": {"at": 0.5, "to_model": "cheap"}}),
            &s,
        )
        .unwrap();
        assert!(b.hard && b.approval);
        assert_eq!(b.owner.as_deref(), Some("alice"));
        assert_eq!(
            b.downgrade,
            Some(Downgrade {
                at: Dec::from_ratio(0.5).unwrap(),
                to: DowngradeTo::Model("cheap".into())
            })
        );
        let t = BudgetDecl::parse(
            &json!({"unit": "usd", "amount": "10", "downgrade": {"at": 1, "to_tier": "fast"}}),
            &s,
        )
        .unwrap();
        assert_eq!(t.downgrade.unwrap().to, DowngradeTo::Tier("fast".into()));
        assert!(!t.approval);
        assert!(
            BudgetDecl::parse(
                &json!({"unit": "usd", "amount": "1", "downgrade": {"at": 0.5, "to_tier": "a", "to_model": "b"}}),
                &s
            )
            .is_err()
        );
    }

    #[test]
    fn rolling_budgets_have_no_forecast() {
        let s = BudgetsSettings::from_value(None);
        let rolling = BudgetDecl::parse(
            &json!({"unit": "usd", "amount": "10", "period": "30d", "algorithm": "sliding_window"}),
            &s,
        )
        .unwrap();
        assert!(!rolling.forecasts());
        for period in ["30d", "monthly", "lifetime"] {
            let b = BudgetDecl::parse(&json!({"unit": "usd", "amount": "10", "period": period}), &s).unwrap();
            assert!(b.forecasts(), "{period}");
        }
    }
}
