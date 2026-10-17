//! Upstream hints: the time before which an upstream asked not to be called again, from the
//! header families in their order of precedence, or from a Gemini `RetryInfo` detail.

use std::time::Duration;

use antirion_core::UtcMicros;
use http::HeaderMap;
use serde_json::Value;

use crate::class::Class;

/// A value of `x-ratelimit-reset` below this many seconds is a delta; at or above, a Unix time.
const EPOCH_THRESHOLD: u64 = 2_592_000;

/// The hint of a failed attempt, if any source gives a valid one. `now` is the gateway clock.
#[must_use]
pub fn hint(headers: &HeaderMap, body: Option<&Value>, class: Class, now: UtcMicros) -> Option<UtcMicros> {
    let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::trim);
    if let Some(d) = get("retry-after-ms").and_then(parse_millis) {
        return Some(now.saturating_add(d));
    }
    if let Some(d) = get("x-ms-retry-after-ms").and_then(parse_millis) {
        return Some(now.saturating_add(d));
    }
    if let Some(at) = get("retry-after").and_then(|v| retry_after(v, now)) {
        return Some(at);
    }
    if class == Class::RateLimited {
        if let Some(at) = family(
            headers,
            "anthropic-ratelimit-",
            &["requests", "tokens", "input-tokens", "output-tokens"],
            |v| UtcMicros::parse_rfc3339(v).ok(),
        ) {
            return Some(at);
        }
        if let Some(at) = family(headers, "x-ratelimit-", &["requests", "tokens"], |v| {
            parse_go_duration(v).map(|d| now.saturating_add(d))
        }) {
            return Some(at);
        }
    }
    if let Some(v) = get("x-ratelimit-reset").and_then(|v| v.parse::<u64>().ok()) {
        if v < EPOCH_THRESHOLD {
            return Some(now.saturating_add(Duration::from_secs(v)));
        }
        let at = i64::try_from(v).ok()?.checked_mul(1_000_000)?;
        return Some(UtcMicros(at).max(now));
    }
    retry_info(body?).map(|d| now.saturating_add(d))
}

/// The reset-header family rule: the latest reset among the exhausted limits, else the earliest.
fn family(
    headers: &HeaderMap,
    prefix: &str,
    names: &[&str],
    parse: impl Fn(&str) -> Option<UtcMicros>,
) -> Option<UtcMicros> {
    let get = |name: String| headers.get(name).and_then(|v| v.to_str().ok()).map(str::trim);
    let mut exhausted: Option<UtcMicros> = None;
    let mut earliest: Option<UtcMicros> = None;
    for n in names {
        let Some(at) = get(format!("{prefix}reset-{n}"))
            .or_else(|| get(format!("{prefix}{n}-reset")))
            .and_then(&parse)
        else {
            continue;
        };
        let remaining =
            get(format!("{prefix}remaining-{n}")).or_else(|| get(format!("{prefix}{n}-remaining")));
        if remaining == Some("0") {
            exhausted = Some(exhausted.map_or(at, |e| e.max(at)));
        }
        earliest = Some(earliest.map_or(at, |e| e.min(at)));
    }
    exhausted.or(earliest)
}

fn parse_millis(v: &str) -> Option<Duration> {
    let ms: f64 = v.parse().ok()?;
    (ms.is_finite() && ms >= 0.0).then(|| Duration::from_secs_f64(ms / 1000.0))
}

fn retry_after(v: &str, now: UtcMicros) -> Option<UtcMicros> {
    if v.is_empty() {
        return None;
    }
    if v.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        let secs: f64 = v.parse().ok()?;
        return (secs.is_finite() && secs >= 0.0).then(|| now.saturating_add(Duration::from_secs_f64(secs)));
    }
    http_date(v).map(|at| at.max(now))
}

/// A Go-style duration as the reset headers carry it: `1h2m3.5s`, `6m0s`, `20ms`.
fn parse_go_duration(v: &str) -> Option<Duration> {
    let mut total = 0f64;
    let mut rest = v;
    if rest.is_empty() {
        return None;
    }
    while !rest.is_empty() {
        let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
        let (num, tail) = rest.split_at(end);
        let n: f64 = num.parse().ok()?;
        let (scale, len) = if tail.starts_with("ms") {
            (0.001, 2)
        } else if tail.starts_with('h') {
            (3600.0, 1)
        } else if tail.starts_with('m') {
            (60.0, 1)
        } else if tail.starts_with('s') {
            (1.0, 1)
        } else {
            return None;
        };
        total += n * scale;
        rest = tail.get(len..)?;
    }
    (total.is_finite() && total >= 0.0).then(|| Duration::from_secs_f64(total))
}

fn retry_info(body: &Value) -> Option<Duration> {
    let details = body.pointer("/error/details")?.as_array()?;
    details.iter().find_map(|d| {
        let ty = d.get("@type")?.as_str()?;
        if !ty.ends_with("google.rpc.RetryInfo") {
            return None;
        }
        let delay = d.get("retryDelay")?.as_str()?.strip_suffix('s')?;
        let secs: f64 = delay.parse().ok()?;
        (secs.is_finite() && secs >= 0.0).then(|| Duration::from_secs_f64(secs))
    })
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// An HTTP-date in any form RFC 9110 accepts: IMF-fixdate, RFC 850 or asctime.
fn http_date(v: &str) -> Option<UtcMicros> {
    let cleaned = v.replace(',', " ");
    let parts: Vec<&str> = cleaned.split_whitespace().collect();
    let month = |s: &str| {
        let lower = s.to_ascii_lowercase();
        MONTHS.iter().position(|m| *m == lower).map(|i| i + 1)
    };
    let (day, mon, year, time) = match parts.as_slice() {
        [_, d, m, y, t, "GMT"] => (d.parse().ok()?, month(m)?, y.parse::<i64>().ok()?, *t),
        [_, dmy, t, "GMT"] => {
            let mut it = dmy.split('-');
            let d = it.next()?.parse().ok()?;
            let m = month(it.next()?)?;
            let y: i64 = it.next()?.parse().ok()?;
            (
                d,
                m,
                if y < 70 {
                    2000 + y
                } else if y < 100 {
                    1900 + y
                } else {
                    y
                },
                *t,
            )
        }
        [_, m, d, t, y] => (d.parse().ok()?, month(m)?, y.parse::<i64>().ok()?, *t),
        _ => return None,
    };
    let mut hms = time.split(':').map(str::parse::<i64>);
    let (h, mi, s) = (hms.next()?.ok()?, hms.next()?.ok()?, hms.next()?.ok()?);
    if !(1..=31).contains(&day) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    let days = days_from_civil(year, i64::try_from(mon).ok()?, day);
    let secs = days * 86_400 + h * 3600 + mi * 60 + s;
    Some(UtcMicros(secs.checked_mul(1_000_000)?))
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, v.parse().unwrap());
        }
        h
    }

    const NOW: UtcMicros = UtcMicros(1_000_000_000_000_000);

    fn secs(h: &HeaderMap) -> Option<f64> {
        hint(h, None, Class::RateLimited, NOW).map(|t| (t.0 - NOW.0) as f64 / 1e6)
    }

    #[test]
    fn header_forms() {
        assert_eq!(secs(&headers(&[("retry-after", "3")])), Some(3.0));
        assert_eq!(secs(&headers(&[("retry-after", "2.5")])), Some(2.5));
        assert_eq!(
            secs(&headers(&[("retry-after-ms", "3000"), ("retry-after", "30")])),
            Some(3.0)
        );
        assert_eq!(
            secs(&headers(&[("retry-after-ms", "-5"), ("retry-after", "3")])),
            Some(3.0)
        );
        assert_eq!(secs(&headers(&[("x-ms-retry-after-ms", "3000")])), Some(3.0));
        assert_eq!(secs(&headers(&[("retry-after", "soon")])), None);
        assert_eq!(
            secs(&headers(&[
                ("x-ratelimit-reset-requests", "3s"),
                ("x-ratelimit-remaining-requests", "0")
            ])),
            Some(3.0)
        );
        assert_eq!(
            secs(&headers(&[
                ("x-ratelimit-reset-requests", "1s"),
                ("x-ratelimit-remaining-requests", "5"),
                ("x-ratelimit-reset-tokens", "3s"),
                ("x-ratelimit-remaining-tokens", "0")
            ])),
            Some(3.0)
        );
        assert_eq!(
            secs(&headers(&[
                ("x-ratelimit-reset-requests", "3s"),
                ("x-ratelimit-remaining-requests", "7"),
                ("x-ratelimit-reset-tokens", "6m0s"),
                ("x-ratelimit-remaining-tokens", "900")
            ])),
            Some(3.0)
        );
        assert_eq!(
            secs(&headers(&[
                ("x-ratelimit-reset-requests", "0m2.5s500ms"),
                ("x-ratelimit-remaining-requests", "0")
            ])),
            Some(3.0)
        );
        assert_eq!(secs(&headers(&[("x-ratelimit-reset", "3")])), Some(3.0));
    }

    #[test]
    fn http_dates() {
        let imf = http_date("Sun, 06 Nov 1994 08:49:37 GMT").unwrap();
        assert_eq!(imf, http_date("Sunday, 06-Nov-94 08:49:37 GMT").unwrap());
        assert_eq!(imf, http_date("Sun Nov  6 08:49:37 1994").unwrap());
        assert_eq!(imf.0, 784_111_777_000_000);
        let past = hint(
            &headers(&[("retry-after", "Sun, 06 Nov 1994 08:49:37 GMT")]),
            None,
            Class::Overloaded,
            NOW,
        );
        assert_eq!(past, Some(NOW));
    }

    #[test]
    fn gemini_retry_info() {
        let body = serde_json::json!({"error": {"details": [{"@type": "type.googleapis.com/google.rpc.RetryInfo", "retryDelay": "1.5s"}]}});
        let at = hint(&HeaderMap::new(), Some(&body), Class::RateLimited, NOW).unwrap();
        assert_eq!(at.0 - NOW.0, 1_500_000);
    }
}
