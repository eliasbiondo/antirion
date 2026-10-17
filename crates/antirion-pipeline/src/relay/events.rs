//! What each upstream event means for the stream lifecycle: content or preamble, an error, a
//! terminator, the usage it carries, and whether the client sees it.

use std::collections::BTreeMap;

use serde_json::Value;

use super::pump::UpEvent;
use crate::reqlog::Usage;

/// The upstream dialect of a relayed stream.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum StreamDialect {
    /// Chat Completions and legacy Completions share the chunk grammar.
    Chat,
    Responses,
    Anthropic,
    Gemini,
    /// Bedrock ConverseStream (AWS event-stream).
    Converse,
    /// Bedrock InvokeModel with response stream: Anthropic events in `chunk` messages.
    InvokeAnthropic,
    /// Bedrock InvokeModel with response stream and a model-native body.
    Invoke,
    /// Cohere v2 Chat events.
    Cohere,
}

impl StreamDialect {
    /// Whether the upstream frames its stream as AWS event-stream messages.
    pub(crate) fn event_stream(self) -> bool {
        matches!(
            self,
            StreamDialect::Converse | StreamDialect::InvokeAnthropic | StreamDialect::Invoke
        )
    }
}

/// One classified upstream event.
#[derive(Debug)]
pub(crate) enum Kind {
    /// Held before commit; forwarded unchanged after.
    Preamble,
    /// Triggers the commit.
    Content,
    /// The dialect's success terminator.
    Terminator,
    /// Carries nothing; never forwarded.
    Empty,
    /// An upstream error, with its effective status, type and message.
    Error {
        status: u16,
        kind: Option<String>,
        message: Option<String>,
    },
    /// Not one JSON object.
    Malformed,
}

/// A classified event and how it is relayed.
#[derive(Debug)]
pub(crate) struct Classified {
    pub(crate) kind: Kind,
    /// Forwarded to the client (false for events the client did not ask for).
    pub(crate) forward: bool,
    pub(crate) json: Option<Value>,
}

/// Per-stream facts accumulated from the events.
#[derive(Debug, Default)]
pub(crate) struct StreamFacts {
    /// Choice index → whether it has received a finish reason.
    pub(crate) choices: BTreeMap<u64, bool>,
    pub(crate) finish_reasons: Vec<String>,
    pub(crate) usage: Usage,
    /// The last usage object the upstream reported, as reported.
    pub(crate) usage_raw: Option<Value>,
    pub(crate) usage_chunk_seen: bool,
    /// A native stream reached the point where a clean end completes it (a stop reason, a
    /// `finishReason`, `messageStop`).
    pub(crate) native_finished: bool,
    /// Characters of output seen, for an estimate when no usage is reported.
    pub(crate) output_chars: u64,
    pub(crate) id: Option<String>,
    pub(crate) created: Option<i64>,
    pub(crate) model: Option<String>,
    /// The last `sequence_number` of a Responses stream.
    pub(crate) sequence: Option<i64>,
    /// The response id a Responses stream announced in `response.created`.
    pub(crate) response_id: Option<String>,
    /// Tool calls the stream started.
    pub(crate) tool_calls: u64,
    /// The service tier the stream's events named.
    pub(crate) service_tier: Option<String>,
}

impl StreamFacts {
    /// Every choice seen has finished (and there was at least one).
    pub(crate) fn all_finished(&self) -> bool {
        !self.choices.is_empty() && self.choices.values().all(|f| *f)
    }

    /// Whether a clean end of the upstream completes the stream (its completion criteria).
    pub(crate) fn complete_at_clean_end(&self, dialect: StreamDialect) -> bool {
        match dialect {
            StreamDialect::Chat => self.all_finished(),
            StreamDialect::Responses => false,
            StreamDialect::Invoke => true,
            _ => self.native_finished,
        }
    }
}

pub(super) fn nonempty(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(_) => true,
    }
}

/// Whether an event ends the answer with a refusal: an Anthropic `refusal` stop, or a Chat
/// `content_filter` finish carrying no content.
pub(crate) fn refusal(json: &Value) -> bool {
    if json.pointer("/delta/stop_reason").and_then(Value::as_str) == Some("refusal") {
        return true;
    }
    json.get("choices").and_then(Value::as_array).is_some_and(|cs| {
        cs.iter().any(|c| {
            c.get("finish_reason").and_then(Value::as_str) == Some("content_filter") && !carries_output(json)
        })
    })
}

/// Whether an upstream event carries output content: text, a tool-call fragment, audio or reasoning.
pub(crate) fn carries_output(json: &Value) -> bool {
    const DELTAS: [&str; 6] = [
        "content",
        "reasoning_content",
        "reasoning",
        "refusal",
        "tool_calls",
        "audio",
    ];
    let choices = json
        .get("choices")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    for c in choices {
        let delta = c.get("delta");
        if DELTAS.iter().any(|n| nonempty(delta.and_then(|d| d.get(*n)))) || nonempty(c.get("text")) {
            return true;
        }
    }
    let typed = json.get("type").and_then(Value::as_str).unwrap_or_default();
    if typed.ends_with(".delta") && nonempty(json.get("delta")) {
        return true;
    }
    if let Some(d) = json.get("delta").filter(|_| typed == "content_block_delta") {
        return ["text", "thinking", "partial_json"]
            .iter()
            .any(|n| nonempty(d.get(*n)));
    }
    let parts = json
        .get("candidates")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| c.pointer("/content/parts").and_then(Value::as_array))
        .flatten();
    for p in parts {
        if nonempty(p.get("text")) || nonempty(p.get("functionCall")) {
            return true;
        }
    }
    if matches!(typed, "content-delta" | "tool-call-start" | "tool-call-delta") {
        return nonempty(json.pointer("/delta/message"));
    }
    json.pointer("/contentBlockDelta/delta")
        .is_some_and(|d| nonempty(d.get("text")) || nonempty(d.get("toolUse")))
}

pub(super) fn openai_usage(u: &Value) -> Usage {
    Usage::openai(u)
}

/// The effective status of an OpenAI-dialect error object.
pub(crate) fn effective_status(err: &Value) -> u16 {
    let as_status = |v: Option<&Value>| {
        v.and_then(Value::as_u64)
            .and_then(|n| u16::try_from(n).ok())
            .filter(|n| (400..=599).contains(n))
    };
    if let Some(s) = as_status(err.get("status")).or_else(|| as_status(err.get("code"))) {
        return s;
    }
    let code = err.get("code").and_then(Value::as_str).unwrap_or_default();
    let kind = err.get("type").and_then(Value::as_str).unwrap_or_default();
    match (kind, code) {
        (_, "invalid_api_key") => 401,
        (_, "model_not_found") => 404,
        (_, "insufficient_quota") | ("insufficient_quota", _) => 402,
        ("invalid_request_error", _) => 400,
        ("authentication_error", _) => 401,
        ("permission_error", _) => 403,
        ("not_found_error", _) => 404,
        ("rate_limit_error" | "rate_limit_exceeded" | "requests" | "tokens", _)
        | (_, "rate_limit_exceeded") => 429,
        ("server_error", _) => 500,
        ("service_unavailable" | "overloaded" | "overloaded_error", _) => 503,
        _ => 502,
    }
}

fn responses_failed_status(resp: &Value) -> u16 {
    match resp
        .get("error")
        .and_then(|e| e.get("code"))
        .and_then(Value::as_str)
    {
        Some("rate_limit_exceeded") => 429,
        Some("server_error") => 500,
        Some("invalid_prompt") => 400,
        _ => 502,
    }
}

pub(super) fn message_of(err: &Value) -> Option<String> {
    err.get("message").and_then(Value::as_str).map(str::to_owned)
}

/// Classifies one event and updates the stream's facts.
pub(crate) fn classify(
    dialect: StreamDialect,
    ev: &UpEvent,
    client_wants_usage: bool,
    facts: &mut StreamFacts,
) -> Classified {
    if facts.service_tier.is_none()
        && ev.data.contains("\"service_tier\"")
        && let Ok(v) = serde_json::from_str::<Value>(ev.data.trim())
    {
        facts.service_tier = crate::apis::service_tier(&v).map(str::to_owned);
    }
    if dialect.event_stream() || matches!(dialect, StreamDialect::Anthropic | StreamDialect::Gemini) {
        return super::native::classify(dialect, ev, facts);
    }
    let data = ev.data.trim_matches(|c: char| c.is_ascii_whitespace());
    let plain = |kind| Classified {
        kind,
        forward: false,
        json: None,
    };
    if dialect == StreamDialect::Chat && data == "[DONE]" {
        return Classified {
            kind: Kind::Terminator,
            forward: true,
            json: None,
        };
    }
    if data.is_empty() {
        return plain(Kind::Empty);
    }
    let Ok(json) = serde_json::from_str::<Value>(data) else {
        return plain(Kind::Malformed);
    };
    if !json.is_object() {
        return plain(Kind::Malformed);
    }
    match dialect {
        StreamDialect::Responses => classify_responses(ev, json, facts),
        StreamDialect::Cohere => classify_cohere(json, facts),
        _ => classify_chat(ev.event.as_deref(), json, client_wants_usage, facts),
    }
}

pub(crate) fn classify_chat(
    ev: Option<&str>,
    json: Value,
    client_wants_usage: bool,
    facts: &mut StreamFacts,
) -> Classified {
    if let Some(err) = json.get("error").filter(|e| !e.is_null()) {
        return Classified {
            kind: Kind::Error {
                status: effective_status(err),
                kind: err.get("type").and_then(Value::as_str).map(str::to_owned),
                message: message_of(err).or_else(|| err.as_str().map(str::to_owned)),
            },
            forward: false,
            json: None,
        };
    }
    if ev == Some("error") {
        return Classified {
            kind: Kind::Error {
                status: 502,
                kind: None,
                message: message_of(&json),
            },
            forward: false,
            json: None,
        };
    }
    if facts.id.is_none() {
        facts.id = json.get("id").and_then(Value::as_str).map(str::to_owned);
        facts.created = json.get("created").and_then(Value::as_i64);
        facts.model = json.get("model").and_then(Value::as_str).map(str::to_owned);
    }
    let usage = json.get("usage").filter(|u| !u.is_null());
    if let Some(u) = usage {
        facts.usage.merge(openai_usage(u));
        facts.usage_raw = Some(u.clone());
    }
    let choices = json.get("choices").and_then(Value::as_array);
    let empty_choices = choices.is_none_or(|c| c.is_empty());
    let mut content = usage.is_some();
    for choice in choices.into_iter().flatten() {
        let index = choice.get("index").and_then(Value::as_u64).unwrap_or(0);
        let finished = choice.get("finish_reason").is_some_and(|f| !f.is_null());
        let entry = facts.choices.entry(index).or_insert(false);
        if finished {
            *entry = true;
            if let Some(f) = choice.get("finish_reason").and_then(Value::as_str) {
                facts.finish_reasons.push(f.to_owned());
            }
            content = true;
        }
        let delta = choice.get("delta");
        let field = |name: &str| delta.and_then(|d| d.get(name));
        facts.tool_calls += field("tool_calls").and_then(Value::as_array).map_or(0, |calls| {
            calls
                .iter()
                .filter(|c| c.get("id").is_some_and(|i| !i.is_null()))
                .count() as u64
        });
        for name in ["content", "reasoning_content", "reasoning", "refusal"] {
            if let Some(s) = field(name).and_then(Value::as_str) {
                facts.output_chars += u64::try_from(s.chars().count()).unwrap_or(u64::MAX);
            }
        }
        if let Some(t) = choice.get("text").and_then(Value::as_str) {
            facts.output_chars += u64::try_from(t.chars().count()).unwrap_or(u64::MAX);
        }
        if [
            "content",
            "reasoning_content",
            "reasoning",
            "refusal",
            "tool_calls",
            "audio",
        ]
        .iter()
        .any(|n| nonempty(field(n)))
            || nonempty(choice.get("text"))
        {
            content = true;
        }
    }
    let usage_chunk = empty_choices && usage.is_some();
    if usage_chunk {
        facts.usage_chunk_seen = true;
    }
    let forward = if empty_choices {
        usage_chunk && client_wants_usage
    } else {
        true
    };
    Classified {
        kind: if content { Kind::Content } else { Kind::Preamble },
        forward,
        json: Some(json),
    }
}

fn classify_cohere(json: Value, facts: &mut StreamFacts) -> Classified {
    let kind = json.get("type").and_then(Value::as_str).unwrap_or_default();
    let class = match kind {
        "message-end" => {
            if let Some(counts) = json
                .pointer("/delta/usage/tokens")
                .or_else(|| json.pointer("/delta/usage/billed_units"))
            {
                facts.usage.merge(crate::apis::cohere_usage(counts));
                facts.usage_raw = json.pointer("/delta/usage").cloned();
            }
            if let Some(r) = json.pointer("/delta/finish_reason").and_then(Value::as_str) {
                facts.finish_reasons.push(r.to_owned());
            }
            facts.native_finished = true;
            Kind::Terminator
        }
        "content-delta" | "tool-call-delta" | "tool-call-start" => {
            if kind == "tool-call-start" {
                facts.tool_calls += 1;
            }
            for p in [
                "/delta/message/content/text",
                "/delta/message/tool_calls/function/arguments",
            ] {
                if let Some(t) = json.pointer(p).and_then(Value::as_str) {
                    facts.output_chars += u64::try_from(t.chars().count()).unwrap_or(u64::MAX);
                }
            }
            Kind::Content
        }
        "" if json.get("message").is_some() => Kind::Error {
            status: 502,
            kind: None,
            message: message_of(&json),
        },
        _ => Kind::Preamble,
    };
    if facts.id.is_none() && kind == "message-start" {
        facts.id = json.get("id").and_then(Value::as_str).map(str::to_owned);
    }
    Classified {
        kind: class,
        forward: true,
        json: Some(json),
    }
}

fn classify_responses(ev: &UpEvent, json: Value, facts: &mut StreamFacts) -> Classified {
    let kind = json
        .get("type")
        .and_then(Value::as_str)
        .or(ev.event.as_deref())
        .unwrap_or("message")
        .to_owned();
    if kind == "response.output_item.added"
        && json
            .pointer("/item/type")
            .and_then(Value::as_str)
            .is_some_and(|t| t.ends_with("_call") && !t.starts_with("reasoning"))
    {
        facts.tool_calls += 1;
    }
    if kind == "response.created" && facts.response_id.is_none() {
        facts.response_id = json
            .get("response")
            .and_then(|r| r.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    let class = match kind.as_str() {
        "error" => Kind::Error {
            status: effective_status(&json),
            kind: json.get("code").and_then(Value::as_str).map(str::to_owned),
            message: message_of(&json),
        },
        "response.failed" => {
            let resp = json.get("response").cloned().unwrap_or(Value::Null);
            Kind::Error {
                status: responses_failed_status(&resp),
                kind: resp
                    .get("error")
                    .and_then(|e| e.get("code"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                message: resp.get("error").and_then(message_of),
            }
        }
        "response.completed" | "response.incomplete" => {
            if let Some(u) = json
                .get("response")
                .and_then(|r| r.get("usage"))
                .filter(|u| !u.is_null())
            {
                facts.usage.merge(openai_usage(u));
                facts.usage_raw = Some(u.clone());
            }
            Kind::Terminator
        }
        "response.created"
        | "response.queued"
        | "response.in_progress"
        | "response.output_item.added"
        | "response.content_part.added"
        | "response.reasoning_summary_part.added" => Kind::Preamble,
        _ => {
            if let Some(d) = json.get("delta").and_then(Value::as_str) {
                facts.output_chars += u64::try_from(d.chars().count()).unwrap_or(u64::MAX);
            }
            Kind::Content
        }
    };
    let forward = !matches!(class, Kind::Error { .. });
    if forward && let Some(n) = json.get("sequence_number").and_then(Value::as_i64) {
        facts.sequence = Some(n);
    }
    Classified {
        kind: class,
        forward,
        json: Some(json),
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use serde_json::json;

    use super::*;

    fn ev(data: &str) -> UpEvent {
        UpEvent {
            raw: Bytes::new(),
            event: None,
            data: data.to_owned(),
            frame: super::super::pump::Frame::Event,
        }
    }

    #[test]
    fn chat_chunks() {
        let mut f = StreamFacts::default();
        let role = r#"{"id":"c","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}"#;
        assert!(matches!(
            classify(StreamDialect::Chat, &ev(role), false, &mut f).kind,
            Kind::Preamble
        ));
        let text = r#"{"choices":[{"index":0,"delta":{"content":"hi"},"finish_reason":null}]}"#;
        assert!(matches!(
            classify(StreamDialect::Chat, &ev(text), false, &mut f).kind,
            Kind::Content
        ));
        assert!(!f.all_finished());
        let fin = r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#;
        classify(StreamDialect::Chat, &ev(fin), false, &mut f);
        assert!(f.all_finished());
        let usage = r#"{"choices":[],"usage":{"prompt_tokens":11,"completion_tokens":4}}"#;
        let c = classify(StreamDialect::Chat, &ev(usage), false, &mut f);
        assert!(!c.forward);
        assert_eq!(f.usage.output, Some(4));
        assert!(classify(StreamDialect::Chat, &ev(usage), true, &mut f).forward);
        assert!(!classify(StreamDialect::Chat, &ev(r#"{"choices":[]}"#), true, &mut f).forward);
        assert!(matches!(
            classify(StreamDialect::Chat, &ev(" [DONE] "), false, &mut f).kind,
            Kind::Terminator
        ));
        assert!(matches!(
            classify(StreamDialect::Chat, &ev("{broken"), false, &mut f).kind,
            Kind::Malformed
        ));
        assert!(matches!(
            classify(StreamDialect::Chat, &ev(""), false, &mut f).kind,
            Kind::Empty
        ));
    }

    #[test]
    fn error_statuses() {
        assert_eq!(effective_status(&json!({"type": "server_error"})), 500);
        assert_eq!(
            effective_status(&json!({"type": "invalid_request_error", "code": "invalid_api_key"})),
            401
        );
        assert_eq!(effective_status(&json!({"code": 429})), 429);
        assert_eq!(effective_status(&json!({"type": "weird"})), 502);
        let mut f = StreamFacts::default();
        let c = classify(
            StreamDialect::Chat,
            &ev(r#"{"error":{"message":"boom","type":"server_error"}}"#),
            false,
            &mut f,
        );
        assert!(matches!(c.kind, Kind::Error { status: 500, .. }));
    }
}
