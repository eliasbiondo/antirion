//! Price objects: token rates in USD per 1M tokens, per-unit prices, tiers, field-wise merging and
//! the rate fallback chain.

use std::collections::BTreeMap;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::money::{Dec, DecimalError};
use crate::usage::Class;

/// A rate a price object may set, at its base or in a tier.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Rate {
    Input,
    Output,
    CacheRead,
    CacheWrite5m,
    CacheWrite1h,
    Reasoning,
    InputAudio,
    InputImage,
    InputVideo,
    OutputAudio,
    OutputImage,
    PerRequest,
    PerAudioSecondIn,
    Per1kCharactersIn,
    PerPage,
    PerWebSearch,
}

impl Rate {
    pub const ALL: [Rate; 16] = [
        Rate::Input,
        Rate::Output,
        Rate::CacheRead,
        Rate::CacheWrite5m,
        Rate::CacheWrite1h,
        Rate::Reasoning,
        Rate::InputAudio,
        Rate::InputImage,
        Rate::InputVideo,
        Rate::OutputAudio,
        Rate::OutputImage,
        Rate::PerRequest,
        Rate::PerAudioSecondIn,
        Rate::Per1kCharactersIn,
        Rate::PerPage,
        Rate::PerWebSearch,
    ];

    #[must_use]
    pub const fn field(self) -> &'static str {
        match self {
            Rate::Input => "input",
            Rate::Output => "output",
            Rate::CacheRead => "cache_read",
            Rate::CacheWrite5m => "cache_write_5m",
            Rate::CacheWrite1h => "cache_write_1h",
            Rate::Reasoning => "reasoning",
            Rate::InputAudio => "input_audio",
            Rate::InputImage => "input_image",
            Rate::InputVideo => "input_video",
            Rate::OutputAudio => "output_audio",
            Rate::OutputImage => "output_image",
            Rate::PerRequest => "per_request",
            Rate::PerAudioSecondIn => "per_audio_second_in",
            Rate::Per1kCharactersIn => "per_1k_characters_in",
            Rate::PerPage => "per_page",
            Rate::PerWebSearch => "per_web_search",
        }
    }

    /// The rate a pricing class is priced at.
    #[must_use]
    pub const fn of_class(c: Class) -> Rate {
        match c {
            Class::InputText => Rate::Input,
            Class::InputAudioUncached => Rate::InputAudio,
            Class::InputImageUncached => Rate::InputImage,
            Class::InputVideoUncached => Rate::InputVideo,
            Class::InputCacheRead => Rate::CacheRead,
            Class::InputCacheWrite5m => Rate::CacheWrite5m,
            Class::InputCacheWrite1h => Rate::CacheWrite1h,
            Class::OutputText => Rate::Output,
            Class::OutputReasoning => Rate::Reasoning,
            Class::OutputAudio => Rate::OutputAudio,
            Class::OutputImage => Rate::OutputImage,
        }
    }

    /// The next rate to use when this one is not set.
    #[must_use]
    pub const fn fallback(self) -> Option<Rate> {
        match self {
            Rate::CacheRead | Rate::CacheWrite5m | Rate::InputAudio | Rate::InputImage | Rate::InputVideo => {
                Some(Rate::Input)
            }
            Rate::CacheWrite1h => Some(Rate::CacheWrite5m),
            Rate::Reasoning | Rate::OutputAudio | Rate::OutputImage => Some(Rate::Output),
            _ => None,
        }
    }
}

/// The rates of one level of a price (its base, or one tier). Unset fields are `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rates {
    values: BTreeMap<Rate, Dec>,
    pub per_image: BTreeMap<String, Dec>,
}

impl Rates {
    #[must_use]
    pub fn get(&self, r: Rate) -> Option<Dec> {
        self.values.get(&r).copied()
    }

    pub fn set(&mut self, r: Rate, d: Dec) {
        self.values.insert(r, d);
    }

    /// `self` with every field `over` sets replacing it.
    #[must_use]
    pub fn overlaid(&self, over: &Rates) -> Rates {
        let mut out = self.clone();
        out.values.extend(over.values.iter().map(|(k, v)| (*k, *v)));
        out.per_image
            .extend(over.per_image.iter().map(|(k, v)| (k.clone(), *v)));
        out
    }

    /// The rate of `r` after the fallback chain, and whether a fallback was used. An exhausted
    /// chain prices at zero and counts as a fallback.
    #[must_use]
    pub fn resolve(&self, r: Rate) -> (Dec, bool) {
        let mut cur = r;
        let mut fell_back = false;
        loop {
            if let Some(d) = self.get(cur) {
                return (d, fell_back);
            }
            fell_back = true;
            match cur.fallback() {
                Some(next) => cur = next,
                None => return (Dec::ZERO, true),
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.values.is_empty() && self.per_image.is_empty()
    }

    /// The token rates multiplied by `scale` (rounded up); unit prices are kept.
    fn scaled(&self, scale: Dec) -> Rates {
        let mut out = self.clone();
        for (rate, value) in &mut out.values {
            if rate.is_token_rate() {
                *value = Dec(value.mul_ceil(scale.0).unwrap_or(u128::MAX));
            }
        }
        out
    }
}

impl Rate {
    /// Whether the rate prices tokens (per 1M), as opposed to a unit charge.
    #[must_use]
    pub const fn is_token_rate(self) -> bool {
        !matches!(
            self,
            Rate::PerRequest
                | Rate::PerAudioSecondIn
                | Rate::Per1kCharactersIn
                | Rate::PerPage
                | Rate::PerWebSearch
        )
    }
}

/// How the call a price is applied to was made: the service tier the upstream (or, failing that,
/// the request) named, and whether it was part of a Batches API job.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct CallTerms<'a> {
    pub service_tier: Option<&'a str>,
    pub batch: bool,
}

/// A tier: rates that replace the base ones when the context exceeds `above_input_tokens`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Tier {
    pub above_input_tokens: u64,
    pub rates: Rates,
}

/// A price object.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Price {
    pub base: Rates,
    /// By ascending threshold.
    pub tiers: Vec<Tier>,
    /// Service tier name → multiplier of the token rates.
    pub service_tiers: BTreeMap<String, Dec>,
    /// Multiplier of the token rates for usage settled from a Batches API job.
    pub batch_multiplier: Option<Dec>,
}

/// A price object whose content does not parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PriceError {
    #[error("{field}: {source}")]
    Decimal { field: String, source: DecimalError },
    #[error("{0}: expected an object")]
    Shape(String),
    #[error("tiers: thresholds must be strictly increasing")]
    TierOrder,
}

fn parse_rates(obj: &Map<String, Value>, prefix: &str) -> Result<Rates, PriceError> {
    let mut rates = Rates::default();
    let dec = |field: &str, v: &Value| -> Result<Dec, PriceError> {
        let s = v.as_str().unwrap_or_default();
        Dec::parse(s).map_err(|source| PriceError::Decimal {
            field: format!("{prefix}{field}"),
            source,
        })
    };
    for r in Rate::ALL {
        if let Some(v) = obj.get(r.field()) {
            rates.set(r, dec(r.field(), v)?);
        }
    }
    if let Some(images) = obj.get("per_image") {
        let map = images
            .as_object()
            .ok_or_else(|| PriceError::Shape(format!("{prefix}per_image")))?;
        for (k, v) in map {
            rates
                .per_image
                .insert(k.clone(), dec(&format!("per_image.{k}"), v)?);
        }
    }
    Ok(rates)
}

impl Price {
    /// Parses a price object of the config.
    pub fn parse(v: &Value) -> Result<Price, PriceError> {
        let obj = v.as_object().ok_or_else(|| PriceError::Shape("price".into()))?;
        let base = parse_rates(obj, "")?;
        let mut tiers = Vec::new();
        for (i, t) in obj
            .get("tiers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let tobj = t
                .as_object()
                .ok_or_else(|| PriceError::Shape(format!("tiers[{i}]")))?;
            let above = tobj
                .get("above_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            tiers.push(Tier {
                above_input_tokens: above,
                rates: parse_rates(tobj, &format!("tiers[{i}]."))?,
            });
        }
        if tiers
            .windows(2)
            .any(|w| matches!(w, [a, b] if a.above_input_tokens >= b.above_input_tokens))
        {
            return Err(PriceError::TierOrder);
        }
        let dec = |field: String, v: &Value| {
            Dec::parse(v.as_str().unwrap_or_default()).map_err(|source| PriceError::Decimal { field, source })
        };
        let mut service_tiers = BTreeMap::new();
        if let Some(m) = obj.get("service_tiers") {
            let m = m
                .as_object()
                .ok_or_else(|| PriceError::Shape("service_tiers".into()))?;
            for (k, v) in m {
                service_tiers.insert(k.clone(), dec(format!("service_tiers.{k}"), v)?);
            }
        }
        let batch_multiplier = obj
            .get("batch_multiplier")
            .map(|v| dec("batch_multiplier".into(), v))
            .transpose()?;
        Ok(Price {
            base,
            tiers,
            service_tiers,
            batch_multiplier,
        })
    }

    /// The price as it applies to a call made under `terms`: every token rate (base and tiers)
    /// multiplied by the service tier's multiplier and, for a batch, the batch multiplier.
    #[must_use]
    pub fn for_call(&self, terms: CallTerms<'_>) -> std::borrow::Cow<'_, Price> {
        let tier = terms
            .service_tier
            .and_then(|t| self.service_tiers.get(t))
            .copied();
        let batch = self.batch_multiplier.filter(|_| terms.batch);
        let scale = match (tier, batch) {
            (None, None) => return std::borrow::Cow::Borrowed(self),
            (Some(t), None) => t,
            (None, Some(b)) => b,
            (Some(t), Some(b)) => Dec(b.mul_ceil(t.0).unwrap_or(u128::MAX)),
        };
        let mut p = self.clone();
        p.base = p.base.scaled(scale);
        for t in &mut p.tiers {
            t.rates = t.rates.scaled(scale);
        }
        std::borrow::Cow::Owned(p)
    }

    /// Field-by-field merge: every field `self` sets wins over `lower`. Tiers merge by threshold.
    #[must_use]
    pub fn over(&self, lower: &Price) -> Price {
        let base = lower.base.overlaid(&self.base);
        let mut tiers: BTreeMap<u64, Rates> = lower
            .tiers
            .iter()
            .map(|t| (t.above_input_tokens, t.rates.clone()))
            .collect();
        for t in &self.tiers {
            let merged = tiers
                .get(&t.above_input_tokens)
                .map_or_else(|| t.rates.clone(), |l| l.overlaid(&t.rates));
            tiers.insert(t.above_input_tokens, merged);
        }
        let mut service_tiers = lower.service_tiers.clone();
        service_tiers.extend(self.service_tiers.iter().map(|(k, v)| (k.clone(), *v)));
        Price {
            base,
            tiers: tiers
                .into_iter()
                .map(|(above_input_tokens, rates)| Tier {
                    above_input_tokens,
                    rates,
                })
                .collect(),
            service_tiers,
            batch_multiplier: self.batch_multiplier.or(lower.batch_multiplier),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.base.is_empty() && self.tiers.is_empty()
    }

    /// The rates for a call whose context is `ctx`: the tier with the largest threshold strictly
    /// below `ctx`, over the base.
    #[must_use]
    pub fn rates_for(&self, ctx: u64) -> Rates {
        match self.tiers.iter().rev().find(|t| ctx > t.above_input_tokens) {
            Some(t) => self.base.overlaid(&t.rates),
            None => self.base.clone(),
        }
    }

    /// The price as config JSON (canonical field order), for the Admin API.
    #[must_use]
    pub fn to_json(&self) -> Value {
        fn rates_json(r: &Rates, into: &mut Map<String, Value>) {
            for rate in Rate::ALL {
                if let Some(d) = r.get(rate) {
                    into.insert(rate.field().into(), Value::String(d.to_string()));
                }
            }
            if !r.per_image.is_empty() {
                let m: Map<String, Value> = r
                    .per_image
                    .iter()
                    .map(|(k, v)| (k.clone(), Value::String(v.to_string())))
                    .collect();
                into.insert("per_image".into(), Value::Object(m));
            }
        }
        let mut out = Map::new();
        rates_json(&self.base, &mut out);
        if !self.tiers.is_empty() {
            let tiers = self
                .tiers
                .iter()
                .map(|t| {
                    let mut m = Map::new();
                    m.insert("above_input_tokens".into(), Value::from(t.above_input_tokens));
                    rates_json(&t.rates, &mut m);
                    Value::Object(m)
                })
                .collect();
            out.insert("tiers".into(), Value::Array(tiers));
        }
        if !self.service_tiers.is_empty() {
            let m: Map<String, Value> = self
                .service_tiers
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.to_string())))
                .collect();
            out.insert("service_tiers".into(), Value::Object(m));
        }
        if let Some(b) = self.batch_multiplier {
            out.insert("batch_multiplier".into(), Value::String(b.to_string()));
        }
        Value::Object(out)
    }

    /// A content hash of the effective price, recorded on every ledger line.
    #[must_use]
    pub fn version(&self) -> String {
        let text = self.to_json().to_string();
        let digest = Sha256::digest(text.as_bytes());
        let mut out = String::with_capacity(16);
        for b in digest.iter().take(8) {
            out.push_str(&format!("{b:02x}"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn d(s: &str) -> Dec {
        Dec::parse(s).unwrap()
    }

    #[test]
    fn parse_and_fallbacks() {
        let p =
            Price::parse(&json!({"input": "1.0", "output": "2.0", "per_image": {"1024x1024/hd": "0.08"}}))
                .unwrap();
        let r = p.rates_for(0);
        assert_eq!(r.resolve(Rate::Input), (d("1.0"), false));
        assert_eq!(r.resolve(Rate::CacheRead), (d("1.0"), true));
        assert_eq!(r.resolve(Rate::CacheWrite1h), (d("1.0"), true));
        assert_eq!(r.resolve(Rate::Reasoning), (d("2.0"), true));
        assert_eq!(r.resolve(Rate::PerPage), (Dec::ZERO, true));
        assert_eq!(r.per_image.get("1024x1024/hd"), Some(&d("0.08")));
        assert!(Price::parse(&json!({"input": "x"})).is_err());
    }

    #[test]
    fn tiers_are_exclusive_and_whole() {
        let p = Price::parse(&json!({"input": "1.00", "cache_read": "0.30",
            "tiers": [{"above_input_tokens": 200000, "input": "2.00", "cache_read": "0.60"}]}))
        .unwrap();
        assert_eq!(p.rates_for(200_000).get(Rate::Input), Some(d("1.00")));
        assert_eq!(p.rates_for(200_001).get(Rate::Input), Some(d("2.00")));
        assert_eq!(p.rates_for(200_001).get(Rate::CacheRead), Some(d("0.60")));
        assert!(
            Price::parse(&json!({"tiers": [{"above_input_tokens": 5}, {"above_input_tokens": 5}]})).is_err()
        );
    }

    #[test]
    fn merge_is_field_by_field() {
        let narrow = Price::parse(&json!({"input": "2.00"})).unwrap();
        let wide = Price::parse(&json!({"input": "2.50", "output": "10.00"})).unwrap();
        let m = narrow.over(&wide);
        assert_eq!(m.base.get(Rate::Input), Some(d("2.00")));
        assert_eq!(m.base.get(Rate::Output), Some(d("10.00")));
        assert_ne!(m.version(), wide.version());
        assert_eq!(m.version(), narrow.over(&wide).version());
        assert_eq!(m.to_json(), json!({"input": "2", "output": "10"}));
    }

    #[test]
    fn service_tiers_and_batches_scale_token_rates_only() {
        let p = Price::parse(&json!({"input": "1.00", "output": "4.00", "per_request": "0.01",
            "tiers": [{"above_input_tokens": 10, "input": "2.00"}],
            "service_tiers": {"priority": "2", "flex": "0.5"}, "batch_multiplier": "0.5"}))
        .unwrap();
        let tier = |t: Option<&str>, batch: bool| {
            p.for_call(CallTerms {
                service_tier: t,
                batch,
            })
            .into_owned()
        };
        let pr = tier(Some("priority"), false);
        assert_eq!(pr.base.get(Rate::Input), Some(d("2.00")));
        assert_eq!(pr.base.get(Rate::Output), Some(d("8.00")));
        assert_eq!(pr.base.get(Rate::PerRequest), Some(d("0.01")));
        assert_eq!(pr.rates_for(11).get(Rate::Input), Some(d("4.00")));
        assert_eq!(tier(Some("default"), false), p);
        assert_eq!(tier(None, true).base.get(Rate::Output), Some(d("2.00")));
        assert_eq!(tier(Some("flex"), true).base.get(Rate::Output), Some(d("1.00")));
        let narrow = Price::parse(&json!({"service_tiers": {"flex": "0.4"}})).unwrap();
        let m = narrow.over(&p);
        assert_eq!(m.service_tiers.get("flex"), Some(&d("0.4")));
        assert_eq!(m.service_tiers.get("priority"), Some(&d("2")));
        assert_eq!(m.batch_multiplier, Some(d("0.5")));
        assert_ne!(m.version(), p.version());
        assert!(Price::parse(&json!({"service_tiers": {"x": "-1"}})).is_err());
    }
}
