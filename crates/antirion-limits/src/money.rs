//! Exact decimals: config decimals as fixed-point integers, money in nano-dollars. No float ever
//! touches an amount.

use std::fmt;

/// Fractional digits a config decimal may carry.
pub const DEC_DIGITS: u32 = 12;
/// `10^DEC_DIGITS`: the scale of [`Dec`].
pub const DEC_ONE: u128 = 1_000_000_000_000;

/// A non-negative config decimal (a rate, a weight, a multiplier, `bytes_per_token`), stored as
/// its value times 10¹².
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Dec(pub u128);

/// A decimal string that is not `^[0-9]+(\.[0-9]{1,12})?$`, or one too large to hold.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a decimal with at most {max} fractional digits: {text:?}")]
pub struct DecimalError {
    pub text: String,
    pub max: u32,
}

/// Parses `^[0-9]+(\.[0-9]{1,max})?$` into the value times `10^scale` (`max ≤ scale`).
fn parse_scaled(s: &str, max: u32, scale: u32) -> Result<u128, DecimalError> {
    let err = || DecimalError {
        text: s.to_owned(),
        max,
    };
    let (int, frac) = match s.split_once('.') {
        Some((i, f)) => (i, f),
        None => (s, ""),
    };
    let digits_ok = |t: &str| t.bytes().all(|b| b.is_ascii_digit());
    if int.is_empty() || !digits_ok(int) || !digits_ok(frac) {
        return Err(err());
    }
    if s.contains('.') && frac.is_empty() {
        return Err(err());
    }
    let frac_len = u32::try_from(frac.len()).map_err(|_| err())?;
    if frac_len > max {
        return Err(err());
    }
    let mut v: u128 = 0;
    for b in int.bytes().chain(frac.bytes()) {
        v = v
            .checked_mul(10)
            .and_then(|v| v.checked_add(u128::from(b - b'0')))
            .ok_or_else(err)?;
    }
    let pad = 10u128.checked_pow(scale - frac_len).ok_or_else(err)?;
    v.checked_mul(pad).ok_or_else(err)
}

impl Dec {
    pub const ZERO: Dec = Dec(0);
    pub const ONE: Dec = Dec(DEC_ONE);

    pub fn parse(s: &str) -> Result<Dec, DecimalError> {
        parse_scaled(s, DEC_DIGITS, DEC_DIGITS).map(Dec)
    }

    /// A ratio given as a JSON number (`alert_at`), read through its shortest decimal text.
    #[must_use]
    pub fn from_ratio(x: f64) -> Option<Dec> {
        if !(0.0..=1.0).contains(&x) {
            return None;
        }
        Dec::parse(&format!("{x:.12}")).ok()
    }

    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// `ceil(n × self)`, or `None` on overflow.
    #[must_use]
    pub fn mul_ceil(self, n: u128) -> Option<u128> {
        ceil_div(n.checked_mul(self.0)?, DEC_ONE)
    }
}

impl fmt::Display for Dec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&format_scaled(self.0, DEC_DIGITS))
    }
}

/// `ceil(n / d)`; `None` when `d` is zero.
#[must_use]
pub fn ceil_div(n: u128, d: u128) -> Option<u128> {
    let q = n.checked_div(d)?;
    if n.checked_rem(d)? == 0 {
        Some(q)
    } else {
        q.checked_add(1)
    }
}

/// `v / 10^scale` as a canonical decimal: no exponent, no trailing zeros, `"0"` for zero.
fn format_scaled(v: u128, scale: u32) -> String {
    if scale <= 19
        && let Ok(small) = u64::try_from(v)
    {
        let one = 10u64.pow(scale);
        return join_scaled(small / one, small % one, scale);
    }
    wide_scaled(v, scale)
}

fn wide_scaled(v: u128, scale: u32) -> String {
    let one = 10u128.pow(scale);
    let int = v / one;
    let frac = v % one;
    if frac == 0 {
        return int.to_string();
    }
    let width = usize::try_from(scale).unwrap_or(0);
    let digits = format!("{frac:0width$}");
    format!("{int}.{}", digits.trim_end_matches('0'))
}

/// [`format_scaled`] of a value that fits 64 bits, without 128-bit division or formatting.
fn join_scaled(int: u64, frac: u64, scale: u32) -> String {
    let mut out = int.to_string();
    if frac == 0 {
        return out;
    }
    let mut digits = [b'0'; 19];
    let width = usize::try_from(scale).unwrap_or(0).min(digits.len());
    let mut rest = frac;
    for d in digits.iter_mut().take(width).rev() {
        *d = b'0' + u8::try_from(rest % 10).unwrap_or(0);
        rest /= 10;
    }
    let kept = digits.get(..width).unwrap_or_default();
    let end = kept.iter().rposition(|d| *d != b'0').map_or(0, |i| i + 1);
    out.push('.');
    for d in kept.get(..end).unwrap_or_default() {
        out.push(char::from(*d));
    }
    out
}

/// Money in nano-dollars (10⁻⁹ USD). Negative values occur only in differences (credits, markups
/// below 1).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Nusd(pub i128);

impl Nusd {
    pub const ZERO: Nusd = Nusd(0);

    /// Parses a USD decimal string with at most 9 fractional digits (a budget amount).
    pub fn parse_usd(s: &str) -> Result<Nusd, DecimalError> {
        let v = parse_scaled(s, 9, 9)?;
        i128::try_from(v).map(Nusd).map_err(|_| DecimalError {
            text: s.to_owned(),
            max: 9,
        })
    }

    /// A USD decimal that may be negative (a credit).
    pub fn parse_signed_usd(s: &str) -> Result<Nusd, DecimalError> {
        match s.strip_prefix('-') {
            Some(rest) => Nusd::parse_usd(rest).map(|n| Nusd(-n.0)),
            None => Nusd::parse_usd(s),
        }
    }

    /// The canonical decimal USD string (no exponent, no trailing zeros, `"0"` for zero).
    #[must_use]
    pub fn to_usd_string(self) -> String {
        let text = format_scaled(self.0.unsigned_abs(), 9);
        if self.0 < 0 { format!("-{text}") } else { text }
    }

    /// The amount as a non-negative counter value (negative amounts clamp to zero).
    #[must_use]
    pub fn units(self) -> u128 {
        u128::try_from(self.0).unwrap_or(0)
    }

    #[must_use]
    pub fn from_units(v: u128) -> Nusd {
        Nusd(i128::try_from(v).unwrap_or(i128::MAX))
    }
}

impl fmt::Display for Nusd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_usd_string())
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        #[test]
        fn the_64_bit_decimal_agrees_with_the_wide_one(v in any::<u64>(), scale in 0u32..=19) {
            prop_assert_eq!(format_scaled(u128::from(v), scale), wide_scaled(u128::from(v), scale));
        }
    }

    #[test]
    fn decimals_parse_exactly() {
        assert_eq!(Dec::parse("1").unwrap(), Dec::ONE);
        assert_eq!(Dec::parse("0.15").unwrap().0, 150_000_000_000);
        assert_eq!(Dec::parse("0.000000000001").unwrap().0, 1);
        assert_eq!(Dec::parse("10000.00").unwrap().0, 10_000 * DEC_ONE);
        for bad in ["", ".5", "1.", "-1", "1e3", "0.0000000000001", " 1", "1,5"] {
            assert!(Dec::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn usd_strings_are_canonical() {
        assert_eq!(Nusd::parse_usd("3.50").unwrap(), Nusd(3_500_000_000));
        assert_eq!(Nusd(3_500_000_000).to_usd_string(), "3.5");
        assert_eq!(Nusd(0).to_usd_string(), "0");
        assert_eq!(Nusd(1).to_usd_string(), "0.000000001");
        assert_eq!(Nusd(-1_780_000).to_usd_string(), "-0.00178");
        assert_eq!(Nusd(2_000_000_000).to_usd_string(), "2");
        assert!(Nusd::parse_usd("0.0000000001").is_err());
    }

    #[test]
    fn ratios_and_ceilings() {
        assert_eq!(Dec::from_ratio(0.8), Some(Dec(800_000_000_000)));
        assert_eq!(Dec::from_ratio(1.0), Some(Dec::ONE));
        assert_eq!(Dec::from_ratio(1.5), None);
        assert_eq!(Dec::parse("0.1").unwrap().mul_ceil(900), Some(90));
        assert_eq!(Dec::parse("0.1").unwrap().mul_ceil(901), Some(91));
        assert_eq!(ceil_div(7, 0), None);
        assert_eq!(Dec::parse("2.5").unwrap().to_string(), "2.5");
    }

    proptest! {
        #[test]
        fn usd_round_trip(v in 0i128..1_000_000_000_000_000_000) {
            let n = Nusd(v);
            prop_assert_eq!(Nusd::parse_usd(&n.to_usd_string()).unwrap(), n);
        }

        #[test]
        fn dec_round_trip(v in 0u128..u128::from(u64::MAX)) {
            let d = Dec(v);
            prop_assert_eq!(Dec::parse(&d.to_string()).unwrap(), d);
        }
    }
}
