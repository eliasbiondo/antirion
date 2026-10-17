//! The `length` check: bounds on characters (code points), words (Unicode word boundaries) or
//! estimated tokens, over the in-scope input segments together, or over each output choice's
//! content.

use antirion_config::{ConfigError, DetailCode, DocPath};
use serde_json::Value;
use unicode_segmentation::UnicodeSegmentation;

/// What is counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Chars,
    Words,
    Tokens,
}

impl Unit {
    const ALL: [(Unit, &'static str); 3] = [
        (Unit::Chars, "chars"),
        (Unit::Words, "words"),
        (Unit::Tokens, "tokens"),
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        Unit::ALL
            .iter()
            .find(|(u, _)| *u == self)
            .map_or("chars", |(_, s)| s)
    }
}

/// One bound: `min_<unit>` or `max_<unit>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bound {
    pub unit: Unit,
    pub min: Option<u64>,
    pub max: Option<u64>,
}

/// A `length` guardrail's bounds, and how tokens are estimated.
#[derive(Clone, Debug, PartialEq)]
pub struct LengthCheck {
    pub bounds: Vec<Bound>,
    /// `limits.estimation.bytes_per_token`, in thousandths.
    pub bytes_per_token_milli: u64,
}

/// `limits.estimation.bytes_per_token`'s default.
const DEFAULT_BYTES_PER_TOKEN_MILLI: u64 = 3_000;

/// Thousandths of a positive decimal string (`"3.0"` → 3000).
fn milli(s: &str) -> Option<u64> {
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    let whole: u64 = whole.parse().ok()?;
    let frac: String = frac.chars().chain(std::iter::repeat('0')).take(3).collect();
    let frac: u64 = frac.parse().ok()?;
    whole.checked_mul(1000)?.checked_add(frac).filter(|n| *n > 0)
}

impl LengthCheck {
    /// The check of a `length` block; a bound whose minimum exceeds its maximum is an error.
    pub(crate) fn compile(
        block: Option<&Value>,
        doc: &Value,
        at: &DocPath,
        errors: &mut Vec<ConfigError>,
    ) -> LengthCheck {
        let get = |name: String| block.and_then(|b| b.get(&name)).and_then(Value::as_u64);
        let mut bounds = Vec::new();
        for (unit, name) in Unit::ALL {
            let (min, max) = (get(format!("min_{name}")), get(format!("max_{name}")));
            if let (Some(lo), Some(hi)) = (min, max)
                && lo > hi
            {
                errors.push(ConfigError::new(
                    &at.key("length").key(&format!("min_{name}")),
                    DetailCode::OutOfRange,
                    format!("min_{name} must not exceed max_{name}"),
                ));
            }
            if min.is_some() || max.is_some() {
                bounds.push(Bound { unit, min, max });
            }
        }
        let bytes_per_token_milli = doc
            .pointer("/limits/estimation/bytes_per_token")
            .and_then(Value::as_str)
            .and_then(milli)
            .unwrap_or(DEFAULT_BYTES_PER_TOKEN_MILLI);
        LengthCheck {
            bounds,
            bytes_per_token_milli,
        }
    }

    /// The count of `unit` in `texts` together.
    #[must_use]
    pub fn count(&self, unit: Unit, texts: &[&str]) -> u64 {
        let n: usize = match unit {
            Unit::Chars => texts.iter().map(|t| t.chars().count()).sum(),
            Unit::Words => texts.iter().map(|t| t.unicode_words().count()).sum(),
            Unit::Tokens => {
                let bytes = texts
                    .iter()
                    .map(|t| t.len() as u64)
                    .fold(0u64, u64::saturating_add);
                let tokens = bytes
                    .saturating_mul(1000)
                    .div_ceil(self.bytes_per_token_milli.max(1));
                return tokens;
            }
        };
        u64::try_from(n).unwrap_or(u64::MAX)
    }

    /// The first bound `texts` breaks, as `<unit> <count>`, or `None` when every bound holds.
    #[must_use]
    pub fn breach(&self, texts: &[&str]) -> Option<String> {
        self.bounds.iter().find_map(|b| {
            let n = self.count(b.unit, texts);
            let out = b.min.is_some_and(|m| n < m) || b.max.is_some_and(|m| n > m);
            out.then(|| format!("{} {n}", b.unit.as_str()))
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn check(block: Value) -> LengthCheck {
        let mut errors = Vec::new();
        let c = LengthCheck::compile(Some(&block), &json!({}), &DocPath::root(), &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        c
    }

    #[test]
    fn units_count_as_specified() {
        let c = check(json!({"min_words": 1}));
        assert_eq!(c.count(Unit::Chars, &["héllo", "!"]), 6);
        assert_eq!(
            c.count(
                Unit::Words,
                &["The quick (\"brown\") fox can't jump 32.3 feet, right?"]
            ),
            9
        );
        assert_eq!(c.count(Unit::Tokens, &["abcdefg"]), 3, "ceil(7 / 3.0)");
        assert_eq!(milli("2.5"), Some(2_500));
        assert_eq!(milli("0"), None);
    }

    #[test]
    fn bounds_are_inclusive() {
        let c = check(json!({"min_words": 2, "max_chars": 10}));
        assert_eq!(c.breach(&["ok"]).as_deref(), Some("words 1"));
        assert_eq!(c.breach(&["ok then"]), None);
        assert_eq!(c.breach(&["ok then, fine"]).as_deref(), Some("chars 13"));
        let mut errors = Vec::new();
        LengthCheck::compile(
            Some(&json!({"min_chars": 5, "max_chars": 4})),
            &json!({}),
            &DocPath::root(),
            &mut errors,
        );
        assert_eq!(errors.len(), 1);
    }
}
