//! The cost formula: per-class components rounded up to whole nUSD, unit charges, multipliers,
//! and the worst-case cost reserved at admission.

use crate::money::{Dec, Nusd, ceil_div};
use crate::price::{Price, Rate, Rates};
use crate::usage::{Class, Units, Usage};
use crate::weights::Incurred;

/// Atto-dollars (10⁻¹⁸ USD) per pico-dollar rate unit times one unit charge.
const ATTO_PER_PICO: u128 = 1_000_000;
/// Atto-dollars per nUSD.
const ATTO_PER_NUSD: u128 = 1_000_000_000;

/// How exact a call's cost is.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum CostStatus {
    /// Every class with tokens had its own rate.
    Exact,
    /// A fallback rate priced some class.
    Approximate,
    /// Some usage came from the gateway's own count.
    Estimated,
    /// No price exists for the deployment.
    Unpriced,
}

impl CostStatus {
    #[must_use]
    pub fn parse(s: &str) -> Option<CostStatus> {
        [
            CostStatus::Exact,
            CostStatus::Approximate,
            CostStatus::Estimated,
            CostStatus::Unpriced,
        ]
        .into_iter()
        .find(|v| v.as_str() == s)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            CostStatus::Exact => "exact",
            CostStatus::Approximate => "approximate",
            CostStatus::Estimated => "estimated",
            CostStatus::Unpriced => "unpriced",
        }
    }
}

/// The reported cost groups. They sum exactly to the cost; `markup` makes them sum to the billed
/// amount.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct CostComponents {
    pub input: Nusd,
    pub cache_read: Nusd,
    pub cache_write: Nusd,
    pub output: Nusd,
    pub reasoning: Nusd,
    pub audio: Nusd,
    pub image: Nusd,
    pub video: Nusd,
    pub unit: Nusd,
    pub markup: Nusd,
}

impl CostComponents {
    /// The groups in reporting order, with their names.
    #[must_use]
    pub fn named(&self) -> [(&'static str, Nusd); 10] {
        [
            ("input", self.input),
            ("cache_read", self.cache_read),
            ("cache_write", self.cache_write),
            ("reasoning", self.reasoning),
            ("output", self.output),
            ("audio", self.audio),
            ("image", self.image),
            ("video", self.video),
            ("unit", self.unit),
            ("markup", self.markup),
        ]
    }
}

/// The cost of one upstream call.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Cost {
    pub components: CostComponents,
    /// The sum of the rounded components.
    pub total: Nusd,
    /// `ceil(total × multiplier)`: what budgets and `x-antirion-cost` use.
    pub billed: Nusd,
    pub status: CostStatus,
}

impl Cost {
    /// A free call (a cache hit, a failure without usage).
    #[must_use]
    pub fn zero() -> Cost {
        Cost {
            components: CostComponents::default(),
            total: Nusd::ZERO,
            billed: Nusd::ZERO,
            status: CostStatus::Exact,
        }
    }

    /// The same cost, marked as computed from the gateway's own usage count.
    #[must_use]
    pub fn estimated(mut self) -> Cost {
        self.status = CostStatus::Estimated;
        self
    }
}

/// Arithmetic beyond 128 bits: the request is refused (admission) or its cost estimated (settle).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("cost arithmetic overflowed")]
pub struct CostOverflow;

fn mul(a: u128, b: u128) -> Result<u128, CostOverflow> {
    a.checked_mul(b).ok_or(CostOverflow)
}

fn add(a: u128, b: u128) -> Result<u128, CostOverflow> {
    a.checked_add(b).ok_or(CostOverflow)
}

fn nusd(atto: u128) -> Result<Nusd, CostOverflow> {
    let n = ceil_div(atto, ATTO_PER_NUSD).ok_or(CostOverflow)?;
    i128::try_from(n).map(Nusd).map_err(|_| CostOverflow)
}

fn unit_atto(rates: &Rates, units: &Units, per_call: Option<Dec>) -> Result<(u128, bool), CostOverflow> {
    let rate = |r: Rate| rates.get(r).unwrap_or(Dec::ZERO).0;
    let mut atto = mul(
        mul(u128::from(units.requests), rate(Rate::PerRequest))?,
        ATTO_PER_PICO,
    )?;
    atto = add(atto, mul(per_call.unwrap_or(Dec::ZERO).0, ATTO_PER_PICO)?)?;
    let mut approximate = false;
    for (key, n) in &units.images {
        match rates.per_image.get(key) {
            Some(p) => atto = add(atto, mul(mul(u128::from(*n), p.0)?, ATTO_PER_PICO)?)?,
            None => approximate |= *n > 0,
        }
    }
    atto = add(
        atto,
        mul(
            mul(u128::from(units.audio_ms_in), rate(Rate::PerAudioSecondIn))?,
            1_000,
        )?,
    )?;
    atto = add(
        atto,
        mul(
            mul(u128::from(units.characters_in), rate(Rate::Per1kCharactersIn))?,
            1_000,
        )?,
    )?;
    atto = add(
        atto,
        mul(mul(u128::from(units.pages), rate(Rate::PerPage))?, ATTO_PER_PICO)?,
    )?;
    atto = add(
        atto,
        mul(
            mul(u128::from(units.web_searches), rate(Rate::PerWebSearch))?,
            ATTO_PER_PICO,
        )?,
    )?;
    Ok((atto, approximate))
}

/// `ceil(cost × multiplier)`.
pub fn billed(total: Nusd, multiplier: Dec) -> Result<Nusd, CostOverflow> {
    let n = ceil_div(mul(total.units(), multiplier.0)?, crate::money::DEC_ONE).ok_or(CostOverflow)?;
    i128::try_from(n).map(Nusd).map_err(|_| CostOverflow)
}

/// The cost of one upstream call with usage `u` and unit counts `units` at `price`. `per_call` is
/// the price of a priced tool or agent call.
pub fn cost(
    price: &Price,
    u: &Usage,
    units: &Units,
    per_call: Option<Dec>,
    multiplier: Dec,
) -> Result<Cost, CostOverflow> {
    let rates = price.rates_for(u.context());
    let mut c = CostComponents::default();
    let mut approximate = false;
    for (class, n) in u.classes().iter() {
        if n == 0 {
            continue;
        }
        let (rate, fell_back) = rates.resolve(Rate::of_class(class));
        approximate |= fell_back;
        let part = nusd(mul(u128::from(n), rate.0)?)?;
        let slot = match class {
            Class::InputText => &mut c.input,
            Class::InputCacheRead => &mut c.cache_read,
            Class::InputCacheWrite5m | Class::InputCacheWrite1h => &mut c.cache_write,
            Class::OutputText => &mut c.output,
            Class::OutputReasoning => &mut c.reasoning,
            Class::InputAudioUncached | Class::OutputAudio => &mut c.audio,
            Class::InputImageUncached | Class::OutputImage => &mut c.image,
            Class::InputVideoUncached => &mut c.video,
        };
        slot.0 = slot.0.checked_add(part.0).ok_or(CostOverflow)?;
    }
    let (atto, images_approx) = unit_atto(&rates, units, per_call)?;
    approximate |= images_approx;
    c.unit = nusd(atto)?;
    let total = [
        c.input,
        c.cache_read,
        c.cache_write,
        c.output,
        c.reasoning,
        c.audio,
        c.image,
        c.video,
        c.unit,
    ]
    .iter()
    .try_fold(0i128, |acc, n| acc.checked_add(n.0))
    .map(Nusd)
    .ok_or(CostOverflow)?;
    let billed = billed(total, multiplier)?;
    c.markup = Nusd(billed.0 - total.0);
    Ok(Cost {
        components: c,
        total,
        billed,
        status: if approximate {
            CostStatus::Approximate
        } else {
            CostStatus::Exact
        },
    })
}

/// Unit charges known before the call.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FixedUnits {
    /// Images to generate (`n`) and their `"<size>/<quality>"`.
    pub images: Option<(u64, String)>,
    /// Characters to synthesise.
    pub characters_in: u64,
    /// The price of a priced tool or agent call.
    pub per_call: Option<Dec>,
}

/// The worst-case billed cost of a request on one deployment, in nUSD, before the hedging factor:
/// `ceil(multiplier × (per_request + fixed + (E_in·Rin + E_out·Rout) / 10⁶))`, with the tier chosen
/// by `E_in` and the rates the largest over the classes the request can incur.
pub fn worst_case(
    price: &Price,
    e_in: u64,
    e_out: u64,
    incurs: &Incurred,
    fixed: &FixedUnits,
    multiplier: Dec,
) -> Result<u128, CostOverflow> {
    let rates = price.rates_for(e_in);
    let max_rate = |classes: &[Class]| {
        classes
            .iter()
            .map(|c| rates.resolve(Rate::of_class(*c)).0)
            .max()
            .unwrap_or(Dec::ZERO)
    };
    let r_in = max_rate(&incurs.input);
    let r_out = max_rate(&incurs.output);
    let mut units = Units {
        requests: 1,
        characters_in: fixed.characters_in,
        ..Units::default()
    };
    if let Some((n, key)) = &fixed.images {
        units.images.insert(key.clone(), *n);
    }
    let (unit, _) = unit_atto(&rates, &units, fixed.per_call)?;
    let tokens = add(mul(u128::from(e_in), r_in.0)?, mul(u128::from(e_out), r_out.0)?)?;
    let atto = add(unit, tokens)?;
    ceil_div(
        mul(atto, multiplier.0)?,
        mul(ATTO_PER_NUSD, crate::money::DEC_ONE)?,
    )
    .ok_or(CostOverflow)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use serde_json::json;

    use super::*;
    use crate::usage::{Convention, normalize};

    fn price(v: serde_json::Value) -> Price {
        Price::parse(&v).unwrap()
    }

    fn calls(p: &Price, u: &Usage) -> Cost {
        cost(p, u, &Units::default(), None, Dec::ONE).unwrap()
    }

    #[test]
    fn anthropic_example_and_fallback_status() {
        let p = price(json!({"input": "1.0", "cache_read": "0.5", "cache_write_5m": "0.3", "output": "2.0"}));
        let (u, _) = normalize(
            Convention::Anthropic,
            &json!({"input_tokens": 600, "cache_read_input_tokens": 300, "cache_creation_input_tokens": 100, "output_tokens": 500}),
        )
        .unwrap();
        let c = calls(&p, &u);
        assert_eq!(c.billed.to_usd_string(), "0.00178");
        assert_eq!(c.status, CostStatus::Exact);
        let p2 = price(json!({"input": "1.0", "output": "2.0"}));
        let (u2, _) = normalize(
            Convention::OpenAiChat,
            &json!({"prompt_tokens": 1000, "prompt_tokens_details": {"cached_tokens": 300}, "completion_tokens": 500}),
        )
        .unwrap();
        let c2 = calls(&p2, &u2);
        assert_eq!(c2.billed.to_usd_string(), "0.002");
        assert_eq!(c2.status, CostStatus::Approximate);
        assert_eq!(calls(&p2, &Usage::default()).billed, Nusd::ZERO);
    }

    #[test]
    fn rounding_examples() {
        let one = |rate: &str, n: u64| {
            calls(
                &price(json!({"input": rate})),
                &Usage {
                    input: n,
                    ..Usage::default()
                },
            )
            .total
            .0
        };
        assert_eq!(one("0.15", 1), 150);
        assert_eq!(one("0.0001", 1), 1);
        assert_eq!(one("1.00", 333), 333_000);
    }

    #[test]
    fn tiers_apply_to_every_token() {
        let p = price(
            json!({"input": "1.00", "output": "0", "tiers": [{"above_input_tokens": 200000, "input": "2.00"}]}),
        );
        let at = |n| {
            calls(
                &p,
                &Usage {
                    input: n,
                    ..Usage::default()
                },
            )
            .billed
            .to_usd_string()
        };
        assert_eq!(at(200_000), "0.2");
        assert_eq!(at(200_001), "0.400002");
    }

    #[test]
    fn units_and_multipliers() {
        let p = price(json!({"input": "0", "output": "0", "per_request": "0.02",
            "per_image": {"1024x1024/hd": "0.08"}, "per_audio_second_in": "0.0001"}));
        let mut units = Units {
            requests: 1,
            ..Units::default()
        };
        assert_eq!(
            cost(&p, &Usage::default(), &units, None, Dec::ONE)
                .unwrap()
                .billed
                .to_usd_string(),
            "0.02"
        );
        units.requests = 0;
        units.images.insert("1024x1024/hd".into(), 2);
        assert_eq!(
            cost(&p, &Usage::default(), &units, None, Dec::ONE)
                .unwrap()
                .billed
                .to_usd_string(),
            "0.16"
        );
        let audio = Units {
            audio_ms_in: 12_500,
            ..Units::default()
        };
        assert_eq!(
            cost(&p, &Usage::default(), &audio, None, Dec::ONE)
                .unwrap()
                .billed
                .to_usd_string(),
            "0.00125"
        );
        let half = Dec::parse("0.5").unwrap();
        let c = cost(&p, &Usage::default(), &units, None, half).unwrap();
        assert_eq!(c.billed.to_usd_string(), "0.08");
        assert_eq!(c.components.markup.to_usd_string(), "-0.08");
        assert_eq!(
            cost(&p, &Usage::default(), &units, None, Dec::ZERO)
                .unwrap()
                .billed,
            Nusd::ZERO
        );
    }

    #[test]
    fn worst_case_reservation() {
        let p = price(json!({"input": "0", "output": "10000.00"}));
        let rho = worst_case(
            &p,
            50,
            100,
            &Incurred::default(),
            &FixedUnits::default(),
            Dec::ONE,
        )
        .unwrap();
        assert_eq!(Nusd::from_units(rho).to_usd_string(), "1");
        let free = worst_case(
            &p,
            50,
            100,
            &Incurred::default(),
            &FixedUnits::default(),
            Dec::ZERO,
        )
        .unwrap();
        assert_eq!(free, 0);
        let img = price(json!({"per_image": {"1024x1024/hd": "0.08"}}));
        let fixed = FixedUnits {
            images: Some((2, "1024x1024/hd".into())),
            ..FixedUnits::default()
        };
        let rho = worst_case(&img, 1, 0, &Incurred::default(), &fixed, Dec::ONE).unwrap();
        assert_eq!(Nusd::from_units(rho).to_usd_string(), "0.16");
    }

    proptest! {
        #[test]
        fn components_sum_to_billed(
            input in 0u64..10_000_000, read in 0u64..10_000_000, output in 0u64..10_000_000,
            reasoning in 0u64..10_000_000, rate_in in 0u64..100_000_000, rate_out in 0u64..100_000_000,
            mult in 0u64..3_000_000,
        ) {
            let fmt = |r: u64| format!("{}.{:06}", r / 1_000_000, r % 1_000_000);
            let p = price(json!({"input": fmt(rate_in), "output": fmt(rate_out), "cache_read": "0.1"}));
            let u = Usage { input, input_cache_read: read.min(input), output, output_reasoning: reasoning.min(output), ..Usage::default() };
            let m = Dec::parse(&fmt(mult)).unwrap();
            let c = cost(&p, &u, &Units::default(), None, m).unwrap();
            let sum: i128 = c.components.named().iter().map(|(_, n)| n.0).sum();
            prop_assert_eq!(sum, c.billed.0);
            prop_assert!(c.total.0 >= 0);
            if (input > 0 && rate_in > 0) || (output > 0 && rate_out > 0) {
                prop_assert!(c.total.0 >= 1);
            }
        }
    }
}
