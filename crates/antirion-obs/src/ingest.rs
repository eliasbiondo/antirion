//! The trace-ingestion batch: each request-log record becomes one `trace-create` event and one
//! `generation-create` event per upstream attempt, in the batch shape of the ingestion API that
//! `observability.request_logs.sinks.langfuse` sinks deliver to. Content goes out only when the
//! record carries it (the content policy) and the sink includes it.

use std::time::Duration;

use antirion_core::UtcMicros;
use serde_json::{Map, Value, json};

use crate::sink::LogRecord;

/// The path of the ingestion endpoint under a sink's base URL.
pub const PATH: &str = "/api/public/ingestion";

fn get<'a>(v: &'a Value, k: &str) -> &'a Value {
    v.get(k).unwrap_or(&Value::Null)
}

fn text(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or_default().to_owned()
}

/// `at` plus `ms` milliseconds, as RFC 3339 with microseconds.
fn after(at: &str, ms: Option<f64>) -> Value {
    let (Ok(start), Some(ms)) = (UtcMicros::parse_rfc3339(at), ms) else {
        return Value::Null;
    };
    let micros = (ms.max(0.0) * 1000.0).round();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let d = Duration::from_micros(micros as u64);
    Value::String(start.saturating_add(d).to_rfc3339_micros())
}

/// Token usage in the ingestion API's shape.
fn usage(u: &Value) -> Value {
    let n = |k: &str| u.get(k).and_then(Value::as_u64);
    match (n("input"), n("output")) {
        (None, None) => Value::Null,
        (input, output) => json!({
            "input": input.unwrap_or(0),
            "output": output.unwrap_or(0),
            "total": n("total").unwrap_or(input.unwrap_or(0) + output.unwrap_or(0)),
            "unit": "TOKENS",
        }),
    }
}

fn cost(c: &Value) -> Value {
    c.get("total")
        .and_then(Value::as_str)
        .and_then(|t| t.parse::<f64>().ok())
        .map_or(Value::Null, |t| json!({"total": t}))
}

fn non_null(m: Map<String, Value>) -> Value {
    Value::Object(m.into_iter().filter(|(_, v)| !v.is_null()).collect())
}

/// The events of one record.
#[must_use]
pub fn events(record: &Value, environment: Option<&str>) -> Vec<Value> {
    let id = text(record, "id");
    let trace = record
        .get("trace_id")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .map_or_else(|| id.clone(), str::to_owned);
    let started = text(record, "started_at");
    let content = get(record, "content");
    let ended = text(record, "ended_at");
    let cache = get(record, "cache");
    let mut metadata = Map::new();
    for k in [
        "request_id",
        "organization",
        "team",
        "project",
        "key",
        "requested_model",
        "model",
        "route",
        "status_code",
        "response_path",
        "code",
        "reason",
        "endpoint",
    ] {
        metadata.insert(k.to_owned(), get(record, k).clone());
    }
    metadata.insert("log_id".into(), Value::String(id.clone()));
    metadata.insert("cache_status".into(), get(cache, "status").clone());
    let mut body = Map::new();
    body.insert("id".into(), Value::String(trace.clone()));
    body.insert("timestamp".into(), Value::String(started.clone()));
    body.insert("name".into(), get(record, "endpoint").clone());
    body.insert("userId".into(), get(record, "end_user").clone());
    body.insert("sessionId".into(), get(record, "session_id").clone());
    body.insert("release".into(), get(record, "gateway_version").clone());
    body.insert("environment".into(), environment.map_or(Value::Null, Value::from));
    body.insert("tags".into(), get(record, "tags").clone());
    body.insert("metadata".into(), non_null(metadata));
    body.insert("input".into(), get(content, "request").clone());
    body.insert("output".into(), get(content, "response").clone());
    let mut out = vec![json!({
        "id": format!("{id}-trace"),
        "timestamp": ended,
        "type": "trace-create",
        "body": non_null(body),
    })];
    let attempts = record
        .get("attempts")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let serving = attempts
        .iter()
        .rposition(|a| a.get("outcome").and_then(Value::as_str) == Some("success"));
    for (i, a) in attempts.iter().enumerate() {
        let start = text(a, "started_at");
        let served = Some(i) == serving;
        let failure = a.get("failure_class").filter(|f| !f.is_null());
        let mut g = Map::new();
        g.insert("id".into(), Value::String(format!("{id}-{i}")));
        g.insert("traceId".into(), Value::String(trace.clone()));
        g.insert("name".into(), get(a, "deployment").clone());
        g.insert("startTime".into(), Value::String(start.clone()));
        g.insert(
            "endTime".into(),
            after(&start, a.get("duration_ms").and_then(Value::as_f64)),
        );
        g.insert(
            "completionStartTime".into(),
            after(&start, a.get("ttft_ms").and_then(Value::as_f64)),
        );
        g.insert("model".into(), get(a, "upstream_model").clone());
        g.insert(
            "level".into(),
            Value::from(if failure.is_some() { "ERROR" } else { "DEFAULT" }),
        );
        g.insert("statusMessage".into(), failure.cloned().unwrap_or(Value::Null));
        let used = if served {
            get(record, "usage")
        } else {
            get(a, "usage")
        };
        g.insert("usage".into(), usage(used));
        if served {
            g.insert("costDetails".into(), cost(get(record, "cost")));
            g.insert("input".into(), get(content, "request").clone());
            g.insert("output".into(), get(content, "response").clone());
        }
        g.insert(
            "metadata".into(),
            non_null(
                [
                    ("provider", get(a, "provider").clone()),
                    ("status_code", get(a, "status_code").clone()),
                    ("outcome", get(a, "outcome").clone()),
                    ("reason", get(a, "reason").clone()),
                ]
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
            ),
        );
        out.push(json!({
            "id": format!("{id}-{i}-generation"),
            "timestamp": ended,
            "type": "generation-create",
            "body": non_null(g),
        }));
    }
    out
}

/// One ingestion request body for a batch of records.
#[must_use]
pub fn encode<R: std::ops::Deref<Target = dyn LogRecord>>(
    records: &[R],
    include_content: bool,
    environment: Option<&str>,
) -> Vec<u8> {
    let mut batch = Vec::new();
    for r in records {
        let mut raw = Vec::new();
        r.write_json(include_content, &mut raw);
        if let Ok(v) = serde_json::from_slice::<Value>(&raw) {
            batch.extend(events(&v, environment));
        }
    }
    serde_json::to_vec(&json!({"batch": batch})).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> Value {
        json!({
            "id": "log1", "request_id": "req1", "trace_id": "0af7651916cd43dd8448eb211c80319c",
            "started_at": "2026-10-14T10:00:00.000000Z", "ended_at": "2026-10-14T10:00:01.000000Z",
            "endpoint": "/v1/chat/completions", "end_user": "u-1", "session_id": null, "tags": ["env:prod"],
            "gateway_version": "0.1.0", "organization": "acme", "model": "m", "status_code": 200,
            "cache": {"status": "miss"},
            "usage": {"input": 11, "output": 4, "total": 15},
            "cost": {"currency": "USD", "total": "0.002"},
            "content": {"request": {"messages": []}, "response": {"choices": []}},
            "attempts": [
                {"index": 0, "deployment": "a", "provider": "p", "upstream_model": "up", "outcome": "failure",
                 "failure_class": "server_error", "started_at": "2026-10-14T10:00:00.100000Z", "duration_ms": 50.0,
                 "ttft_ms": null, "status_code": 500, "usage": null, "reason": "primary"},
                {"index": 1, "deployment": "b", "provider": "p", "upstream_model": "up", "outcome": "success",
                 "failure_class": null, "started_at": "2026-10-14T10:00:00.200000Z", "duration_ms": 700.5,
                 "ttft_ms": 120.0, "status_code": 200, "usage": null, "reason": "retry"}
            ]
        })
    }

    #[test]
    fn a_record_is_a_trace_and_a_generation_per_attempt() {
        let ev = events(&record(), Some("prod"));
        assert_eq!(ev.len(), 3);
        assert_eq!(ev[0]["type"], "trace-create");
        assert_eq!(ev[0]["body"]["id"], "0af7651916cd43dd8448eb211c80319c");
        assert_eq!(ev[0]["body"]["userId"], "u-1");
        assert_eq!(ev[0]["body"]["environment"], "prod");
        assert_eq!(ev[0]["body"]["metadata"]["log_id"], "log1");
        assert!(ev[0]["body"].get("sessionId").is_none());
        let failed = &ev[1]["body"];
        assert_eq!(failed["level"], "ERROR");
        assert_eq!(failed["statusMessage"], "server_error");
        assert!(failed.get("usage").is_none() && failed.get("input").is_none());
        let served = &ev[2]["body"];
        assert_eq!(served["traceId"], ev[0]["body"]["id"]);
        assert_eq!(served["endTime"], "2026-10-14T10:00:00.900500Z");
        assert_eq!(served["completionStartTime"], "2026-10-14T10:00:00.320000Z");
        assert_eq!(
            served["usage"],
            json!({"input": 11, "output": 4, "total": 15, "unit": "TOKENS"})
        );
        assert_eq!(served["costDetails"], json!({"total": 0.002}));
        assert_eq!(served["output"], json!({"choices": []}));
    }

    #[test]
    fn without_content_nothing_of_it_is_sent() {
        let mut r = record();
        r["content"] = Value::Null;
        let ev = events(&r, None);
        assert!(
            ev.iter()
                .all(|e| e["body"].get("input").is_none() && e["body"].get("output").is_none())
        );
        assert!(ev[0]["body"].get("environment").is_none());
    }
}
