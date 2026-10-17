//! OTLP `ExportLogsServiceRequest` encodings of request-log records: one LogRecord per record, its
//! body the record's JSON as a string, its trace and span ids the record's, its time the request's
//! start and its severity from the status code.

use antirion_core::UtcMicros;
use serde_json::{Value as Json, json};

use crate::sink::LogRecord;
use crate::trace::otlp::Pb;
use crate::trace::{SpanId, TraceId};

/// The instrumentation scope every record is exported under.
const SCOPE: &str = "antirion";
/// The `event.name` attribute of every exported record.
pub const EVENT_NAME: &str = "antirion.request";

/// One record as a LogRecord carries it.
struct Entry {
    body: String,
    id: String,
    time_unix_nanos: u64,
    severity: (u64, &'static str),
    trace_id: Option<TraceId>,
    span_id: Option<SpanId>,
}

/// OTLP severity numbers: INFO below 400, WARN for client errors, ERROR from 500.
fn severity(status: Option<u64>) -> (u64, &'static str) {
    match status {
        Some(s) if s >= 500 => (17, "ERROR"),
        Some(s) if s >= 400 => (13, "WARN"),
        _ => (9, "INFO"),
    }
}

fn entry(record: &dyn LogRecord, include_content: bool) -> Entry {
    let mut raw = Vec::new();
    record.write_json(include_content, &mut raw);
    let parsed: Json = serde_json::from_slice(&raw).unwrap_or(Json::Null);
    let text = |k: &str| parsed.get(k).and_then(Json::as_str);
    let time_unix_nanos = text("started_at")
        .and_then(|t| UtcMicros::parse_rfc3339(t).ok())
        .and_then(|t| u64::try_from(t.0).ok())
        .map_or(0, |us| us.saturating_mul(1000));
    Entry {
        id: text("id").unwrap_or_default().to_owned(),
        time_unix_nanos,
        severity: severity(parsed.get("status_code").and_then(Json::as_u64)),
        trace_id: text("trace_id").and_then(TraceId::from_hex),
        span_id: text("span_id").and_then(SpanId::from_hex),
        body: String::from_utf8(raw).unwrap_or_default(),
    }
}

fn entries<R: std::ops::Deref<Target = dyn LogRecord>>(records: &[R], include_content: bool) -> Vec<Entry> {
    records.iter().map(|r| entry(&**r, include_content)).collect()
}

fn json_attrs(attrs: &[(String, String)]) -> Json {
    Json::Array(
        attrs
            .iter()
            .map(|(k, v)| json!({"key": k, "value": {"stringValue": v}}))
            .collect(),
    )
}

fn record_attrs(e: &Entry) -> [(String, String); 2] {
    [
        ("event.name".to_owned(), EVENT_NAME.to_owned()),
        ("log.record.uid".to_owned(), e.id.clone()),
    ]
}

/// The OTLP/HTTP JSON body for `records` under one resource, observed at `now_nanos`.
#[must_use]
pub fn encode_json<R: std::ops::Deref<Target = dyn LogRecord>>(
    resource: &[(String, String)],
    version: &str,
    records: &[R],
    include_content: bool,
    now_nanos: u64,
) -> Vec<u8> {
    let logs: Vec<Json> = entries(records, include_content)
        .iter()
        .map(|e| {
            let mut o = json!({
                "timeUnixNano": e.time_unix_nanos.to_string(),
                "observedTimeUnixNano": now_nanos.to_string(),
                "severityNumber": e.severity.0,
                "severityText": e.severity.1,
                "body": {"stringValue": e.body},
                "attributes": json_attrs(&record_attrs(e)),
            });
            if let Some(obj) = o.as_object_mut() {
                if let Some(t) = e.trace_id {
                    obj.insert("traceId".into(), Json::String(t.hex()));
                }
                if let Some(s) = e.span_id {
                    obj.insert("spanId".into(), Json::String(s.hex()));
                }
            }
            o
        })
        .collect();
    let body = json!({"resourceLogs": [{
        "resource": {"attributes": json_attrs(resource)},
        "scopeLogs": [{"scope": {"name": SCOPE, "version": version}, "logRecords": logs}],
    }]});
    serde_json::to_vec(&body).unwrap_or_default()
}

fn pb_attrs(p: &mut Pb, field: u32, attrs: &[(String, String)]) {
    for (k, v) in attrs {
        p.message(field, |kv| {
            kv.string(1, k);
            kv.message(2, |any| any.bytes(1, v.as_bytes()));
        });
    }
}

/// The protobuf `ExportLogsServiceRequest` for `records` under one resource, observed at
/// `now_nanos`.
#[must_use]
pub fn encode_proto<R: std::ops::Deref<Target = dyn LogRecord>>(
    resource: &[(String, String)],
    version: &str,
    records: &[R],
    include_content: bool,
    now_nanos: u64,
) -> Vec<u8> {
    let entries = entries(records, include_content);
    let mut req = Pb::default();
    req.message(1, |rl| {
        rl.message(1, |r| pb_attrs(r, 1, resource));
        rl.message(2, |sl| {
            sl.message(1, |scope| {
                scope.string(1, SCOPE);
                scope.string(2, version);
            });
            for e in &entries {
                sl.message(2, |lr| {
                    lr.fixed64(1, e.time_unix_nanos);
                    lr.uint(2, e.severity.0);
                    lr.string(3, e.severity.1);
                    lr.message(5, |any| any.bytes(1, e.body.as_bytes()));
                    pb_attrs(lr, 6, &record_attrs(e));
                    if let Some(t) = e.trace_id {
                        lr.bytes(9, &t.0);
                    }
                    if let Some(s) = e.span_id {
                        lr.bytes(10, &s.0);
                    }
                    lr.fixed64(11, now_nanos);
                });
            }
        });
    });
    req.0
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn records() -> Vec<Arc<dyn LogRecord>> {
        vec![
            Arc::new(json!({
                "id": "log1", "trace_id": "0af7651916cd43dd8448eb211c80319c", "span_id": "b7ad6b7169203331",
                "started_at": "2026-10-14T10:00:00.000001Z", "status_code": 503,
                "content": {"request": {"messages": []}}, "attempts": []
            })),
            Arc::new(json!({"id": "log2", "status_code": 200})),
        ]
    }

    #[test]
    fn json_carries_the_record_as_its_body() {
        let body: Json = serde_json::from_slice(&encode_json(
            &[("service.name".into(), "antirion".into())],
            "0.1.0",
            &records(),
            false,
            7,
        ))
        .unwrap();
        let logs = &body["resourceLogs"][0]["scopeLogs"][0]["logRecords"];
        let first = &logs[0];
        assert_eq!(first["traceId"], "0af7651916cd43dd8448eb211c80319c");
        assert_eq!(first["spanId"], "b7ad6b7169203331");
        assert_eq!(first["timeUnixNano"], "1791972000000001000");
        assert_eq!(first["observedTimeUnixNano"], "7");
        assert_eq!(first["severityNumber"], 17);
        assert_eq!(first["severityText"], "ERROR");
        let inner: Json = serde_json::from_str(first["body"]["stringValue"].as_str().unwrap()).unwrap();
        assert_eq!(inner["id"], "log1");
        assert_eq!(inner["content"], Json::Null, "content stays out unless included");
        assert_eq!(first["attributes"][1]["value"]["stringValue"], "log1");
        assert!(logs[1].get("traceId").is_none());
        assert_eq!(logs[1]["severityText"], "INFO");
        assert_eq!(
            body["resourceLogs"][0]["resource"]["attributes"][0]["value"]["stringValue"],
            "antirion"
        );
    }

    #[test]
    fn content_is_exported_when_included() {
        let body: Json = serde_json::from_slice(&encode_json(&[], "v", &records(), true, 0)).unwrap();
        let text = body["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0]["body"]["stringValue"]
            .as_str()
            .unwrap();
        assert!(text.contains("messages"));
    }

    #[test]
    fn protobuf_has_the_ids_and_the_body() {
        let bytes = encode_proto(&[], "v", &records(), false, 9);
        assert_eq!(bytes.first(), Some(&0x0a));
        let trace = TraceId::from_hex("0af7651916cd43dd8448eb211c80319c").unwrap();
        let mut field = vec![0x4a, 0x10];
        field.extend_from_slice(&trace.0);
        assert!(bytes.windows(field.len()).any(|w| w == field.as_slice()));
        assert!(bytes.windows(4).any(|w| w == b"log1"));
        assert!(bytes.windows(5).any(|w| w == b"ERROR"));
    }
}
