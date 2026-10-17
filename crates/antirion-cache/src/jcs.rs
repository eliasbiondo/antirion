//! The JSON Canonicalization Scheme (RFC 8785): members sorted by their UTF-16 code units, no
//! insignificant whitespace, strings with the minimal escapes, numbers spelled as ECMAScript prints
//! them. Integers that parsed exactly keep their decimal digits.

use std::cmp::Ordering;
use std::fmt::Write as _;

use serde_json::{Number, Value};

/// The canonical text of `v`.
#[must_use]
pub fn canonical(v: &Value) -> String {
    let mut out = String::new();
    write(v, &mut out);
    out
}

fn write(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => number(n, out),
        Value::String(s) => string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut members: Vec<(&String, &Value)> = map.iter().collect();
            members.sort_by(|a, b| utf16_order(a.0, b.0));
            out.push('{');
            for (i, (k, v)) in members.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                string(k, out);
                out.push(':');
                write(v, out);
            }
            out.push('}');
        }
    }
}

fn utf16_order(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

fn string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn number(n: &Number, out: &mut String) {
    if let Some(u) = n.as_u64() {
        let _ = write!(out, "{u}");
    } else if let Some(i) = n.as_i64() {
        let _ = write!(out, "{i}");
    } else if let Some(f) = n.as_f64() {
        es_number(f, out);
    }
}

/// ECMAScript `Number::toString` of a finite double.
fn es_number(f: f64, out: &mut String) {
    if f == 0.0 || !f.is_finite() {
        out.push('0');
        return;
    }
    if f < 0.0 {
        out.push('-');
    }
    // `{:e}` prints the shortest digits that round-trip, as `d[.ddd]e<exp>`.
    let sci = format!("{:e}", f.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let k = i64::try_from(digits.len()).unwrap_or(1);
    let n = exp.parse::<i64>().unwrap_or(0) + 1;
    if k <= n && n <= 21 {
        out.push_str(&digits);
        for _ in 0..(n - k) {
            out.push('0');
        }
    } else if 0 < n && n <= 21 {
        let (int, frac) = digits.split_at(usize::try_from(n).unwrap_or(0));
        out.push_str(int);
        out.push('.');
        out.push_str(frac);
    } else if -6 < n && n <= 0 {
        out.push_str("0.");
        for _ in 0..(-n) {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        let (first, rest) = digits.split_at(1.min(digits.len()));
        out.push_str(first);
        if !rest.is_empty() {
            out.push('.');
            out.push_str(rest);
        }
        let e = n - 1;
        let _ = write!(out, "e{}{}", if e < 0 { '-' } else { '+' }, e.abs());
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn num(text: &str) -> String {
        canonical(&serde_json::from_str::<Value>(text).unwrap())
    }

    #[test]
    fn numbers_follow_ecmascript() {
        for (input, want) in [
            ("1", "1"),
            ("1.0", "1"),
            ("1e0", "1"),
            ("0.5", "0.5"),
            ("5e-1", "0.5"),
            ("50e-2", "0.5"),
            ("-0.0", "0"),
            ("1e21", "1e+21"),
            ("1e20", "100000000000000000000"),
            ("123.456", "123.456"),
            ("0.000001", "0.000001"),
            ("0.0000001", "1e-7"),
            ("1.5e-9", "1.5e-9"),
            ("-2.5e30", "-2.5e+30"),
            ("9007199254740993", "9007199254740993"),
            ("-9007199254740993", "-9007199254740993"),
            ("333333333.33333329", "333333333.3333333"),
        ] {
            assert_eq!(num(input), want, "{input}");
        }
    }

    #[test]
    fn members_sort_by_utf16_and_strings_escape_minimally() {
        let v = json!({"b": 1, "a": [true, null], "\u{20ac}": "x", "\u{1f600}": "y", "c": "\u{1}\"\n/é"});
        assert_eq!(
            canonical(&v),
            "{\"a\":[true,null],\"b\":1,\"c\":\"\\u0001\\\"\\n/é\",\"\u{20ac}\":\"x\",\"\u{1f600}\":\"y\"}"
        );
    }
}
