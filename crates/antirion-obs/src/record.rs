//! The request-log record (schema version 1): its published JSON Schema, and the bounded forms of
//! what the record copies from a request (content bodies, metadata, header values).

use serde_json::{Map, Value, json};

use crate::metrics::truncate_utf8;

/// The record's schema version.
pub const SCHEMA_VERSION: u64 = 1;

/// Top-level members: name, JSON type, nullable.
pub const FIELDS: &[(&str, &str, bool)] = &[
    ("schema_version", "integer", false),
    ("id", "string", false),
    ("request_id", "string", false),
    ("trace_id", "string", false),
    ("span_id", "string", false),
    ("node_id", "string", false),
    ("gateway_version", "string", false),
    ("config_generation", "integer", false),
    ("started_at", "string", false),
    ("ended_at", "string", false),
    ("method", "string", false),
    ("endpoint", "string", false),
    ("operation", "string", false),
    ("dialect", "string", false),
    ("stream", "object", true),
    ("client_ip", "string", true),
    ("user_agent", "string", true),
    ("client_retry_count", "integer", true),
    ("organization", "string", true),
    ("team", "string", true),
    ("project", "string", true),
    ("key", "string", true),
    ("end_user", "string", true),
    ("tags", "array", false),
    ("metadata", "object", false),
    ("metadata_dropped", "integer", false),
    ("session_id", "string", true),
    ("requested_model", "string", true),
    ("model", "string", true),
    ("route", "string", true),
    ("provider", "string", true),
    ("deployment", "string", true),
    ("upstream_model", "string", true),
    ("response_model", "string", true),
    ("upstream_request_id", "string", true),
    ("response_path", "string", false),
    ("status_code", "integer", false),
    ("code", "string", true),
    ("reason", "string", true),
    ("stage", "string", true),
    ("error_message", "string", true),
    ("finish_reasons", "array", false),
    ("stop_category", "string", true),
    ("client_disconnected", "boolean", false),
    ("attempt_count", "integer", false),
    ("attempts", "array", false),
    ("skipped_candidates", "array", false),
    ("translation_warnings", "array", false),
    ("params_applied", "object", false),
    ("cache", "object", false),
    ("admission", "object", true),
    ("degraded", "array", false),
    ("guardrails", "array", false),
    ("extensions", "array", false),
    ("usage", "object", false),
    ("cost", "object", false),
    ("timing", "object", false),
    ("request_headers", "object", false),
    ("response_headers", "object", false),
    ("content", "object", true),
    ("mcp", "object", true),
    ("a2a", "object", true),
    ("experiment", "object", true),
    ("route_path", "string", true),
    ("route_rules", "array", false),
    ("budget_downgrade", "object", true),
];

/// Members of each attempt.
pub const ATTEMPT_FIELDS: &[(&str, &str, bool)] = &[
    ("index", "integer", false),
    ("reason", "string", false),
    ("provider", "string", false),
    ("deployment", "string", false),
    ("upstream_model", "string", false),
    ("credential", "string", true),
    ("started_at", "string", false),
    ("duration_ms", "number", false),
    ("ttft_ms", "number", true),
    ("status_code", "integer", true),
    ("outcome", "string", false),
    ("failure_class", "string", true),
    ("retry_after_ms", "integer", true),
    ("upstream_request_id", "string", true),
    ("error_message", "string", true),
    ("error_body", "string", true),
    ("usage", "object", true),
    ("key_rotated", "boolean", false),
];

pub const USAGE_TOKENS: &[&str] = &[
    "input",
    "input_cache_read",
    "input_cache_write",
    "input_cache_write_1h",
    "input_audio",
    "input_image",
    "input_video",
    "output",
    "output_reasoning",
    "output_audio",
    "output_image",
    "total",
];

const TIMING: &[&str] = &[
    "total_ms",
    "overhead_ms",
    "upstream_ms",
    "wait_ms",
    "external_ms",
    "ttft_ms",
    "upstream_ttft_ms",
    "tpot_ms",
    "ttfb_overhead_ms",
    "relay_ms",
];

const COST: &[&str] = &[
    "total",
    "input",
    "cache_read",
    "cache_write",
    "output",
    "reasoning",
    "audio",
    "image",
    "video",
    "unit",
];

fn typed(t: &str, nullable: bool) -> Value {
    if nullable {
        json!({"type": [t, "null"]})
    } else {
        json!({"type": t})
    }
}

fn object(fields: &[(&str, &str, bool)], overrides: &[(&str, Value)]) -> Value {
    let mut props = Map::new();
    for (name, t, nullable) in fields {
        props.insert((*name).to_owned(), typed(t, *nullable));
    }
    for (name, v) in overrides {
        props.insert((*name).to_owned(), v.clone());
    }
    let required: Vec<&str> = fields.iter().map(|(n, _, _)| *n).collect();
    json!({"type": "object", "properties": props, "required": required})
}

fn closed(names: &[&str], t: Value) -> Value {
    let props: Map<String, Value> = names.iter().map(|n| ((*n).to_owned(), t.clone())).collect();
    json!({"type": "object", "properties": props, "required": names})
}

/// The JSON Schema of a record, served at `GET /admin/v1/logs/schema`.
#[must_use]
pub fn schema() -> Value {
    let int_or_null = json!({"type": ["integer", "null"]});
    let ms = json!({"type": ["number", "null"], "minimum": 0});
    let money = json!({"type": ["string", "null"]});
    let mut usage = closed(USAGE_TOKENS, int_or_null.clone());
    if let Some(props) = usage.get_mut("properties").and_then(Value::as_object_mut) {
        props.insert("units".into(), json!({"type": ["object", "null"]}));
        props.insert(
            "source".into(),
            json!({"enum": ["upstream", "estimated", "cache", "none"]}),
        );
    }
    let mut cost = closed(COST, money.clone());
    if let Some(props) = cost.get_mut("properties").and_then(Value::as_object_mut) {
        props.insert("currency".into(), json!({"const": "USD"}));
        props.insert("saved".into(), money.clone());
        props.insert(
            "status".into(),
            json!({"enum": ["exact", "approximate", "estimated", "unpriced"]}),
        );
    }
    let mut timing = closed(TIMING, ms);
    if let Some(props) = timing.get_mut("properties").and_then(Value::as_object_mut) {
        props.insert("stage_ms".into(), json!({"type": ["object", "null"]}));
    }
    let attempt = object(
        ATTEMPT_FIELDS,
        &[(
            "outcome",
            json!({"enum": ["success", "upstream_4xx", "upstream_429", "upstream_5xx", "timeout",
                             "connect_error", "stream_error", "cancelled"]}),
        )],
    );
    let body = json!({"type": "object", "required": ["body", "truncated", "original_bytes"], "properties": {
        "body": {"type": ["object", "string", "array"]},
        "truncated": {"type": "boolean"},
        "original_bytes": {"type": "integer"},
    }});
    let mut doc = object(
        FIELDS,
        &[
            ("schema_version", json!({"const": SCHEMA_VERSION})),
            ("trace_id", json!({"type": "string", "pattern": "^[0-9a-f]{32}$"})),
            ("span_id", json!({"type": "string", "pattern": "^[0-9a-f]{16}$"})),
            ("attempts", json!({"type": "array", "items": attempt})),
            ("usage", usage),
            ("cost", cost),
            ("timing", timing),
            ("tags", json!({"type": "array", "items": {"type": "string"}})),
            (
                "metadata",
                json!({"type": "object", "additionalProperties": {"type": "string"}}),
            ),
            (
                "stream",
                json!({"type": ["object", "null"], "required": ["end_reason", "committed", "relay_mode"]}),
            ),
            (
                "cache",
                json!({"type": "object", "required": ["status", "layer", "similarity", "key"]}),
            ),
            (
                "content",
                json!({"type": ["object", "null"], "properties": {"request": body, "response": {"oneOf": [body, {"type": "null"}]}}}),
            ),
        ],
    );
    if let Some(o) = doc.as_object_mut() {
        o.insert(
            "$schema".into(),
            json!("https://json-schema.org/draft/2020-12/schema"),
        );
        o.insert("title".into(), json!("Antirion request-log record"));
    }
    doc
}

/// A recorded body: the parsed JSON when it fits in `max` bytes, else the first `max` bytes cut at a
/// UTF-8 boundary.
#[must_use]
pub fn body(bytes: &[u8], parsed: Option<Value>, max: usize) -> Value {
    let original = bytes.len();
    if original <= max {
        let body = parsed
            .or_else(|| serde_json::from_slice(bytes).ok())
            .unwrap_or_else(|| Value::String(String::from_utf8_lossy(bytes).into_owned()));
        return json!({"body": body, "truncated": false, "original_bytes": original});
    }
    let text = String::from_utf8_lossy(bytes.get(..max.min(original)).unwrap_or_default());
    let mut cut = truncate_utf8(&text, max).to_owned();
    while cut.ends_with('\u{FFFD}') {
        cut.pop();
    }
    json!({"body": cut, "truncated": true, "original_bytes": original})
}

/// Request metadata flattened to strings and bounded: entries beyond `max_entries` (in key order) and
/// keys outside `[A-Za-z0-9_.-]{1,64}` are dropped and counted; values are cut to `max_value_bytes`.
#[must_use]
pub fn metadata(
    meta: &Map<String, Value>,
    max_entries: usize,
    max_value_bytes: usize,
) -> (Map<String, Value>, u64) {
    let mut out = Map::new();
    let mut dropped = 0u64;
    let mut entries: Vec<(&String, &Value)> = meta.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    for (k, v) in entries {
        let valid = (1..=64).contains(&k.len())
            && k.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
        if !valid || out.len() >= max_entries {
            dropped += 1;
            continue;
        }
        let text = match v {
            Value::String(s) => s.clone(),
            other => serde_json::to_string(other).unwrap_or_default(),
        };
        out.insert(
            k.clone(),
            Value::String(truncate_utf8(&text, max_value_bytes).to_owned()),
        );
    }
    (out, dropped)
}

/// The bytes of a header value as recorded: UTF-8 with invalid bytes replaced.
#[must_use]
pub fn header_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_lists_every_field_as_required() {
        let s = schema();
        let required: Vec<&str> = s["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        for (name, _, _) in FIELDS {
            assert!(s["properties"].get(name).is_some(), "{name}");
            assert!(required.contains(name), "{name}");
        }
        assert_eq!(
            s["properties"]["attempts"]["items"]["required"]
                .as_array()
                .unwrap()
                .len(),
            ATTEMPT_FIELDS.len()
        );
    }

    #[test]
    fn bodies_are_truncated_at_utf8_boundaries() {
        let small = body(br#"{"a":1}"#, None, 100);
        assert_eq!(small["body"]["a"], 1);
        assert_eq!(small["truncated"], false);
        let big = "é".repeat(10);
        let b = body(big.as_bytes(), None, 5);
        assert_eq!(b["body"], "éé");
        assert_eq!(b["truncated"], true);
        assert_eq!(b["original_bytes"], 20);
    }

    #[test]
    fn metadata_is_flattened_and_bounded() {
        let meta: Map<String, Value> = serde_json::from_str(
            r#"{"bad key":"x","team":"search","n":3,"obj":{"a":[1,true]},"multi":"ééééé"}"#,
        )
        .unwrap();
        let (m, dropped) = metadata(&meta, 10, 64);
        assert_eq!(dropped, 1);
        assert_eq!(m["team"], "search");
        assert_eq!(m["n"], "3");
        assert_eq!(m["obj"], r#"{"a":[1,true]}"#);
        let (m, dropped) = metadata(&meta, 2, 8);
        assert_eq!(dropped, 3);
        assert_eq!(m["multi"], "éééé");
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn header_values_are_lossy() {
        assert_eq!(header_text(&[0x4f, 0x6c, 0xe1]), "Ol\u{FFFD}");
    }
}
