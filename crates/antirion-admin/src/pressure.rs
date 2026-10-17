//! `GET /reports/provider_pressure`: each provider's upstream rate-limit pressure over time, from
//! every node's readings, and how many providers ran hot in each bucket.

use antirion_core::UtcMicros;
use antirion_identity::rbac::Scope;
use antirion_providers::pressure::Series;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::Handled;
use crate::call::Call;
use crate::error::AdminError;

/// The share of a quota at which a provider counts as hot.
const HOT: f64 = 0.8;
const MAX_BUCKETS: i64 = 1440;
const DAY_US: i64 = 86_400_000_000;

pub(crate) fn routes(segs: &[&str]) -> bool {
    matches!(segs, ["reports", "provider_pressure"])
}

pub(crate) fn dispatch<'a>(call: &'a Call, segs: &'a [&'a str]) -> Handled<'a> {
    match segs {
        ["reports", "provider_pressure"] => Box::pin(report(call)),
        _ => crate::routes::done(Err(AdminError::not_found())),
    }
}

fn time(call: &Call, name: &str) -> Result<Option<i64>, AdminError> {
    call.param(name)
        .map(|v| {
            UtcMicros::parse_rfc3339(v)
                .map(|t| t.0)
                .map_err(|_| AdminError::invalid_parameter(format!("{name} must be an RFC 3339 time")))
        })
        .transpose()
}

fn ts(us: i64) -> String {
    UtcMicros(us).to_rfc3339_seconds()
}

fn hot(v: Option<f64>) -> bool {
    v.is_some_and(|v| v >= HOT)
}

/// The report's body for `series`, whose buckets start at `starts`.
fn body(series: &[Series], starts: &[i64], from: i64, to: i64, interval: Option<&str>) -> Value {
    let items: Vec<Value> = series
        .iter()
        .map(|s| {
            let buckets: Vec<Value> = s
                .buckets
                .iter()
                .map(|b| {
                    json!({
                        "start": ts(b.start_us),
                        "rpm_utilization": b.rpm_utilization,
                        "tpm_utilization": b.tpm_utilization,
                        "requests_limit": b.requests_limit,
                        "tokens_limit": b.tokens_limit,
                    })
                })
                .collect();
            json!({"provider": s.provider, "buckets": buckets})
        })
        .collect();
    let counts: Vec<Value> = starts
        .iter()
        .enumerate()
        .map(|(i, start)| {
            let n = series
                .iter()
                .filter(|s| {
                    s.buckets
                        .get(i)
                        .is_some_and(|b| hot(b.rpm_utilization) || hot(b.tpm_utilization))
                })
                .count();
            json!({"start": ts(*start), "providers": n})
        })
        .collect();
    json!({
        "from": ts(from),
        "to": ts(to),
        "interval": interval,
        "threshold": HOT,
        "items": items,
        "hot": counts,
    })
}

async fn report(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&["from", "to", "interval", "provider"])?;
    call.require("analytics:read", &[Scope::Gateway])?;
    let to = time(call, "to")?.unwrap_or_else(|| call.rt.now().0 + 1);
    let from = time(call, "from")?.unwrap_or(to - DAY_US);
    if from >= to {
        return Err(AdminError::invalid_parameter("from must be before to"));
    }
    let interval_text = call.param("interval");
    let interval = interval_text
        .map(|v| {
            crate::delivery::interval_us(v).ok_or_else(|| AdminError::invalid_parameter("unknown interval"))
        })
        .transpose()?;
    if interval.is_some_and(|i| (to - from) / i > MAX_BUCKETS) {
        return Err(AdminError::invalid_parameter(
            "the range holds more than 1440 buckets of this interval",
        ));
    }
    let wanted: Vec<&str> = call
        .query
        .iter()
        .filter(|(k, _)| k == "provider")
        .map(|(_, v)| v.as_str())
        .collect();
    let mut series = call.rt.provider_pressure(from, to, interval).await.map_err(|e| {
        tracing::warn!(error = %e, "provider pressure could not be read");
        AdminError::store_unavailable()
    })?;
    if !wanted.is_empty() {
        series.retain(|s| wanted.contains(&s.provider.as_str()));
    }
    let starts = antirion_providers::pressure::starts(from, to, interval);
    Ok(call.json(StatusCode::OK, &body(&series, &starts, from, to, interval_text)))
}

#[cfg(test)]
mod tests {
    use antirion_providers::pressure::Bucket;

    use super::*;

    #[test]
    fn hot_providers_are_counted_per_bucket() {
        let bucket = |start_us: i64, rpm: Option<f64>, tpm: Option<f64>| Bucket {
            start_us,
            rpm_utilization: rpm,
            tpm_utilization: tpm,
            requests_limit: rpm.map(|_| 500),
            tokens_limit: tpm.map(|_| 9000),
        };
        let series = [
            Series {
                provider: "a".into(),
                buckets: vec![bucket(0, Some(0.85), None), bucket(60, Some(0.2), None)],
            },
            Series {
                provider: "b".into(),
                buckets: vec![bucket(0, Some(0.1), Some(0.8)), bucket(60, None, None)],
            },
        ];
        let v = body(&series, &[0, 60], 0, 120, Some("1m"));
        assert_eq!(v["hot"][0]["providers"], 2);
        assert_eq!(v["hot"][1]["providers"], 0);
        assert_eq!(v["items"][0]["buckets"][0]["requests_limit"], 500);
        assert_eq!(v["items"][1]["buckets"][1]["rpm_utilization"], Value::Null);
        assert_eq!(v["threshold"], 0.8);
    }
}
