//! Numbers and names as people read them in alert summaries and mails: money in dollars with cents
//! and thousands separators, counts with separators, ratios as percentages, and resources by their
//! display names where they have one. Machine-readable fields keep their exact decimals.

use antirion_limits::decl::BudgetUnit;
use antirion_limits::level::Level;
use serde_json::Value;

const NANO_PER_CENT: u128 = 10_000_000;

/// `n` with a comma between groups of three digits.
#[must_use]
pub fn grouped(n: u128) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// An amount of nano-dollars: `$2,067.44`, rounded half up to the cent; an amount under a cent
/// keeps two significant digits (`$0.0004`) so it does not read as zero.
#[must_use]
pub fn usd_nano(nano: u128) -> String {
    if nano > 0 && nano < NANO_PER_CENT / 2 {
        let frac = format!("{nano:09}");
        let first = frac.find(|c| c != '0').unwrap_or(0);
        let kept = frac.get(..(first + 2).min(9)).unwrap_or(&frac);
        return format!("$0.{}", kept.trim_end_matches('0'));
    }
    let cents = (nano + NANO_PER_CENT / 2) / NANO_PER_CENT;
    format!("${}.{:02}", grouped(cents / 100), cents % 100)
}

/// A dollar amount given as a float (a metric's value), signed.
#[must_use]
pub fn usd(v: f64) -> String {
    if !v.is_finite() {
        return v.to_string();
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let nano = (v.abs() * 1e9).round() as u128;
    let text = usd_nano(nano);
    if v < 0.0 && nano > 0 {
        format!("-{text}")
    } else {
        text
    }
}

/// A ratio as a percentage with at most one decimal: `0.072` is `7.2%`, `0.8` is `80%`.
#[must_use]
pub fn percent(r: f64) -> String {
    let p = (r * 1000.0).round() / 10.0;
    let text = format!("{p:.1}");
    format!("{}%", text.strip_suffix(".0").unwrap_or(&text))
}

/// A float shown as a whole number with separators (counts, milliseconds).
#[must_use]
pub fn whole(v: f64) -> String {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = v.abs().round() as u128;
    if v < 0.0 && n > 0 {
        format!("-{}", grouped(n))
    } else {
        grouped(n)
    }
}

/// A number whose unit is unknown: at most two decimals, trailing zeros dropped, with separators.
#[must_use]
pub fn number(v: f64) -> String {
    if !v.is_finite() {
        return v.to_string();
    }
    let text = format!("{:.2}", v.abs());
    let (int, frac) = text.split_once('.').unwrap_or((&text, ""));
    let frac = frac.trim_end_matches('0');
    let grouped = int.parse::<u128>().map_or_else(|_| int.to_owned(), grouped);
    let sign = if v < 0.0 && (int != "0" || !frac.is_empty()) {
        "-"
    } else {
        ""
    };
    if frac.is_empty() {
        format!("{sign}{grouped}")
    } else {
        format!("{sign}{grouped}.{frac}")
    }
}

/// A budget amount in its unit: dollars, or a token count.
#[must_use]
pub fn budget_amount(unit: &BudgetUnit, v: u128) -> String {
    match unit {
        BudgetUnit::Usd => usd_nano(v),
        BudgetUnit::Tokens(_) => format!("{} tokens", grouped(v)),
    }
}

/// The collection a resource kind lives in.
pub(crate) fn collection(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "organization" => "organizations",
        "team" => "teams",
        "project" => "projects",
        "key" => "keys",
        "user" => "users",
        "route" => "routes",
        "model" => "models",
        "provider" => "providers",
        "deployment" => "deployments",
        _ => return None,
    })
}

/// A resource's display name when the document gives it one, else its name.
#[must_use]
pub fn display_name<'a>(doc: &'a Value, kind: &str, name: &'a str) -> &'a str {
    collection(kind)
        .and_then(|c| doc.get(c))
        .and_then(|c| c.get(name))
        .and_then(|r| r.get("display_name"))
        .and_then(Value::as_str)
        .filter(|d| !d.trim().is_empty())
        .unwrap_or(name)
}

/// A forecast alert's summary, dated: `Budget for team Search (monthly) was projected on 2026-10-16 to
/// spend $2,067.44 of $1,200.00 this period`. `at` is the RFC 3339 time it fired.
#[must_use]
pub fn forecast_summary(label: &str, projected: &str, amount: &str, at: &str) -> String {
    let day = at.get(..10).unwrap_or(at);
    format!("{label} was projected on {day} to spend {projected} of {amount} this period")
}

/// A budget as a sentence names it: `Budget for project Zendesk assistant (monthly)`.
#[must_use]
pub fn budget_label(doc: &Value, level: Level, owner: &str, name: &str) -> String {
    match level {
        Level::Gateway => format!("Gateway budget ({name})"),
        _ => {
            let kind = level.as_str();
            format!("Budget for {kind} {} ({name})", display_name(doc, kind, owner))
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn numbers_without_a_unit_keep_two_decimals_and_never_a_negative_zero() {
        assert_eq!(number(2_067.443_506_045), "2,067.44");
        assert_eq!(number(1_200.0), "1,200");
        assert_eq!(number(0.5), "0.5");
        assert_eq!(number(-0.001), "0");
        assert_eq!(number(-3.256), "-3.26");
    }

    #[test]
    fn money_has_cents_and_separators() {
        assert_eq!(usd_nano(2_067_443_506_045), "$2,067.44");
        assert_eq!(usd_nano(1_200_000_000_000), "$1,200.00");
        assert_eq!(usd_nano(0), "$0.00");
        assert_eq!(usd_nano(5_000_000), "$0.01", "half a cent rounds up");
        assert_eq!(usd_nano(400_000), "$0.0004");
        assert_eq!(usd_nano(1_234_567), "$0.0012");
        assert_eq!(usd(1_234_567.891), "$1,234,567.89");
        assert_eq!(usd(-3.5), "-$3.50");
    }

    #[test]
    fn counts_and_ratios() {
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_000), "1,000");
        assert_eq!(grouped(1_200_000), "1,200,000");
        assert_eq!(whole(5012.25), "5,012");
        assert_eq!(percent(0.072), "7.2%");
        assert_eq!(percent(0.8), "80%");
        assert_eq!(percent(1.7228), "172.3%");
    }

    #[test]
    fn budgets_are_named_by_display_names() {
        let doc = json!({"teams": {"search": {"display_name": "Search"}}, "projects": {"zen": {}}});
        assert_eq!(
            budget_label(&doc, Level::Team, "search", "monthly"),
            "Budget for team Search (monthly)"
        );
        assert_eq!(
            budget_label(&doc, Level::Project, "zen", "monthly"),
            "Budget for project zen (monthly)"
        );
        assert_eq!(
            budget_label(&doc, Level::Gateway, "", "all"),
            "Gateway budget (all)"
        );
        assert_eq!(
            budget_amount(&BudgetUnit::Tokens(Default::default()), 1_200_000),
            "1,200,000 tokens"
        );
    }
}
