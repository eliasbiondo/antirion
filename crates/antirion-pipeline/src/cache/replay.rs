//! A hit: the stored response, as JSON or as a stream in the client's dialect, with the routing
//! identity recorded when it was written and the cache headers.

use antirion_cache::{Entry, StoredEvent};
use antirion_core::headers as h;
use bytes::{BufMut, Bytes, BytesMut};
use http::header::{CACHE_CONTROL, CONTENT_TYPE};
use http::{HeaderName, HeaderValue, Response, StatusCode};
use serde_json::{Value, json};

use super::{AGE, Inputs, KEY, SIMILARITY, STATUS, TYPE};
use crate::ctx::{Body, Ctx, full};
use crate::relay::{self, StreamFacts};

/// How the client's dialect frames a stream.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Framing {
    /// `data:` events ending with `data: [DONE]`.
    Chat,
    Completions,
    /// Named events whose last one ends the stream.
    Anthropic,
    Responses,
}

fn framing(endpoint_id: &str) -> Option<Framing> {
    Some(match endpoint_id {
        "chat_completions" => Framing::Chat,
        "completions" => Framing::Completions,
        "messages" => Framing::Anthropic,
        "responses" => Framing::Responses,
        _ => return None,
    })
}

fn frame(out: &mut BytesMut, event: Option<&str>, data: &str) {
    if let Some(e) = event {
        out.put_slice(b"event: ");
        out.put_slice(e.as_bytes());
        out.put_u8(b'\n');
    }
    for line in data.split('\n') {
        out.put_slice(b"data: ");
        out.put_slice(line.as_bytes());
        out.put_u8(b'\n');
    }
    out.put_u8(b'\n');
}

/// A Chat event's payload as a request without `include_usage` sees it: the usage-only chunk
/// dropped, usage removed from the others.
fn without_usage(data: &str) -> Option<String> {
    let Ok(mut v) = serde_json::from_str::<Value>(data) else {
        return Some(data.to_owned());
    };
    if v.get("usage").is_none_or(Value::is_null) {
        return Some(data.to_owned());
    }
    if v.get("choices")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
    {
        return None;
    }
    if let Some(o) = v.as_object_mut() {
        o.shift_remove("usage");
    }
    serde_json::to_string(&v).ok()
}

/// The usage-only chunk of a Chat stream, after its last event.
fn usage_chunk(events: &[StoredEvent], usage: &Value) -> Option<String> {
    let last: Value = serde_json::from_str(&events.last()?.data).ok()?;
    let chunk = json!({
        "id": last.get("id").cloned().unwrap_or(Value::Null),
        "object": "chat.completion.chunk",
        "created": last.get("created").cloned().unwrap_or(Value::Null),
        "model": last.get("model").cloned().unwrap_or(Value::Null),
        "choices": [],
        "usage": usage,
    });
    serde_json::to_string(&chunk).ok()
}

/// The stream of an entry written from a stream: the same events, usage as the request asks.
fn replay_events(f: Framing, events: &[StoredEvent], usage: Option<&Value>, wants_usage: bool) -> Bytes {
    let mut out = BytesMut::new();
    let chat = f == Framing::Chat;
    let mut has_usage = false;
    for e in events {
        let data = if chat && !wants_usage {
            match without_usage(&e.data) {
                Some(d) => d,
                None => continue,
            }
        } else {
            e.data.clone()
        };
        has_usage |= chat && data.contains("\"usage\"");
        frame(&mut out, e.event.as_deref(), &data);
    }
    if chat
        && wants_usage
        && !has_usage
        && let Some(chunk) = usage.and_then(|u| usage_chunk(events, u))
    {
        frame(&mut out, None, &chunk);
    }
    if matches!(f, Framing::Chat | Framing::Completions) {
        frame(&mut out, None, "[DONE]");
    }
    out.freeze()
}

/// The stream of an entry written from a complete response.
fn synthesize(f: Framing, body: &str, wants_usage: bool) -> Option<Bytes> {
    let v: Value = serde_json::from_str(body).ok()?;
    let mut facts = StreamFacts::default();
    let events = match f {
        Framing::Chat | Framing::Completions => relay::synthesize(&v, None, wants_usage, &mut facts)?,
        Framing::Anthropic => relay::synth::anthropic(&v, &mut facts)?,
        Framing::Responses => relay::synth::responses(&v, &mut facts)?,
    };
    let mut out = BytesMut::new();
    for e in events {
        out.put_slice(&e);
    }
    if matches!(f, Framing::Chat | Framing::Completions) {
        frame(&mut out, None, "[DONE]");
    }
    Some(out.freeze())
}

/// The response of a hit (a semantic one with its `similarity`), or `None` when the entry cannot
/// answer this request (a stream-written entry with no assembled body, asked for JSON).
pub(super) fn respond(
    cx: &Ctx,
    inp: &Inputs<'_>,
    entry: &Entry,
    key: &str,
    now_ms: i64,
    similarity: Option<f64>,
) -> Option<Response<Body>> {
    let (body, content_type) = if inp.stream {
        let f = framing(inp.endpoint.id)?;
        let bytes = match (&entry.events, &entry.body) {
            (Some(events), _) => replay_events(f, events, entry.usage.as_ref(), inp.wants_usage),
            (None, Some(body)) => synthesize(f, body, inp.wants_usage)?,
            (None, None) => return None,
        };
        (bytes, "text/event-stream; charset=utf-8")
    } else {
        (Bytes::from(entry.body.clone()?), entry.content_type.as_str())
    };
    let mut resp = Response::new(full(body));
    *resp.status_mut() = StatusCode::OK;
    let headers = resp.headers_mut();
    let mut set = |name: &'static str, value: &str| {
        if let Ok(v) = HeaderValue::from_str(value) {
            headers.insert(HeaderName::from_static(name), v);
        }
    };
    set(h::RESPONSE_PATH, "cache");
    set(h::PROVIDER, &entry.provider);
    set(h::MODEL, &entry.model);
    set(h::DEPLOYMENT, &entry.deployment);
    set(h::UPSTREAM_STATUS, &entry.upstream_status.to_string());
    set(h::ATTEMPTS, "0");
    if let Some(d) = &entry.upstream_dialect {
        set("x-antirion-upstream-dialect", d);
    }
    set(STATUS, "hit");
    match similarity {
        Some(sim) => {
            set(TYPE, "semantic");
            set(SIMILARITY, &format!("{sim:.4}"));
        }
        None => set(TYPE, "exact"),
    }
    set(AGE, &entry.age_secs(now_ms).to_string());
    set(KEY, key);
    if let Ok(ct) = HeaderValue::from_str(content_type) {
        headers.insert(CONTENT_TYPE, ct);
    }
    if inp.stream {
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    }
    let usage = entry
        .usage
        .as_ref()
        .map(|u| crate::apis::usage_of(&json!({"usage": u})))
        .unwrap_or_default();
    let response_id = entry
        .body
        .as_deref()
        .and_then(|b| serde_json::from_str::<Value>(b).ok())
        .and_then(|v| v.get("id").and_then(Value::as_str).map(str::to_owned));
    cx.log.update(|r| {
        r.usage = usage;
        r.response_model = Some(entry.model.clone());
        r.response_id = response_id;
        r.cache.origin = Some(entry.deployment.clone());
    });
    cx.finish(&mut resp, false);
    Some(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(data: &str) -> StoredEvent {
        StoredEvent {
            event: None,
            data: data.to_owned(),
        }
    }

    #[test]
    fn chat_usage_follows_the_request() {
        let events = [
            ev(r#"{"id":"c","choices":[{"index":0,"delta":{"content":"hi"},"finish_reason":"stop"}]}"#),
            ev(r#"{"id":"c","choices":[],"usage":{"total_tokens":3}}"#),
        ];
        let usage = json!({"total_tokens": 3});
        let with = replay_events(Framing::Chat, &events, Some(&usage), true);
        let without = replay_events(Framing::Chat, &events, Some(&usage), false);
        assert_eq!(String::from_utf8_lossy(&with).matches("data: ").count(), 3);
        assert!(!String::from_utf8_lossy(&without).contains("usage"));
        assert!(without.ends_with(b"data: [DONE]\n\n"));
        let added = replay_events(Framing::Chat, &events[..1], Some(&usage), true);
        assert!(String::from_utf8_lossy(&added).contains("\"choices\":[],"));
    }

    #[test]
    fn named_events_keep_their_names() {
        let events = [StoredEvent {
            event: Some("message_stop".into()),
            data: r#"{"type":"message_stop"}"#.into(),
        }];
        let out = replay_events(Framing::Anthropic, &events, None, false);
        assert_eq!(
            &out[..],
            b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
        );
    }
}
