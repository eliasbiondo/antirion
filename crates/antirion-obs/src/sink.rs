//! What every batching exporter shares: the retry schedule and the batch encodings.

use std::time::Duration;

use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};
use serde_json::Value;

use crate::settings::BatchFormat;

/// The delay before retry `n` (1-based) of a failed batch: 1 s doubling to at most 30 s, then ±20 %
/// jitter from `u` in `[0, 1)`.
#[must_use]
pub fn backoff(n: u32, u: f64) -> Duration {
    let base = 2f64
        .powi(i32::try_from(n.saturating_sub(1).min(16)).unwrap_or(16))
        .min(30.0);
    Duration::from_secs_f64(base * (0.8 + 0.4 * u.clamp(0.0, 1.0)))
}

/// One batch of records in `format`.
#[must_use]
pub fn encode<T: Serialize>(format: BatchFormat, records: &[T]) -> Vec<u8> {
    match format {
        BatchFormat::Ndjson => {
            let mut out = Vec::new();
            for r in records {
                if serde_json::to_writer(&mut out, r).is_ok() {
                    out.push(b'\n');
                }
            }
            out
        }
        BatchFormat::JsonArray | BatchFormat::TraceIngest | BatchFormat::OtlpLogs(_) => {
            serde_json::to_vec(records).unwrap_or_default()
        }
    }
}

/// A settled request's log record, which each sink serialises as it sends it.
pub trait LogRecord: Send + Sync + std::fmt::Debug {
    /// Appends the record as JSON: without content (a null `content` and null attempt
    /// `error_body`s) unless `include_content`.
    fn write_json(&self, include_content: bool, out: &mut Vec<u8>);
}

impl LogRecord for Value {
    fn write_json(&self, include_content: bool, out: &mut Vec<u8>) {
        let _ = serde_json::to_writer(
            out,
            &ForSink {
                record: self,
                include_content,
            },
        );
    }
}

/// One batch of log records in `format`.
#[must_use]
pub fn encode_records<R: std::ops::Deref<Target = dyn LogRecord>>(
    format: BatchFormat,
    records: &[R],
    include_content: bool,
) -> Vec<u8> {
    match format {
        BatchFormat::TraceIngest => return crate::ingest::encode(records, include_content, None),
        BatchFormat::OtlpLogs(crate::settings::TraceProtocol::HttpJson) => {
            return crate::otlp_logs::encode_json(&[], "", records, include_content, 0);
        }
        BatchFormat::OtlpLogs(_) => {
            return crate::otlp_logs::encode_proto(&[], "", records, include_content, 0);
        }
        BatchFormat::Ndjson | BatchFormat::JsonArray => {}
    }
    let mut out = Vec::new();
    if format == BatchFormat::JsonArray {
        out.push(b'[');
    }
    for (i, r) in records.iter().enumerate() {
        match format {
            BatchFormat::Ndjson => {
                r.write_json(include_content, &mut out);
                out.push(b'\n');
            }
            BatchFormat::JsonArray | BatchFormat::TraceIngest | BatchFormat::OtlpLogs(_) => {
                if i > 0 {
                    out.push(b',');
                }
                r.write_json(include_content, &mut out);
            }
        }
    }
    if format == BatchFormat::JsonArray {
        out.push(b']');
    }
    out
}

/// The record as a sink sends it: without content (and attempt error bodies) unless the sink includes
/// content. It serialises the record itself, so exporting never copies it.
#[derive(Copy, Clone, Debug)]
pub struct ForSink<'a> {
    pub record: &'a Value,
    pub include_content: bool,
}

impl Serialize for ForSink<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let Value::Object(fields) = self.record else {
            return self.record.serialize(s);
        };
        if self.include_content {
            return self.record.serialize(s);
        }
        let mut m = s.serialize_map(Some(fields.len()))?;
        for (k, v) in fields {
            match k.as_str() {
                "content" => m.serialize_entry(k, &Value::Null)?,
                "attempts" => m.serialize_entry(k, &Attempts(v))?,
                _ => m.serialize_entry(k, v)?,
            }
        }
        m.end()
    }
}

/// The `attempts` of a record without their error bodies.
struct Attempts<'a>(&'a Value);

impl Serialize for Attempts<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let Value::Array(items) = self.0 else {
            return self.0.serialize(s);
        };
        let mut seq = s.serialize_seq(Some(items.len()))?;
        for item in items {
            seq.serialize_element(&WithoutErrorBody(item))?;
        }
        seq.end()
    }
}

struct WithoutErrorBody<'a>(&'a Value);

impl Serialize for WithoutErrorBody<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let Value::Object(fields) = self.0 else {
            return self.0.serialize(s);
        };
        let mut m = s.serialize_map(Some(fields.len()))?;
        for (k, v) in fields {
            if k == "error_body" {
                m.serialize_entry(k, &Value::Null)?;
            } else {
                m.serialize_entry(k, v)?;
            }
        }
        m.end()
    }
}

/// Nulls `content` and every attempt's `error_body`; returns whether anything was removed.
pub fn strip_content(r: &mut Value) -> bool {
    let mut removed = false;
    if let Some(c) = r.get_mut("content")
        && !c.is_null()
    {
        *c = Value::Null;
        removed = true;
    }
    if let Some(attempts) = r.get_mut("attempts").and_then(Value::as_array_mut) {
        for a in attempts {
            if let Some(b) = a.get_mut("error_body")
                && !b.is_null()
            {
                *b = Value::Null;
                removed = true;
            }
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn backoff_doubles_with_jitter_and_caps() {
        assert_eq!(backoff(1, 0.5), Duration::from_secs(1));
        assert_eq!(backoff(2, 0.5), Duration::from_secs(2));
        assert_eq!(backoff(10, 0.5), Duration::from_secs(30));
        assert_eq!(backoff(1, 0.0), Duration::from_millis(800));
        assert!(backoff(1, 0.999_999) <= Duration::from_millis(1200));
    }

    #[test]
    fn encodings() {
        let a = json!({"id": "a"});
        let b = json!({"id": "b"});
        assert_eq!(
            encode(BatchFormat::Ndjson, &[&a, &b]),
            b"{\"id\":\"a\"}\n{\"id\":\"b\"}\n"
        );
        assert_eq!(encode(BatchFormat::JsonArray, &[&a]), b"[{\"id\":\"a\"}]");
    }

    #[test]
    fn records_encode_like_values() {
        let a: std::sync::Arc<dyn LogRecord> =
            std::sync::Arc::new(json!({"id": "a", "content": {"x": 1}, "attempts": [{"error_body": "e"}]}));
        let b: std::sync::Arc<dyn LogRecord> = std::sync::Arc::new(json!({"id": "b"}));
        let values = [
            json!({"id": "a", "content": null, "attempts": [{"error_body": null}]}),
            json!({"id": "b"}),
        ];
        for format in [BatchFormat::Ndjson, BatchFormat::JsonArray] {
            assert_eq!(
                encode_records(
                    format,
                    &[std::sync::Arc::clone(&a), std::sync::Arc::clone(&b)],
                    false
                ),
                encode(format, &values)
            );
        }
        assert!(
            String::from_utf8(encode_records(BatchFormat::JsonArray, &[a], true))
                .unwrap()
                .contains("\"x\":1")
        );
    }

    #[test]
    fn content_is_stripped_unless_included() {
        let r = json!({"id": "i", "content": {"request": 1}, "attempts": [{"error_body": "x", "n": 1}]});
        let view = |include_content| ForSink {
            record: &r,
            include_content,
        };
        let mut stripped = r.clone();
        assert!(strip_content(&mut stripped));
        assert_eq!(serde_json::to_string(&view(false)).unwrap(), stripped.to_string());
        assert_eq!(serde_json::to_string(&view(true)).unwrap(), r.to_string());
        assert_eq!(
            encode(BatchFormat::Ndjson, &[view(false)]),
            format!("{stripped}\n").into_bytes()
        );
    }
}
