//! The top-level `limits`, `budgets` and `pricing` settings; absent fields take their defaults.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::decl::{DeclError, OnUnavailable, field};
use crate::money::Dec;
use crate::price::Price;

/// The `limits.estimation` settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EstimationSettings {
    pub input: InputEstimator,
    pub bytes_per_token: Dec,
    pub image_tokens_default: u64,
    pub audio_tokens_default: u64,
    pub video_tokens_default: u64,
    pub output: OutputMode,
    pub default_max_output_tokens: u64,
    pub output_cap: Option<u64>,
    pub max_output_tokens_ceiling: u64,
}

impl Default for EstimationSettings {
    fn default() -> Self {
        EstimationSettings {
            input: InputEstimator::Auto,
            bytes_per_token: Dec(3 * crate::money::DEC_ONE),
            image_tokens_default: 1600,
            audio_tokens_default: 1500,
            video_tokens_default: 10_000,
            output: OutputMode::Strict,
            default_max_output_tokens: 4096,
            output_cap: None,
            max_output_tokens_ceiling: 1_073_741_823,
        }
    }
}

/// How `E_in` is computed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InputEstimator {
    Auto,
    Bytes,
    UpperBound,
}

/// How `E_out` relates to the request's cap.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OutputMode {
    Strict,
    Clamp,
    Optimistic,
}

/// The `limits.headers` settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderSettings {
    pub ietf: bool,
    pub dialect: bool,
    pub max_policies: usize,
    pub expose_reservation: bool,
}

/// The top-level `limits` settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LimitsSettings {
    pub enabled: bool,
    pub algorithm: String,
    pub estimation: EstimationSettings,
    pub lease_ttl_us: u64,
    pub store_timeout_us: u64,
    pub on_store_unavailable: OnUnavailable,
    pub headers: HeaderSettings,
    pub forward_upstream_headers: bool,
}

impl Default for LimitsSettings {
    fn default() -> Self {
        LimitsSettings {
            enabled: true,
            algorithm: "gcra".into(),
            estimation: EstimationSettings::default(),
            lease_ttl_us: 60_000_000,
            store_timeout_us: 100_000,
            on_store_unavailable: OnUnavailable::LocalShare,
            headers: HeaderSettings {
                ietf: true,
                dialect: true,
                max_policies: 10,
                expose_reservation: false,
            },
            forward_upstream_headers: false,
        }
    }
}

fn get<'a>(v: Option<&'a Value>, path: &[&str]) -> Option<&'a Value> {
    let mut cur = v?;
    for p in path {
        cur = cur.get(p)?;
    }
    Some(cur)
}

fn us_of(v: Option<&Value>) -> Option<u64> {
    let ns = antirion_config::scalar::duration_nanos(v?.as_str()?)?;
    u64::try_from(ns / 1_000).ok()
}

impl LimitsSettings {
    /// Reads the top-level `limits` object; absent fields take their defaults.
    #[must_use]
    pub fn from_value(v: Option<&Value>) -> LimitsSettings {
        let d = LimitsSettings::default();
        let b = |p: &[&str], dflt: bool| get(v, p).and_then(Value::as_bool).unwrap_or(dflt);
        let u = |p: &[&str], dflt: u64| get(v, p).and_then(Value::as_u64).unwrap_or(dflt);
        let s = |p: &[&str]| get(v, p).and_then(Value::as_str);
        let e = &d.estimation;
        LimitsSettings {
            enabled: b(&["enabled"], true),
            algorithm: s(&["algorithm"]).unwrap_or("gcra").to_owned(),
            estimation: EstimationSettings {
                input: match s(&["estimation", "input"]) {
                    Some("bytes") => InputEstimator::Bytes,
                    Some("upper_bound") => InputEstimator::UpperBound,
                    _ => InputEstimator::Auto,
                },
                bytes_per_token: s(&["estimation", "bytes_per_token"])
                    .and_then(|t| Dec::parse(t).ok())
                    .filter(|d| !d.is_zero())
                    .unwrap_or(e.bytes_per_token),
                image_tokens_default: u(&["estimation", "image_tokens_default"], e.image_tokens_default),
                audio_tokens_default: u(&["estimation", "audio_tokens_default"], e.audio_tokens_default),
                video_tokens_default: u(&["estimation", "video_tokens_default"], e.video_tokens_default),
                output: match s(&["estimation", "output"]) {
                    Some("clamp") => OutputMode::Clamp,
                    Some("optimistic") => OutputMode::Optimistic,
                    _ => OutputMode::Strict,
                },
                default_max_output_tokens: u(
                    &["estimation", "default_max_output_tokens"],
                    e.default_max_output_tokens,
                ),
                output_cap: get(v, &["estimation", "output_cap"]).and_then(Value::as_u64),
                max_output_tokens_ceiling: u(
                    &["estimation", "max_output_tokens_ceiling"],
                    e.max_output_tokens_ceiling,
                ),
            },
            lease_ttl_us: us_of(get(v, &["lease_ttl"])).unwrap_or(d.lease_ttl_us),
            store_timeout_us: us_of(get(v, &["store", "timeout"])).unwrap_or(d.store_timeout_us),
            on_store_unavailable: s(&["on_store_unavailable"])
                .and_then(OnUnavailable::parse)
                .unwrap_or(d.on_store_unavailable),
            headers: HeaderSettings {
                ietf: b(&["headers", "ietf"], true),
                dialect: b(&["headers", "dialect"], true),
                max_policies: usize::try_from(u(&["headers", "max_policies"], 10)).unwrap_or(10),
                expose_reservation: b(&["headers", "expose_reservation"], false),
            },
            forward_upstream_headers: b(&["forward_upstream_headers"], false),
        }
    }
}

/// The top-level `budgets` settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetsSettings {
    pub expose_headers: bool,
    pub charge_auxiliary: bool,
    pub on_store_unavailable: OnUnavailable,
}

impl BudgetsSettings {
    #[must_use]
    pub fn from_value(v: Option<&Value>) -> BudgetsSettings {
        BudgetsSettings {
            expose_headers: get(v, &["expose_headers"])
                .and_then(Value::as_bool)
                .unwrap_or(false),
            charge_auxiliary: get(v, &["charge_auxiliary"])
                .and_then(Value::as_bool)
                .unwrap_or(false),
            on_store_unavailable: get(v, &["on_store_unavailable"])
                .and_then(Value::as_str)
                .and_then(OnUnavailable::parse)
                .unwrap_or(OnUnavailable::FailClosed),
        }
    }
}

/// What an unpriced deployment costs.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OnUnpriced {
    /// Refuse requests with an applicable USD budget (403 `model_unpriced`).
    RejectIfBudgeted,
    /// Cost 0, status `unpriced`.
    Zero,
}

/// The top-level `pricing` settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PricingSettings {
    pub bundled: bool,
    pub on_unpriced: OnUnpriced,
    pub report_in_body: bool,
    /// Multipliers by level (`organizations`, `teams`, `projects`, `keys`) and name.
    pub multipliers: BTreeMap<String, BTreeMap<String, Dec>>,
    pub tools: BTreeMap<String, Dec>,
    pub agents: BTreeMap<String, Dec>,
    pub models: BTreeMap<String, Price>,
}

impl PricingSettings {
    pub fn from_value(v: Option<&Value>) -> Result<PricingSettings, DeclError> {
        let mut multipliers = BTreeMap::new();
        for level in ["organizations", "teams", "projects", "keys"] {
            let mut m = BTreeMap::new();
            for (name, d) in get(v, &["multipliers", level])
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
            {
                let dec = d
                    .as_str()
                    .and_then(|s| Dec::parse(s).ok())
                    .ok_or_else(|| field(&format!("pricing.multipliers.{level}.{name}"), "not a decimal"))?;
                m.insert(name.clone(), dec);
            }
            multipliers.insert(level.to_owned(), m);
        }
        let per_call = |key: &str| -> Result<BTreeMap<String, Dec>, DeclError> {
            let mut m = BTreeMap::new();
            for (name, p) in get(v, &[key]).and_then(Value::as_object).into_iter().flatten() {
                let dec = p
                    .get("per_call")
                    .and_then(Value::as_str)
                    .and_then(|s| Dec::parse(s).ok())
                    .ok_or_else(|| field(&format!("pricing.{key}.{name}.per_call"), "not a decimal"))?;
                m.insert(name.clone(), dec);
            }
            Ok(m)
        };
        let mut models = BTreeMap::new();
        for (name, p) in get(v, &["models"])
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            models.insert(name.clone(), Price::parse(p)?);
        }
        Ok(PricingSettings {
            bundled: get(v, &["catalog"]).and_then(Value::as_str) != Some("none"),
            on_unpriced: match get(v, &["on_unpriced"]).and_then(Value::as_str) {
                Some("zero") => OnUnpriced::Zero,
                _ => OnUnpriced::RejectIfBudgeted,
            },
            report_in_body: get(v, &["report_in_body"])
                .and_then(Value::as_bool)
                .unwrap_or(false),
            multipliers,
            tools: per_call("tools")?,
            agents: per_call("agents")?,
            models,
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn settings() {
        let l = LimitsSettings::from_value(Some(
            &json!({"lease_ttl": "6s", "headers": {"expose_reservation": true},
            "estimation": {"input": "upper_bound", "bytes_per_token": "4"}}),
        ));
        assert_eq!(l.lease_ttl_us, 6_000_000);
        assert!(l.headers.expose_reservation);
        assert_eq!(l.estimation.input, InputEstimator::UpperBound);
        assert_eq!(l.estimation.bytes_per_token, Dec::parse("4").unwrap());
        let p = PricingSettings::from_value(Some(
            &json!({"catalog": "none", "multipliers": {"teams": {"t": "0.5"}},
            "tools": {"search/*": {"per_call": "0.01"}}, "models": {"gpt-4o": {"input": "2.50"}}}),
        ))
        .unwrap();
        assert!(!p.bundled);
        assert_eq!(p.multipliers["teams"]["t"], Dec::parse("0.5").unwrap());
        assert!(p.models.contains_key("gpt-4o"));
    }
}
