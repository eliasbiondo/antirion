//! Spend ledger lines: one immutable record per upstream call, cache hit, expired reservation or
//! adjustment, idempotent by `(request_id, line)`.

use antirion_core::UtcMicros;
use serde::Serialize;
use serde::ser::{SerializeMap, Serializer};
use serde_json::{Map, Value, json};

use crate::cost::CostStatus;
use crate::counter::CounterId;
use crate::estimate::OutSource;
use crate::money::Nusd;
use crate::usage::Usage;

/// What happened to the call a line records.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum LineStatus {
    Success,
    Failed,
    FailedWithUsage,
    Cancelled,
    Expired,
    CacheHit,
    Adjustment,
}

impl LineStatus {
    #[must_use]
    pub fn parse(s: &str) -> Option<LineStatus> {
        [
            LineStatus::Success,
            LineStatus::Failed,
            LineStatus::FailedWithUsage,
            LineStatus::Cancelled,
            LineStatus::Expired,
            LineStatus::CacheHit,
            LineStatus::Adjustment,
        ]
        .into_iter()
        .find(|v| v.as_str() == s)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            LineStatus::Success => "success",
            LineStatus::Failed => "failed",
            LineStatus::FailedWithUsage => "failed_with_usage",
            LineStatus::Cancelled => "cancelled",
            LineStatus::Expired => "expired",
            LineStatus::CacheHit => "cache_hit",
            LineStatus::Adjustment => "adjustment",
        }
    }
}

/// Why the call was made.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Purpose {
    Primary,
    Retry,
    Fallback,
    Hedge,
    Shadow,
    Guardrail,
    Router,
    Cache,
    CacheEmbedding,
    /// An operator's reset or adjustment of a budget.
    Adjustment,
    /// An operator's test of a provider credential, charged to no budget.
    Test,
}

impl Purpose {
    #[must_use]
    pub fn parse(s: &str) -> Option<Purpose> {
        [
            Purpose::Primary,
            Purpose::Retry,
            Purpose::Fallback,
            Purpose::Hedge,
            Purpose::Shadow,
            Purpose::Guardrail,
            Purpose::Router,
            Purpose::Cache,
            Purpose::CacheEmbedding,
            Purpose::Adjustment,
            Purpose::Test,
        ]
        .into_iter()
        .find(|v| v.as_str() == s)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Purpose::Primary => "primary",
            Purpose::Retry => "retry",
            Purpose::Fallback => "fallback",
            Purpose::Hedge => "hedge",
            Purpose::Shadow => "shadow",
            Purpose::Guardrail => "guardrail",
            Purpose::Router => "router",
            Purpose::Cache => "cache",
            Purpose::CacheEmbedding => "cache_embedding",
            Purpose::Adjustment => "adjustment",
            Purpose::Test => "test",
        }
    }

    /// Auxiliary calls are charged to gateway-level budgets, and to the request's others only
    /// with `budgets.charge_auxiliary`.
    #[must_use]
    pub const fn is_auxiliary(self) -> bool {
        matches!(
            self,
            Purpose::Shadow | Purpose::Guardrail | Purpose::Router | Purpose::CacheEmbedding
        )
    }
}

/// Where a line's usage came from.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum UsageSource {
    Upstream,
    Estimated,
    Cache,
}

impl UsageSource {
    #[must_use]
    pub fn parse(s: &str) -> Option<UsageSource> {
        [UsageSource::Upstream, UsageSource::Estimated, UsageSource::Cache]
            .into_iter()
            .find(|v| v.as_str() == s)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            UsageSource::Upstream => "upstream",
            UsageSource::Estimated => "estimated",
            UsageSource::Cache => "cache",
        }
    }
}

/// The admission estimate a line's request reserved.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Reserved {
    pub input: u64,
    pub output: u64,
    pub usd: Option<Nusd>,
    pub out_source: OutSource,
}

/// A budget a line charged, and the amount in the budget's unit (nUSD or tokens; negative for a
/// credit).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetCharge {
    pub level: String,
    pub owner: String,
    pub name: String,
    /// The budget's identity without its period: partitions are hashed, never stored raw.
    pub identity: CounterId,
    pub amount: i128,
}

/// The MCP server and tool, or the A2A agent, a line's call went to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallTarget {
    pub mcp_server: Option<String>,
    pub mcp_tool: Option<String>,
    pub a2a_agent: Option<String>,
}

/// One ledger line.
#[derive(Clone, Debug, PartialEq)]
pub struct LedgerLine {
    /// The gateway-generated request id (the log id), never the client's `x-request-id`.
    pub request_id: String,
    pub line: u32,
    pub purpose: Purpose,
    pub shadow: bool,
    pub status: LineStatus,
    pub admitted_at: UtcMicros,
    pub settled_at: UtcMicros,
    pub organization: String,
    pub team: String,
    pub project: String,
    pub key: String,
    pub user: Option<String>,
    pub end_user: Option<String>,
    pub tags: Vec<String>,
    pub model: Option<String>,
    pub route: Option<String>,
    pub provider: Option<String>,
    pub deployment: Option<String>,
    pub upstream_model: Option<String>,
    /// The MCP server, tool and A2A agent of a priced tool or agent call.
    pub call: CallTarget,
    pub endpoint: String,
    pub stream: bool,
    pub usage: Usage,
    pub usage_raw: Option<Value>,
    pub usage_source: UsageSource,
    /// Absent when unpriced (and not `on_unpriced: zero`).
    pub cost: Option<Nusd>,
    pub billed: Option<Nusd>,
    pub cost_status: CostStatus,
    pub price_version: Option<String>,
    /// The service tier the call was priced under, when the response or the request named one.
    pub service_tier: Option<String>,
    /// What a cache hit saved: its entry's usage at the current price of the deployment that
    /// produced it. Never charged.
    pub avoided: Option<Nusd>,
    pub reserved: Reserved,
    pub budgets: Vec<BudgetCharge>,
}

/// The usage record under its field names.
#[must_use]
pub fn usage_json(u: &Usage) -> Value {
    Value::Object(
        u.fields()
            .iter()
            .map(|(k, n)| ((*k).to_owned(), Value::from(*n)))
            .collect(),
    )
}

impl LedgerLine {
    /// The line as the Admin API shows it: money as decimal strings, times in RFC 3339 UTC.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let money = |n: Option<Nusd>| n.map_or(Value::Null, |v| Value::String(v.to_usd_string()));
        let opt = |s: &Option<String>| s.clone().map_or(Value::Null, Value::String);
        let mut m = Map::new();
        m.insert("request_id".into(), Value::String(self.request_id.clone()));
        m.insert("line".into(), Value::from(self.line));
        m.insert("purpose".into(), Value::from(self.purpose.as_str()));
        m.insert("shadow".into(), Value::from(self.shadow));
        m.insert("status".into(), Value::from(self.status.as_str()));
        m.insert("admitted_at".into(), Value::String(self.admitted_at.to_rfc3339()));
        m.insert("settled_at".into(), Value::String(self.settled_at.to_rfc3339()));
        for (k, v) in [
            ("organization", &self.organization),
            ("team", &self.team),
            ("project", &self.project),
            ("key", &self.key),
            ("endpoint", &self.endpoint),
        ] {
            m.insert(k.into(), Value::String(v.clone()));
        }
        for (k, v) in [
            ("user", &self.user),
            ("end_user", &self.end_user),
            ("model", &self.model),
            ("route", &self.route),
            ("provider", &self.provider),
            ("deployment", &self.deployment),
            ("upstream_model", &self.upstream_model),
            ("price_version", &self.price_version),
            ("mcp_server", &self.call.mcp_server),
            ("mcp_tool", &self.call.mcp_tool),
            ("a2a_agent", &self.call.a2a_agent),
        ] {
            m.insert(k.into(), opt(v));
        }
        m.insert("tags".into(), Value::from(self.tags.clone()));
        m.insert("stream".into(), Value::from(self.stream));
        m.insert("usage".into(), usage_json(&self.usage));
        m.insert("usage_raw".into(), self.usage_raw.clone().unwrap_or(Value::Null));
        m.insert("usage_source".into(), Value::from(self.usage_source.as_str()));
        m.insert("cost_usd".into(), money(self.cost));
        m.insert("billed_usd".into(), money(self.billed));
        m.insert("avoided_cost_usd".into(), money(self.avoided));
        m.insert("cost_status".into(), Value::from(self.cost_status.as_str()));
        m.insert(
            "reserved".into(),
            json!({
                "in": self.reserved.input,
                "out": self.reserved.output,
                "usd": money(self.reserved.usd),
                "out_source": self.reserved.out_source.as_str(),
            }),
        );
        let budgets: Vec<Value> = self
            .budgets
            .iter()
            .map(|b| {
                json!({"level": b.level, "owner": b.owner, "name": b.name, "counter": b.identity.hex(),
                       "amount": b.amount.to_string()})
            })
            .collect();
        m.insert("budgets".into(), Value::Array(budgets));
        if let Some(t) = &self.service_tier {
            m.insert("service_tier".into(), Value::String(t.clone()));
        }
        Value::Object(m)
    }
}

struct Money(Option<Nusd>);

impl Serialize for Money {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Some(n) => s.serialize_str(&n.to_usd_string()),
            None => s.serialize_none(),
        }
    }
}

struct UsageFields<'a>(&'a Usage);

impl Serialize for UsageFields<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let fields = self.0.fields();
        let mut m = s.serialize_map(Some(fields.len()))?;
        for (k, n) in fields {
            m.serialize_entry(k, &n)?;
        }
        m.end()
    }
}

struct ReservedJson<'a>(&'a Reserved);

impl Serialize for ReservedJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let r = self.0;
        let mut m = s.serialize_map(Some(4))?;
        m.serialize_entry("in", &r.input)?;
        m.serialize_entry("out", &r.output)?;
        m.serialize_entry("usd", &Money(r.usd))?;
        m.serialize_entry("out_source", r.out_source.as_str())?;
        m.end()
    }
}

struct ChargeJson<'a>(&'a BudgetCharge);

impl Serialize for ChargeJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let b = self.0;
        let mut m = s.serialize_map(Some(5))?;
        m.serialize_entry("level", &b.level)?;
        m.serialize_entry("owner", &b.owner)?;
        m.serialize_entry("name", &b.name)?;
        m.serialize_entry("counter", &b.identity.hex())?;
        m.serialize_entry("amount", &b.amount.to_string())?;
        m.end()
    }
}

/// The form of [`LedgerLine::to_json`], written without building it.
impl Serialize for LedgerLine {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("request_id", &self.request_id)?;
        m.serialize_entry("line", &self.line)?;
        m.serialize_entry("purpose", self.purpose.as_str())?;
        m.serialize_entry("shadow", &self.shadow)?;
        m.serialize_entry("status", self.status.as_str())?;
        m.serialize_entry("admitted_at", &self.admitted_at.to_rfc3339())?;
        m.serialize_entry("settled_at", &self.settled_at.to_rfc3339())?;
        for (k, v) in [
            ("organization", &self.organization),
            ("team", &self.team),
            ("project", &self.project),
            ("key", &self.key),
            ("endpoint", &self.endpoint),
        ] {
            m.serialize_entry(k, v)?;
        }
        for (k, v) in [
            ("user", &self.user),
            ("end_user", &self.end_user),
            ("model", &self.model),
            ("route", &self.route),
            ("provider", &self.provider),
            ("deployment", &self.deployment),
            ("upstream_model", &self.upstream_model),
            ("price_version", &self.price_version),
            ("mcp_server", &self.call.mcp_server),
            ("mcp_tool", &self.call.mcp_tool),
            ("a2a_agent", &self.call.a2a_agent),
        ] {
            m.serialize_entry(k, v)?;
        }
        m.serialize_entry("tags", &self.tags)?;
        m.serialize_entry("stream", &self.stream)?;
        m.serialize_entry("usage", &UsageFields(&self.usage))?;
        m.serialize_entry("usage_raw", &self.usage_raw)?;
        m.serialize_entry("usage_source", self.usage_source.as_str())?;
        m.serialize_entry("cost_usd", &Money(self.cost))?;
        m.serialize_entry("billed_usd", &Money(self.billed))?;
        m.serialize_entry("avoided_cost_usd", &Money(self.avoided))?;
        m.serialize_entry("cost_status", self.cost_status.as_str())?;
        m.serialize_entry("reserved", &ReservedJson(&self.reserved))?;
        let budgets: Vec<ChargeJson<'_>> = self.budgets.iter().map(ChargeJson).collect();
        m.serialize_entry("budgets", &budgets)?;
        if let Some(t) = &self.service_tier {
            m.serialize_entry("service_tier", t)?;
        }
        m.end()
    }
}

impl LedgerLine {
    /// [`LedgerLine::to_json`] as text, serialised once.
    #[must_use]
    pub fn to_json_string(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// A line read back from [`LedgerLine::to_json`]; `None` when a required field is missing.
    #[must_use]
    pub fn from_json(v: &Value) -> Option<LedgerLine> {
        let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        let time = |k: &str| UtcMicros::parse_rfc3339(v.get(k)?.as_str()?).ok();
        let money = |k: &str| Nusd::parse_signed_usd(v.get(k)?.as_str()?).ok();
        let reserved = v.get("reserved")?;
        let budgets = v
            .get("budgets")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|b| {
                Some(BudgetCharge {
                    level: b.get("level")?.as_str()?.to_owned(),
                    owner: b.get("owner")?.as_str()?.to_owned(),
                    name: b.get("name")?.as_str()?.to_owned(),
                    identity: CounterId::from_hex(b.get("counter")?.as_str()?)?,
                    amount: b.get("amount")?.as_str()?.parse().ok()?,
                })
            })
            .collect();
        Some(LedgerLine {
            request_id: text("request_id")?,
            line: u32::try_from(v.get("line")?.as_u64()?).ok()?,
            purpose: Purpose::parse(v.get("purpose")?.as_str()?)?,
            shadow: v.get("shadow").and_then(Value::as_bool).unwrap_or(false),
            status: LineStatus::parse(v.get("status")?.as_str()?)?,
            admitted_at: time("admitted_at")?,
            settled_at: time("settled_at")?,
            organization: text("organization")?,
            team: text("team")?,
            project: text("project")?,
            key: text("key")?,
            user: text("user"),
            end_user: text("end_user"),
            tags: v
                .get("tags")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|t| t.as_str().map(str::to_owned))
                .collect(),
            model: text("model"),
            route: text("route"),
            provider: text("provider"),
            deployment: text("deployment"),
            upstream_model: text("upstream_model"),
            call: CallTarget {
                mcp_server: text("mcp_server"),
                mcp_tool: text("mcp_tool"),
                a2a_agent: text("a2a_agent"),
            },
            endpoint: text("endpoint")?,
            stream: v.get("stream").and_then(Value::as_bool).unwrap_or(false),
            usage: Usage::from_fields(v.get("usage")?),
            usage_raw: v.get("usage_raw").filter(|u| !u.is_null()).cloned(),
            usage_source: UsageSource::parse(v.get("usage_source")?.as_str()?)?,
            cost: money("cost_usd"),
            billed: money("billed_usd"),
            cost_status: CostStatus::parse(v.get("cost_status")?.as_str()?)?,
            price_version: text("price_version"),
            service_tier: text("service_tier"),
            avoided: money("avoided_cost_usd"),
            reserved: Reserved {
                input: reserved.get("in").and_then(Value::as_u64).unwrap_or(0),
                output: reserved.get("out").and_then(Value::as_u64).unwrap_or(0),
                usd: reserved
                    .get("usd")
                    .and_then(Value::as_str)
                    .and_then(|s| Nusd::parse_usd(s).ok()),
                out_source: OutSource::parse(reserved.get("out_source").and_then(Value::as_str)?)?,
            },
            budgets,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_shape() {
        let line = LedgerLine {
            request_id: "0190".into(),
            line: 1,
            purpose: Purpose::Primary,
            shadow: false,
            status: LineStatus::Success,
            admitted_at: UtcMicros(0),
            settled_at: UtcMicros(1_000_000),
            organization: "o".into(),
            team: "t".into(),
            project: "p".into(),
            key: "k".into(),
            user: None,
            end_user: Some("U".into()),
            tags: vec!["t1".into()],
            model: Some("m".into()),
            route: None,
            provider: Some("openai".into()),
            deployment: Some("d".into()),
            upstream_model: Some("gpt".into()),
            call: crate::ledger::CallTarget::default(),
            endpoint: "chat_completions".into(),
            stream: false,
            usage: Usage {
                input: 1000,
                output: 500,
                ..Usage::default()
            },
            usage_raw: None,
            usage_source: UsageSource::Upstream,
            cost: Some(Nusd(2_000_000)),
            billed: Some(Nusd(2_000_000)),
            cost_status: CostStatus::Exact,
            price_version: Some("abc".into()),
            service_tier: None,
            avoided: None,
            reserved: Reserved {
                input: 10,
                output: 500,
                usd: None,
                out_source: OutSource::Default,
            },
            budgets: vec![],
        };
        let v = line.to_json();
        assert_eq!(v["billed_usd"], "0.002");
        assert_eq!(v["usage"]["input"], 1000);
        assert_eq!(v["reserved"]["out_source"], "default");
        assert_eq!(v["end_user"], "U");
        assert_eq!(v["user"], Value::Null);
        assert_eq!(v["settled_at"], "1970-01-01T00:00:01Z");
        assert!(Purpose::Shadow.is_auxiliary() && !Purpose::Retry.is_auxiliary());
        let mut with_budget = line.clone();
        with_budget.budgets.push(BudgetCharge {
            level: "key".into(),
            owner: "k".into(),
            name: "b".into(),
            identity: CounterId([3; 16]),
            amount: -5,
        });
        with_budget.usage_raw = Some(serde_json::json!({"prompt_tokens": 1000}));
        assert_eq!(with_budget.to_json_string(), with_budget.to_json().to_string());
        assert_eq!(line.to_json_string(), line.to_json().to_string());
        assert_eq!(LedgerLine::from_json(&with_budget.to_json()), Some(with_budget));
    }
}
