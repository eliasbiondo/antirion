//! Writes: whether a response may be stored, and the write itself, inline on the memory backend
//! and on a tracked task otherwise.

use std::sync::Arc;

use antirion_cache::Entry;
use antirion_cache::vectors::Item;
use antirion_obs::metrics::Fam;
use antirion_runtime::Runtime;
use bytes::Bytes;
use http::{HeaderMap, HeaderValue};
use serde_json::Value;

use super::CacheSlot;
use crate::outcome::Attempt;
use crate::reqlog::Draft;

/// The routing identity a hit replays.
#[derive(Clone, Debug)]
pub(super) struct Identity {
    pub(super) provider: String,
    pub(super) deployment: String,
    pub(super) model: String,
    pub(super) status: u16,
    pub(super) upstream_dialect: String,
}

impl Identity {
    pub(super) fn of(a: &Attempt<'_>, status: u16) -> Identity {
        let (_, upstream) = crate::apis::dialect_names(a.plan.endpoint, a.cell);
        Identity {
            provider: a.deployment.provider.name.clone(),
            deployment: a.deployment.name.clone(),
            model: a.deployment.model.clone(),
            status,
            upstream_dialect: upstream.to_owned(),
        }
    }

    pub(super) fn entry(
        self,
        slot: &CacheSlot,
        now_ms: i64,
        body: Option<String>,
        events: Option<Vec<antirion_cache::StoredEvent>>,
        usage: Option<Value>,
    ) -> Entry {
        Entry {
            written_at_ms: now_ms,
            ttl_ms: u64::try_from(slot.ttl.as_millis()).unwrap_or(u64::MAX),
            content_type: "application/json".to_owned(),
            body,
            events,
            provider: self.provider,
            deployment: self.deployment,
            model: self.model,
            upstream_status: self.status,
            upstream_dialect: Some(self.upstream_dialect),
            usage,
            meta: None,
        }
    }
}

/// The attempt's response may be stored at all: a final 200 from the deployment the request
/// first targeted (unless the policy stores after a fallback), and no upstream `no-store`.
pub(super) fn attempt_storable(a: &Attempt<'_>, slot: &CacheSlot, status: u16, upstream: &HeaderMap) -> bool {
    status == 200
        && (a.deployment.name == a.first || slot.store_after_fallback)
        && !upstream
            .get_all(http::header::CACHE_CONTROL)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .any(|d| d.trim().eq_ignore_ascii_case("no-store"))
}

/// An output guardrail that ran gave a verdict other than pass or redact.
pub(super) fn guard_flagged(log: &Draft) -> bool {
    log.read(|r| {
        r.guardrails.iter().any(|g| {
            g.get("stage").and_then(Value::as_str) == Some("output")
                && !matches!(
                    g.get("verdict").and_then(Value::as_str),
                    Some("pass" | "redact" | "skipped")
                )
        })
    })
    .unwrap_or(false)
}

fn arguments_parse(args: Option<&Value>) -> bool {
    match args {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).is_ok(),
        _ => true,
    }
}

/// The body is a complete answer worth replaying: no content filter or refusal, and every tool
/// call's arguments are JSON.
pub(super) fn body_storable(v: &Value) -> bool {
    let list = |v: &Value, name: &str| -> Vec<Value> {
        v.get(name).and_then(Value::as_array).cloned().unwrap_or_default()
    };
    if v.get("stop_reason").and_then(Value::as_str) == Some("refusal") {
        return false;
    }
    if v.get("status").and_then(Value::as_str) == Some("incomplete")
        && v.pointer("/incomplete_details/reason").and_then(Value::as_str) == Some("content_filter")
    {
        return false;
    }
    for choice in list(v, "choices") {
        if choice.get("finish_reason").and_then(Value::as_str) == Some("content_filter") {
            return false;
        }
        let calls = choice
            .pointer("/message/tool_calls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !calls
            .iter()
            .all(|c| arguments_parse(c.pointer("/function/arguments")))
        {
            return false;
        }
    }
    list(v, "output").iter().all(|item| {
        item.get("type").and_then(Value::as_str) != Some("function_call")
            || arguments_parse(item.get("arguments"))
    })
}

/// Writes `entry`, and its vector when the slot has one: inline on the memory backend, so it is
/// visible before the response ends; on a tracked task on a shared backend, which keeps the slot's
/// flight until the write is done. Failures are counted, never surfaced.
pub(super) async fn write(rt: &Arc<Runtime>, slot: &CacheSlot, mut entry: Entry) {
    let now_ms = rt.now().0 / 1000;
    let mut meta = (*slot.meta).clone();
    if slot.semantic.is_some() {
        meta.mode = antirion_cache::Mode::Semantic;
    }
    (meta.input_tokens, meta.output_tokens) = antirion_cache::meta::tokens(entry.usage.as_ref());
    entry.meta = Some(meta);
    let vector = slot.semantic.as_ref().map(|w| {
        (
            w.partition.clone(),
            Item {
                scope: slot.scope.clone(),
                key: slot.key.clone(),
                vector: Arc::clone(&w.vector),
                polarity: w.polarity.clone(),
                written_at_ms: entry.written_at_ms,
                expires_at_ms: entry.expires_at_ms(),
            },
        )
    });
    let index = move |rt: &Runtime, ok: bool| {
        if let (true, Some((partition, item))) = (ok, vector) {
            rt.state.cache.vectors.insert(&partition, item);
        }
    };
    if rt.state.cache.local() {
        let result = rt.state.cache.write(&slot.scope, &slot.key, entry, now_ms).await;
        index(rt, result.is_ok());
        count(rt, result.is_ok());
        return;
    }
    let rt2 = Arc::clone(rt);
    let (scope, key) = (slot.scope.clone(), slot.key.clone());
    let flight = slot.flight.clone();
    rt.tasks.spawn(async move {
        let result = rt2.state.cache.write(&scope, &key, entry, now_ms).await;
        if let Err(e) = &result {
            tracing::warn!(error = %e, "a cache entry could not be written");
        }
        index(&rt2, result.is_ok());
        count(&rt2, result.is_ok());
        drop(flight);
    });
}

fn count(rt: &Runtime, ok: bool) {
    rt.metrics.add(
        Fam::CacheWrites,
        &["exact", if ok { "stored" } else { "error" }],
        1.0,
    );
}

/// Counts a response that had a slot but was not storable.
pub(super) fn skipped(rt: &Runtime) {
    rt.metrics.add(Fam::CacheWrites, &["exact", "skipped"], 1.0);
}

/// Stores a complete non-streaming response the client received, when it is storable.
pub(crate) async fn store_response(
    a: &Attempt<'_>,
    slot: &CacheSlot,
    status: u16,
    content_type: &HeaderValue,
    body: &Bytes,
    upstream: &HeaderMap,
) {
    match response_entry(a, slot, status, content_type, body, upstream) {
        Some(entry) => write(&a.cx.rt, slot, entry).await,
        None => skipped(&a.cx.rt),
    }
}

fn response_entry(
    a: &Attempt<'_>,
    slot: &CacheSlot,
    status: u16,
    content_type: &HeaderValue,
    body: &Bytes,
    upstream: &HeaderMap,
) -> Option<Entry> {
    if !attempt_storable(a, slot, status, upstream)
        || u64::try_from(body.len()).unwrap_or(u64::MAX) > slot.max_entry_size
        || guard_flagged(&a.cx.log)
    {
        return None;
    }
    let text = std::str::from_utf8(body).ok()?;
    let v = serde_json::from_str::<Value>(text).ok()?;
    if !body_storable(&v) {
        return None;
    }
    let now_ms = a.cx.rt.now().0 / 1000;
    let mut entry =
        Identity::of(a, status).entry(slot, now_ms, Some(text.to_owned()), None, v.get("usage").cloned());
    if let Ok(ct) = content_type.to_str() {
        ct.clone_into(&mut entry.content_type);
    }
    Some(entry)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn refusals_and_broken_tool_calls_are_not_storable() {
        assert!(body_storable(&json!({"choices": [{"finish_reason": "stop"}]})));
        assert!(!body_storable(
            &json!({"choices": [{"finish_reason": "content_filter"}]})
        ));
        assert!(!body_storable(&json!({"stop_reason": "refusal"})));
        assert!(!body_storable(
            &json!({"status": "incomplete", "incomplete_details": {"reason": "content_filter"}})
        ));
        let call = |args: &str| {
            json!({"choices": [{"finish_reason": "tool_calls",
            "message": {"tool_calls": [{"function": {"name": "f", "arguments": args}}]}}]})
        };
        assert!(body_storable(&call("{\"a\":1}")));
        assert!(!body_storable(&call("{\"a\":")));
        assert!(!body_storable(
            &json!({"output": [{"type": "function_call", "arguments": "{"}]})
        ));
    }
}
