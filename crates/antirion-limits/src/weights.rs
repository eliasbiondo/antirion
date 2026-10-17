//! Token weights: how many units each pricing class charges to a `tokens` counter.

use std::collections::BTreeMap;

use crate::money::{DEC_ONE, Dec, ceil_div};
use crate::usage::{Class, Usage};

/// The weight of every pricing class, as decimals.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Weights([Dec; 11]);

/// A `tokens` preset name, or a weights map, that cannot be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WeightsError {
    #[error("unknown tokens preset {0:?}")]
    Preset(String),
    #[error("unknown pricing class {0:?}")]
    Class(String),
    #[error("weight of {class} is not a non-negative decimal")]
    Value { class: String },
}

impl Default for Weights {
    fn default() -> Self {
        Weights([Dec::ONE; 11])
    }
}

impl Weights {
    /// Builds the weights of a `tokens` preset (`total`, `input`, `output`, `weighted`); `weighted`
    /// reads `weights`, where an omitted class weighs 1.
    pub fn from_preset(preset: &str, weights: &BTreeMap<String, String>) -> Result<Weights, WeightsError> {
        let mut w = Weights::default();
        match preset {
            "total" => {}
            "input" | "output" => {
                let keep_input = preset == "input";
                for c in Class::ALL {
                    if c.is_input() != keep_input {
                        w.set(c, Dec::ZERO);
                    }
                }
            }
            "weighted" => {
                for (name, value) in weights {
                    let c = Class::parse(name).ok_or_else(|| WeightsError::Class(name.clone()))?;
                    let d = Dec::parse(value).map_err(|_| WeightsError::Value { class: name.clone() })?;
                    w.set(c, d);
                }
            }
            other => return Err(WeightsError::Preset(other.to_owned())),
        }
        Ok(w)
    }

    #[must_use]
    pub fn get(&self, c: Class) -> Dec {
        self.0.get(c as usize).copied().unwrap_or(Dec::ZERO)
    }

    fn set(&mut self, c: Class, d: Dec) {
        if let Some(slot) = self.0.get_mut(c as usize) {
            *slot = d;
        }
    }

    /// `ceil(Σ w_k · U_k)` over the pricing classes; `None` on overflow.
    #[must_use]
    pub fn charge(&self, u: &Usage) -> Option<u128> {
        let mut sum: u128 = 0;
        for (c, n) in u.classes().iter() {
            sum = sum.checked_add(u128::from(n).checked_mul(self.get(c).0)?)?;
        }
        ceil_div(sum, DEC_ONE)
    }

    /// Which side of the usage the weights count: `input` when only input classes weigh, `output`
    /// when only output classes do, otherwise `total`.
    #[must_use]
    pub fn side(&self) -> &'static str {
        let weighs = |input: bool| {
            Class::ALL
                .iter()
                .any(|c| c.is_input() == input && !self.get(*c).is_zero())
        };
        match (weighs(true), weighs(false)) {
            (true, false) => "input",
            (false, true) => "output",
            _ => "total",
        }
    }

    /// The largest weight among `classes`.
    #[must_use]
    pub fn max_of(&self, classes: &[Class]) -> Dec {
        classes.iter().map(|c| self.get(*c)).max().unwrap_or(Dec::ZERO)
    }

    /// The token reservation `ceil(H · (w_in · E_in + w_out · E_out))`, where the weights are the
    /// largest over the classes the request can incur; `None` on overflow.
    #[must_use]
    pub fn reservation(&self, incurs: &Incurred, e_in: u64, e_out: u64, hedge: u32) -> Option<u128> {
        let w_in = self.max_of(&incurs.input);
        let w_out = self.max_of(&incurs.output);
        let sum = u128::from(e_in)
            .checked_mul(w_in.0)?
            .checked_add(u128::from(e_out).checked_mul(w_out.0)?)?
            .checked_mul(u128::from(hedge))?;
        ceil_div(sum, DEC_ONE)
    }
}

/// The pricing classes a request can incur, known at admission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Incurred {
    pub input: Vec<Class>,
    pub output: Vec<Class>,
}

impl Default for Incurred {
    fn default() -> Self {
        Incurred::new(false, false, false, false, false, false)
    }
}

impl Incurred {
    /// Text input always, cache writes when the request carries (or the route injects) cache
    /// markers, a modality when it is present; text and reasoning output always, audio and image
    /// output when requested.
    #[must_use]
    pub fn new(
        cache_markers: bool,
        audio_in: bool,
        image_in: bool,
        video_in: bool,
        audio_out: bool,
        image_out: bool,
    ) -> Incurred {
        let mut input = vec![Class::InputText];
        if cache_markers {
            input.extend([Class::InputCacheWrite5m, Class::InputCacheWrite1h]);
        }
        for (on, c) in [
            (audio_in, Class::InputAudioUncached),
            (image_in, Class::InputImageUncached),
            (video_in, Class::InputVideoUncached),
        ] {
            if on {
                input.push(c);
            }
        }
        let mut output = vec![Class::OutputText, Class::OutputReasoning];
        if audio_out {
            output.push(Class::OutputAudio);
        }
        if image_out {
            output.push(Class::OutputImage);
        }
        Incurred { input, output }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn presets() {
        let u = Usage {
            input: 5,
            output: 60,
            ..Usage::default()
        };
        let none = BTreeMap::new();
        assert_eq!(Weights::from_preset("total", &none).unwrap().charge(&u), Some(65));
        assert_eq!(Weights::from_preset("input", &none).unwrap().charge(&u), Some(5));
        assert_eq!(
            Weights::from_preset("output", &none).unwrap().charge(&u),
            Some(60)
        );
        assert!(Weights::from_preset("bogus", &none).is_err());
        assert_eq!(Weights::from_preset("input", &none).unwrap().side(), "input");
        assert_eq!(Weights::from_preset("output", &none).unwrap().side(), "output");
        assert_eq!(Weights::from_preset("total", &none).unwrap().side(), "total");
    }

    #[test]
    fn weighted_cache_reads() {
        let w = Weights::from_preset("weighted", &map(&[("input_cache_read", "0.1")])).unwrap();
        let u = Usage {
            input: 1000,
            input_cache_read: 900,
            output: 10,
            ..Usage::default()
        };
        assert_eq!(w.charge(&u), Some(200));
        assert!(Weights::from_preset("weighted", &map(&[("nope", "1")])).is_err());
        assert!(Weights::from_preset("weighted", &map(&[("output_text", "-1")])).is_err());
    }

    #[test]
    fn reservation_uses_the_largest_weight() {
        let w = Weights::from_preset(
            "weighted",
            &map(&[("input_cache_write_5m", "1.25"), ("output_text", "0.5")]),
        )
        .unwrap();
        let marked = Incurred::new(true, false, false, false, false, false);
        assert_eq!(w.reservation(&marked, 100, 10, 1), Some(125 + 10));
        let plain = Incurred::default();
        assert_eq!(w.reservation(&plain, 100, 10, 2), Some(2 * 110));
        let out = Weights::from_preset("output", &BTreeMap::new()).unwrap();
        assert_eq!(out.reservation(&plain, 1000, 300, 1), Some(300));
    }
}
