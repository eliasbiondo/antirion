//! Analytics: `GET /analytics/usage` (buckets of 1 minute to 1 day, or one bucket for the range, by
//! up to three dimensions, over the log selection) and `GET /analytics/latency_histogram`. Both see
//! only records in the caller's `analytics:read` scope.

use antirion_core::UtcMicros;
use antirion_limits::money::Nusd;
use antirion_limits::period::PeriodSpec;
use antirion_obs::sketch::Sketch;
use antirion_runtime::reqlog::analytics::{AggQuery, Group};
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;
use crate::logquery::{self, SELECTION};
use crate::logs::{require_some, searches_content};
use crate::pagination;

const PERMISSION: &str = "analytics:read";

/// The most groups one page of usage analytics returns, the contract's bound: groups of an
/// aggregation, not stored resources, so the page size of resource lists does not cap them.
const MAX_GROUPS: usize = 10_000;

/// Buckets per group one response may hold.
const MAX_BUCKETS: usize = 1_440;
/// The range when the query names none.
const DEFAULT_RANGE_US: i64 = 86_400_000_000;

pub(crate) fn interval_us(s: &str) -> Option<i64> {
    let secs = match s {
        "1m" => 60,
        "5m" => 300,
        "15m" => 900,
        "1h" | "hour" => 3_600,
        "6h" => 21_600,
        "1d" | "day" => 86_400,
        _ => return None,
    };
    Some(secs * 1_000_000)
}

/// `[from, to)`: the query's, or the 24 hours before now.
pub(crate) fn range(call: &Call) -> Result<(i64, i64), AdminError> {
    let to = logquery::time(call, "to")?.unwrap_or_else(|| call.rt.now().0);
    let from = logquery::time(call, "from")?.unwrap_or(to.saturating_sub(DEFAULT_RANGE_US));
    if from >= to {
        return Err(AdminError::invalid_parameter("from must be before to"));
    }
    Ok((from, to))
}

/// Bucket starts over `[from, to)`: multiples of the interval in UTC, days at midnight in `tz`.
pub(crate) fn starts(
    from: i64,
    to: i64,
    interval: Option<i64>,
    tz: Option<&str>,
) -> Result<Vec<i64>, AdminError> {
    let Some(step) = interval else {
        return Ok(vec![from]);
    };
    let too_many = || {
        AdminError::invalid_parameter(format!(
            "the range holds more than {MAX_BUCKETS} buckets of this interval"
        ))
    };
    let mut out = Vec::new();
    match tz.filter(|_| step == 86_400_000_000) {
        Some(zone) => {
            let spec = PeriodSpec::parse("daily", Some(zone), None)
                .map_err(|_| AdminError::invalid_parameter(format!("unknown time zone {zone:?}")))?;
            let mut t = spec.bounds(UtcMicros(from)).start.0;
            while t < to {
                if out.len() == MAX_BUCKETS {
                    return Err(too_many());
                }
                out.push(t);
                let next = spec.bounds(UtcMicros(t)).end.map_or(to, |e| e.0);
                if next <= t {
                    break;
                }
                t = next;
            }
        }
        None => {
            let first = from.div_euclid(step) * step;
            let count = (to - first + step - 1) / step;
            if count > i64::try_from(MAX_BUCKETS).unwrap_or(i64::MAX) {
                return Err(too_many());
            }
            out.extend((0..count).map(|i| first + i * step));
        }
    }
    Ok(out)
}

fn ms(v: Option<f64>) -> Value {
    v.map_or(Value::Null, |x| json!((x * 1000.0).round() / 1000.0))
}

fn percentiles(s: &Sketch) -> Value {
    json!({
        "p50": ms(s.quantile(0.50)),
        "p90": ms(s.quantile(0.90)),
        "p95": ms(s.quantile(0.95)),
        "p99": ms(s.quantile(0.99)),
    })
}

fn usd(n: i64) -> String {
    Nusd(i128::from(n)).to_usd_string()
}

fn bucket(g: &Group, names: &[&str]) -> Value {
    let c = &g.agg.counts;
    let group: serde_json::Map<String, Value> = names
        .iter()
        .zip(&g.key)
        .map(|(n, v)| ((*n).to_owned(), v.clone().map_or(Value::Null, Value::String)))
        .collect();
    json!({
        "start": UtcMicros(g.start_us).to_rfc3339_seconds(),
        "group": group,
        "requests": c[0],
        "errors": c[1],
        "cancelled": c[2],
        "fallbacks": c[3],
        "retries": c[4],
        "cache_hits": c[5],
        "guardrail_blocks": c[6],
        "guardrail_redactions": c[7],
        "guardrail_flags": c[8],
        "input": c[9],
        "input_cache_read": c[10],
        "input_cache_write": c[11],
        "output": c[12],
        "output_reasoning": c[13],
        "cost_usd": usd(c[14]),
        "cache_saved_usd": usd(c[15]),
        "latency_ms": percentiles(&g.agg.latency),
        "ttft_ms": percentiles(&g.agg.ttft),
        "output_tokens_per_second": ms(g.agg.otps.quantile(0.5)),
        "unique_keys": g.keys.len(),
    })
}

fn query(call: &Call, interval: Option<i64>) -> Result<AggQuery, AdminError> {
    let (from, to) = range(call)?;
    let sel = logquery::parse(call, PERMISSION, searches_content(call))?;
    let top = call
        .param("top")
        .map(|v| {
            v.parse::<usize>()
                .ok()
                .filter(|n| (1..=100).contains(n))
                .ok_or_else(|| AdminError::invalid_parameter("top must be an integer from 1 to 100"))
        })
        .transpose()?;
    Ok(AggQuery {
        from_us: from,
        to_us: to,
        starts: starts(from, to, interval, call.param("tz"))?,
        group_by: logquery::group_by(call)?,
        filter: sel.filter,
        rollup: sel.rollup,
        metadata: sel.metadata,
        top,
    })
}

/// `GET /analytics/usage`.
pub(crate) async fn usage(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    logquery::allow(
        call,
        SELECTION,
        &["interval", "group_by", "limit", "cursor", "tz", "top"],
    )?;
    require_some(call, PERMISSION)?;
    let interval = call
        .param("interval")
        .map(|v| {
            interval_us(v).ok_or_else(|| {
                AdminError::invalid_parameter("interval must be one of 1m, 5m, 15m, 1h, 6h, 1d, hour, day")
            })
        })
        .transpose()?;
    let q = query(call, interval)?;
    let max = call.rt.statics.max_page_size.max(MAX_GROUPS);
    let (limit, after) = pagination::page_params_up_to(call, "analytics", max)?;
    let offset = after
        .map(|a| {
            a.parse::<usize>()
                .map_err(|_| AdminError::invalid_parameter("the cursor is not valid"))
        })
        .transpose()?
        .unwrap_or(0);
    let groups = call
        .rt
        .aggregate(&q)
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    let names: Vec<&str> = q.group_by.iter().map(|d| d.name()).collect();
    let items: Vec<Value> = groups
        .iter()
        .skip(offset)
        .take(limit)
        .map(|g| bucket(g, &names))
        .collect();
    let next = (groups.len() > offset + limit)
        .then(|| pagination::encode(&call.rt.cursor_key, "analytics", &(offset + limit).to_string()));
    Ok(call.json(StatusCode::OK, &json!({"items": items, "next_cursor": next})))
}

/// `counts.len()` buckets between log-spaced `bounds` from the smallest to the largest value.
fn histogram(s: &Sketch, n: usize) -> Value {
    let (Some(min), Some(max)) = (s.min(), s.max()) else {
        return json!({"bounds": [], "counts": []});
    };
    let lo = min.max(0.001);
    let hi = if max > lo { max } else { lo * 1.01 };
    let steps = n as f64;
    let mut bounds: Vec<f64> = (0..=n).map(|i| lo * (hi / lo).powf(i as f64 / steps)).collect();
    if let Some(first) = bounds.first_mut() {
        *first = min.min(lo);
    }
    if let Some(last) = bounds.last_mut() {
        *last = hi;
    }
    let mut counts = vec![0u64; n];
    for (v, c) in s.buckets() {
        let i = bounds
            .iter()
            .skip(1)
            .position(|b| v < *b)
            .unwrap_or(n.saturating_sub(1));
        if let Some(slot) = counts.get_mut(i) {
            *slot += c;
        }
    }
    let round = |x: f64| (x * 1000.0).round() / 1000.0;
    json!({
        "bounds": bounds.into_iter().map(round).collect::<Vec<_>>(),
        "counts": counts,
        "p50": ms(s.quantile(0.50)),
        "p95": ms(s.quantile(0.95)),
        "p99": ms(s.quantile(0.99)),
    })
}

/// `GET /analytics/latency_histogram`.
pub(crate) async fn latency_histogram(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    logquery::allow(call, SELECTION, &["measure", "buckets"])?;
    require_some(call, PERMISSION)?;
    let ttft = match call.param("measure") {
        None | Some("latency_ms") => false,
        Some("ttft_ms") => true,
        Some(_) => {
            return Err(AdminError::invalid_parameter(
                "measure must be latency_ms or ttft_ms",
            ));
        }
    };
    let n = match call.param("buckets") {
        None => 12,
        Some(v) => v
            .parse::<usize>()
            .ok()
            .filter(|n| (5..=50).contains(n))
            .ok_or_else(|| AdminError::invalid_parameter("buckets must be an integer from 5 to 50"))?,
    };
    let mut q = query(call, None)?;
    q.group_by.clear();
    q.top = None;
    let groups = call
        .rt
        .aggregate(&q)
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    let mut s = Sketch::new();
    for g in &groups {
        s.merge(if ttft { &g.agg.ttft } else { &g.agg.latency });
    }
    let mut body = histogram(&s, n);
    if s.is_empty()
        && let Some(o) = body.as_object_mut()
    {
        o.remove("p50");
    }
    Ok(call.json(StatusCode::OK, &body))
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: i64 = 60_000_000;

    #[test]
    fn buckets_align_to_the_interval_and_are_bounded() {
        assert_eq!(
            starts(7 * M + 5, 17 * M, Some(5 * M), None).unwrap(),
            vec![5 * M, 10 * M, 15 * M]
        );
        assert_eq!(starts(0, 30 * M, None, None).unwrap(), vec![0]);
        assert!(starts(0, 2 * 1_440 * M, Some(M), None).is_err());
        assert_eq!(starts(0, 1_440 * M, Some(M), None).unwrap().len(), 1_440);
        let day = 86_400_000_000;
        let t = |s: &str| UtcMicros::parse_rfc3339(s).unwrap().0;
        let from = t("2026-09-30T00:00:00Z");
        let sp = starts(from, from + 2 * day, Some(day), Some("America/Sao_Paulo")).unwrap();
        assert_eq!(
            sp,
            vec![
                t("2026-09-29T03:00:00Z"),
                t("2026-09-30T03:00:00Z"),
                t("2026-10-01T03:00:00Z")
            ]
        );
        assert!(starts(0, day, Some(day), Some("Nowhere/Else")).is_err());
    }

    #[test]
    fn histogram_counts_cover_every_value_and_bounds_increase() {
        let mut s = Sketch::new();
        for v in [5.0, 9.0, 120.0, 4_000.0, 4_000.0] {
            s.add(v);
        }
        let h = histogram(&s, 12);
        let bounds: Vec<f64> = serde_json::from_value(h["bounds"].clone()).unwrap();
        let counts: Vec<u64> = serde_json::from_value(h["counts"].clone()).unwrap();
        assert_eq!(counts.len(), 12);
        assert_eq!(bounds.len(), 13);
        assert!(bounds.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(counts.iter().sum::<u64>(), 5);
        assert_eq!(bounds[0], 5.0);
        assert_eq!(bounds[12], 4_000.0);
        let one = {
            let mut s = Sketch::new();
            s.add(7.0);
            histogram(&s, 5)
        };
        assert_eq!(
            serde_json::from_value::<Vec<u64>>(one["counts"].clone())
                .unwrap()
                .iter()
                .sum::<u64>(),
            1
        );
        assert_eq!(histogram(&Sketch::new(), 5)["counts"], json!([]));
    }
}
