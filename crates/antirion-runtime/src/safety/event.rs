//! What a request's record becomes for safety: an event per non-pass verdict and the per-hour counts
//! of every evaluation, and how an event is shown.

use antirion_core::UtcMicros;
use antirion_store::SafetyEventRow;
use serde_json::{Value, json};

/// What the record's entries do not carry: per entry (by index into `guardrails`), a digest of the
/// matched text and the gateway keys verified as exposed in it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mark {
    pub entry: usize,
    pub digest: Option<[u8; 32]>,
    /// The key ids of verified gateway virtual keys found in the text.
    pub exposed: Vec<String>,
}

/// The verdicts that make an event.
const FINDINGS: [&str; 6] = ["block", "redact", "flag", "divert", "error", "error_open"];

fn s<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// An entry's verdict, with an asynchronous guardrail's result in place of `async`.
#[must_use]
pub fn verdict_of(entry: &Value) -> &str {
    match s(entry, "verdict") {
        Some("async") => s(entry, "result").unwrap_or("async"),
        Some(v) => v,
        None => "pass",
    }
}

/// Whether a verdict makes an event.
#[must_use]
pub fn is_finding(verdict: &str) -> bool {
    FINDINGS.contains(&verdict)
}

/// The event action of a verdict.
fn action_of(verdict: &str) -> Option<&'static str> {
    Some(match verdict {
        "block" => "block",
        "redact" => "redact",
        "flag" => "flag",
        "divert" => "divert",
        "error" | "error_open" => "error",
        _ => return None,
    })
}

/// The category an `error` event reports: the one its type finds.
#[must_use]
pub fn type_category(guardrail_type: &str) -> &'static str {
    match guardrail_type {
        "regex" => "deny_pattern",
        "keywords" => "keyword",
        "pii" => "pii",
        "secrets" => "secret",
        "json_schema" => "schema_violation",
        "tool_permission" => "tool_not_allowed",
        "length" => "length",
        _ => "custom",
    }
}

/// The entity counts of an entry's spans, by entity, sorted.
#[must_use]
pub fn entities(entry: &Value) -> Vec<(String, u64)> {
    let mut out: Vec<(String, u64)> = Vec::new();
    for sp in entry.get("spans").and_then(Value::as_array).into_iter().flatten() {
        let Some(e) = s(sp, "entity") else { continue };
        let e = e.to_ascii_lowercase();
        match out.iter_mut().find(|(n, _)| *n == e) {
            Some((_, c)) => *c += 1,
            None => out.push((e, 1)),
        }
    }
    out.sort();
    out
}

/// What an event is built from besides the record.
#[derive(Debug)]
pub(crate) struct Facts<'a> {
    pub(crate) id: String,
    pub(crate) node_id: &'a str,
    pub(crate) guardrail_type: String,
    pub(crate) fingerprint: String,
}

/// The event of entry `entry` of `record`, when its verdict is a finding or a failure.
pub(crate) fn row(record: &Value, entry: &Value, facts: Facts<'_>) -> Option<SafetyEventRow> {
    let verdict = verdict_of(entry);
    if !FINDINGS.contains(&verdict) {
        return None;
    }
    let action = action_of(verdict)?;
    let category = match (action, s(entry, "category")) {
        ("error", _) | (_, None | Some("error")) => type_category(&facts.guardrail_type).to_owned(),
        (_, Some(c)) => c.to_owned(),
    };
    let entities: Vec<Value> = entities(entry)
        .into_iter()
        .map(|(t, c)| json!({"type": t, "count": c}))
        .collect();
    let field = |k: &str| s(record, k).map(str::to_owned);
    Some(SafetyEventRow {
        id: facts.id,
        time_us: s(record, "started_at")
            .and_then(|t| UtcMicros::parse_rfc3339(t).ok())
            .map_or(0, |t| t.0),
        log_id: field("id").unwrap_or_default(),
        request_id: field("request_id").unwrap_or_default(),
        node_id: facts.node_id.to_owned(),
        guardrail: s(entry, "name").unwrap_or_default().to_owned(),
        guardrail_type: facts.guardrail_type,
        stage: s(entry, "stage").unwrap_or_default().to_owned(),
        action: action.to_owned(),
        category,
        severity: s(entry, "severity").unwrap_or("high").to_owned(),
        confidence: entry.get("confidence").and_then(Value::as_f64).unwrap_or(1.0),
        score: entry.get("score").and_then(Value::as_f64),
        entities: Value::Array(entities).to_string(),
        spans: entry
            .get("spans")
            .and_then(Value::as_array)
            .map(|a| lowercase_entities(a).to_string()),
        fingerprint: facts.fingerprint,
        organization: field("organization"),
        team: field("team"),
        project: field("project"),
        key: field("key"),
        end_user: field("end_user"),
        model: field("model"),
        provider: field("provider"),
        latency_ms: entry.get("duration_ms").and_then(Value::as_f64),
        review_state: "open".to_owned(),
        reviewed_by: None,
        reviewed_us: None,
        note: None,
    })
}

/// Spans with their entity in lowercase.
fn lowercase_entities(spans: &[Value]) -> Value {
    Value::Array(
        spans
            .iter()
            .map(|sp| {
                let mut sp = sp.clone();
                if let Some(o) = sp.as_object_mut()
                    && let Some(e) = o
                        .get("entity")
                        .and_then(Value::as_str)
                        .map(str::to_ascii_lowercase)
                {
                    o.insert("entity".into(), Value::String(e));
                }
                sp
            })
            .collect(),
    )
}

/// What the fingerprint is computed over: the guardrail, the category and the normalised match
/// (its digest), else the entity types found.
#[must_use]
pub fn fingerprint_input(
    guardrail: &str,
    category: &str,
    digest: Option<&[u8; 32]>,
    entities: &[(String, u64)],
) -> String {
    let matched = match digest {
        Some(d) => d.iter().map(|b| format!("{b:02x}")).collect::<String>(),
        None => entities
            .iter()
            .map(|(t, _)| t.as_str())
            .collect::<Vec<_>>()
            .join(","),
    };
    format!("safety\u{1f}{guardrail}\u{1f}{category}\u{1f}{matched}")
}

/// An event as the Admin API shows it; spans and score only for readers of content.
#[must_use]
pub fn event_json(e: &SafetyEventRow, content: bool) -> Value {
    let parse = |t: &str| serde_json::from_str::<Value>(t).unwrap_or(Value::Null);
    json!({
        "id": e.id,
        "log_id": e.log_id,
        "request_id": e.request_id,
        "time": UtcMicros(e.time_us).to_rfc3339_micros(),
        "guardrail": e.guardrail,
        "guardrail_type": e.guardrail_type,
        "stage": e.stage,
        "action": e.action,
        "category": e.category,
        "severity": e.severity,
        "confidence": e.confidence,
        "entities": parse(&e.entities),
        "spans": if content { e.spans.as_deref().map_or(Value::Array(Vec::new()), parse) } else { Value::Null },
        "score": if content { json!(e.score) } else { Value::Null },
        "fingerprint": e.fingerprint,
        "organization": e.organization,
        "team": e.team,
        "project": e.project,
        "key": e.key,
        "end_user": e.end_user,
        "model": e.model,
        "provider": e.provider,
        "latency_ms": e.latency_ms,
        "review_state": e.review_state,
        "reviewed_by": e.reviewed_by,
        "reviewed_at": e.reviewed_us.map(|t| UtcMicros(t).to_rfc3339_micros()),
        "note": e.note,
    })
}

/// The data of a `guardrail.event`: ids, category and severity, never matched text.
#[must_use]
pub fn announced(e: &SafetyEventRow) -> Value {
    json!({
        "event": e.id,
        "log_id": e.log_id,
        "request_id": e.request_id,
        "guardrail": e.guardrail,
        "guardrail_type": e.guardrail_type,
        "stage": e.stage,
        "action": e.action,
        "category": e.category,
        "severity": e.severity,
        "fingerprint": e.fingerprint,
        "organization": e.organization,
        "team": e.team,
        "project": e.project,
        "key": e.key,
        "model": e.model,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> Value {
        json!({"id": "log1", "request_id": "r1", "started_at": "2026-01-01T00:00:00Z", "organization": "o",
               "team": "t", "project": "p", "key": "k", "model": "m", "provider": "pv", "end_user": null})
    }

    fn facts() -> Facts<'static> {
        Facts {
            id: "e1".into(),
            node_id: "n",
            guardrail_type: "pii".into(),
            fingerprint: "fp".into(),
        }
    }

    #[test]
    fn a_finding_becomes_an_event_and_a_pass_does_not() {
        let entry = json!({"name": "g", "stage": "input", "verdict": "redact", "category": "pii", "severity": "medium",
            "confidence": 1.0, "score": null, "duration_ms": 0.5,
            "spans": [{"start": 0, "end": 3, "entity": "email"}, {"start": 5, "end": 9, "entity": "email"}]});
        let e = row(&record(), &entry, facts()).unwrap();
        assert_eq!(
            (e.action.as_str(), e.category.as_str(), e.log_id.as_str()),
            ("redact", "pii", "log1")
        );
        let ents: Value = serde_json::from_str(&e.entities).unwrap();
        assert_eq!(ents, json!([{"type": "email", "count": 2}]));
        assert_eq!(e.time_us, 1_767_225_600_000_000);
        let pass = json!({"name": "g", "stage": "input", "verdict": "pass"});
        assert!(row(&record(), &pass, facts()).is_none());
    }

    #[test]
    fn a_failure_reports_its_type_category_and_an_async_result_counts() {
        let failed = json!({"name": "g", "stage": "input", "verdict": "error_open", "category": "error"});
        let e = row(&record(), &failed, facts()).unwrap();
        assert_eq!((e.action.as_str(), e.category.as_str()), ("error", "pii"));
        let later =
            json!({"name": "g", "stage": "output", "verdict": "async", "result": "flag", "category": "pii"});
        assert_eq!(row(&record(), &later, facts()).unwrap().action, "flag");
        let pending = json!({"name": "g", "stage": "output", "verdict": "async"});
        assert!(row(&record(), &pending, facts()).is_none());
    }

    #[test]
    fn readers_without_content_see_no_spans_or_score() {
        let mut e = row(
            &record(),
            &json!({"name": "g", "stage": "input", "verdict": "block", "category": "pii", "score": 0.4, "spans": []}),
            facts(),
        )
        .unwrap();
        e.score = Some(0.4);
        assert!(event_json(&e, false)["spans"].is_null() && event_json(&e, false)["score"].is_null());
        assert_eq!(event_json(&e, true)["score"], 0.4);
        assert!(announced(&e).get("spans").is_none());
    }

    #[test]
    fn the_fingerprint_input_uses_the_digest_else_the_entities() {
        let with = fingerprint_input("g", "pii", Some(&[1u8; 32]), &[]);
        assert!(with.ends_with(&"01".repeat(32)));
        let without = fingerprint_input("g", "pii", None, &[("email".into(), 1), ("phone".into(), 2)]);
        assert!(without.ends_with("email,phone"));
    }
}
