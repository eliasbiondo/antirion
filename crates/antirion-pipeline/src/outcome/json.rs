//! A non-streaming JSON answer: decoded, checked for refusals and missing members, and returned
//! with the requested model name.

use antirion_egress::InboundResponse;
use antirion_error::reason::hygiene;
use antirion_json::{JsonLimits, Splice, skim};
use antirion_resilience::{Class, Failure};
use bytes::Bytes;
use http::header::CONTENT_TYPE;
use http::{HeaderName, HeaderValue, Response, StatusCode};
use serde_json::Value;

use crate::apis;
use crate::attempt::{Done, essence};
use crate::cell::Cell;
use crate::ctx::full;
use crate::relay::ClientDialect;

use super::{Attempt, is_json, response_head, response_model};

/// Whether a 2xx answer is a refusal: a `content_filter` finish or an Anthropic `refusal` stop.
fn refusal(v: &Value) -> bool {
    v.get("stop_reason").and_then(Value::as_str) == Some("refusal")
        || v.get("choices").and_then(Value::as_array).is_some_and(|cs| {
            cs.iter()
                .any(|c| c.get("finish_reason").and_then(Value::as_str) == Some("content_filter"))
        })
}

/// What an unusable body of `api` lacks, for the client's message.
fn missing_member(api: apis::Api) -> &'static str {
    match api {
        apis::Api::Chat | apis::Api::Completions => "upstream response missing 'choices'",
        apis::Api::Embeddings => "upstream response missing 'data'",
        apis::Api::Moderations => "upstream response missing 'results'",
        apis::Api::Anthropic => "upstream response missing 'content'",
        apis::Api::Gemini => "upstream response missing 'candidates'",
        apis::Api::Converse => "upstream response missing 'output'",
        apis::Api::AnthropicCount | apis::Api::ResponsesCount => "upstream response missing 'input_tokens'",
        apis::Api::GeminiCount => "upstream response missing 'totalTokens'",
        _ => "upstream response is not a json object",
    }
}

/// A Gemini answer that rejected the replayed history for a missing thought signature: the
/// rejected-replay class, so the one repair re-send follows. The answer came with a 2xx status, so the
/// client sees 400, the status of the request Gemini refused.
pub(crate) fn missing_signature(a: &Attempt<'_>) -> Failure {
    let class = Class::ReasoningReplayRejected;
    let mut f = a.fail(
        class,
        class.final_code(),
        None,
        Some(400),
        antirion_error::Message::Fixed("The upstream rejected the request: a thought signature is missing."),
    );
    if let Some(u) = f.error.upstream.as_mut() {
        u.error = Some(Box::new(antirion_error::UpstreamError {
            kind: Some(crate::relay::native::MISSING_THOUGHT_SIGNATURE.to_owned()),
            ..antirion_error::UpstreamError::default()
        }));
    }
    f
}

/// A non-streaming JSON response: decoded, checked, and returned with the requested model name.
pub(crate) async fn json_response(
    a: &Attempt<'_>,
    up: InboundResponse,
    api: apis::Api,
) -> Result<(Done, Class), Failure> {
    let status = up.status.as_u16();
    let ct = essence(&up.headers);
    if !is_json(ct.as_deref()) {
        return Err(a.bad(
            hygiene::UNEXPECTED_CONTENT_TYPE,
            status,
            format!(
                "upstream answered with content-type {}, which this endpoint does not produce",
                ct.as_deref().unwrap_or("none")
            ),
        ));
    }
    let content_type = up.headers.get(CONTENT_TYPE).cloned();
    let up_headers = up.headers.clone();
    let raw = match a.read(up.body, a.body_cap()).await? {
        Ok(b) => b,
        Err(why) => return Err(a.unread(why, status)),
    };
    let Ok(v) = serde_json::from_slice::<Value>(&raw) else {
        return Err(a.bad(
            hygiene::INVALID_BODY,
            status,
            "upstream response is not valid json".to_owned(),
        ));
    };
    if let Some(err) = v.get("error").filter(|e| e.is_object()) {
        return Err(a.error_object(err));
    }
    if !apis::usable(api, &v) {
        return Err(a.bad(hygiene::INVALID_BODY, status, missing_member(api).to_owned()));
    }
    if api == apis::Api::Gemini && crate::relay::native::missing_thought_signature(&v) {
        return Err(missing_signature(a));
    }
    if a.predicates
        .iter()
        .any(antirion_resilience::ResponsePredicate::needs_body)
        && antirion_resilience::predicate::answer_matches(a.predicates, &apis::answer_facts(&v))
    {
        let reported = apis::usage_of(&v);
        if let Some(u) = v
            .get("usage")
            .or_else(|| v.get("usageMetadata"))
            .filter(|_| !reported.is_empty())
        {
            let raw = u.clone();
            a.cx.log.update(|r| r.pending_error_usage = Some((reported, raw)));
        }
        return Err(a.predicate_matched(status));
    }
    let upstream_model = v
        .get("model")
        .or_else(|| v.get("modelVersion"))
        .and_then(Value::as_str)
        .unwrap_or(&a.deployment.model)
        .to_owned();
    let shown = response_model(a, &upstream_model);
    let (translated, warnings) = crate::xlate::response(a, api, &v, &shown, &up_headers, status)?;
    deliver(
        a,
        Answer {
            status,
            raw,
            facts: v,
            shown,
            translated,
            warnings,
            content_type,
            up_headers,
        },
    )
    .await
}

/// A complete answer on its way to the client: the upstream's bytes, the body its facts are read
/// from, and the client body when it differs from the upstream's.
pub(crate) struct Answer {
    pub(crate) status: u16,
    pub(crate) raw: Bytes,
    pub(crate) facts: Value,
    /// The model name the client sees.
    pub(crate) shown: String,
    pub(crate) translated: Result<Option<Value>, ()>,
    pub(crate) warnings: Option<antirion_dialects::xlate::warn::Warnings>,
    pub(crate) content_type: Option<HeaderValue>,
    pub(crate) up_headers: http::HeaderMap,
}

/// Logs a complete answer's facts, stores Responses state, runs the output guardrails and the
/// response hooks, writes the cache and builds the client's response.
pub(crate) async fn deliver(a: &Attempt<'_>, ans: Answer) -> Result<(Done, Class), Failure> {
    let Answer {
        status,
        raw,
        facts: v,
        shown,
        mut translated,
        mut warnings,
        content_type,
        up_headers,
    } = ans;
    let class = if refusal(&v) {
        Class::ContentPolicy
    } else {
        Class::Success
    };
    if class == Class::ContentPolicy && a.refusal_fails {
        return Err(crate::attempt::refused(a.deployment, a.attempts));
    }
    let mut usage = apis::usage_of(&v);
    if usage.output.is_none() && usage.total.is_none() {
        usage.output = Some(apis::output_chars(&v).div_ceil(4));
        usage.estimated = true;
    }
    a.cx.log.update(|r| {
        r.usage = usage;
        r.usage_raw = v.get("usage").or_else(|| v.get("usageMetadata")).cloned();
        r.units.web_searches = v
            .pointer("/usage/server_tool_use/web_search_requests")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        r.finish_reasons = apis::finish_reasons(&v);
        if let Some(t) = apis::service_tier(&v) {
            r.service_tier = Some(t.to_owned());
        }
        r.tool_call_count = Some(apis::tool_calls(&v));
        r.response_model = v
            .get("model")
            .or_else(|| v.get("modelVersion"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        r.response_id = v
            .get("id")
            .or_else(|| v.get("responseId"))
            .and_then(Value::as_str)
            .map(str::to_owned);
    });

    let mut store_ignored = false;
    if let Some(turn) = &a.plan.turn {
        match (a.cell, &mut translated) {
            (Cell::Identity, _) => {
                if let Some(id) = v.get("id").and_then(Value::as_str) {
                    Box::pin(crate::respstate::link(a.cx, turn, id, &a.deployment.name)).await;
                }
            }
            (
                Cell::Translate {
                    client: ClientDialect::Responses,
                    ..
                },
                Ok(Some(t)),
            ) => {
                let id = crate::respstate::new_id();
                if let Some(o) = t.as_object_mut() {
                    o.insert("id".into(), Value::from(id.as_str()));
                }
                store_ignored = !Box::pin(crate::respstate::keep(&a.cx.rt, turn, &id, t)).await;
            }
            _ => {}
        }
    }
    let body = match &translated {
        Ok(None) => rewrite_model(&raw, v.get("model").and_then(Value::as_str), &shown),
        Ok(Some(t)) => Bytes::from(serde_json::to_vec(t).unwrap_or_default()),
        Err(()) => {
            return Err(a.bad(
                hygiene::INVALID_BODY,
                status,
                "upstream response cannot be translated".to_owned(),
            ));
        }
    };
    let (body, hook_headers) = match Box::pin(crate::guard::complete_response(a, status, body)).await {
        Ok(x) => x,
        Err(e) => {
            a.record(Some(status), class, None);
            return Err(crate::streamed::guarded(status, e));
        }
    };
    a.cx.log.response_content(&body);
    let content_type = match translated {
        Ok(Some(_)) => None,
        _ => content_type,
    }
    .unwrap_or(HeaderValue::from_static("application/json"));
    if let Some(slot) = &a.plan.cache {
        crate::cache::store_response(a, slot, status, &content_type, &body, &up_headers).await;
    }
    a.record(Some(status), class, None);
    let mut resp = Response::new(full(body));
    *resp.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
    resp.headers_mut().insert(CONTENT_TYPE, content_type);
    for (n, v) in hook_headers {
        resp.headers_mut().insert(n, v);
    }
    let mut entries = warnings.take().map(|w| w.entries()).unwrap_or_default();
    if store_ignored {
        entries.push("ignored=store".to_owned());
    }
    if let Some(v) =
        antirion_dialects::xlate::warn::header_value(&entries).and_then(|v| HeaderValue::from_str(&v).ok())
    {
        resp.headers_mut()
            .insert(HeaderName::from_static("x-antirion-translation-warnings"), v);
    }
    a.cx.log.update(|r| r.translation_warnings = entries);
    if a.plan.endpoint.id == "count_tokens" || a.plan.endpoint.action == Some("input_tokens") {
        resp.headers_mut().insert(
            HeaderName::from_static(antirion_core::headers::TOKEN_COUNT_SOURCE),
            HeaderValue::from_static("upstream"),
        );
    }
    response_head(a, &mut resp, status, &up_headers);
    Ok((Done::Response(resp), Class::Success))
}

/// The body with its top-level `model` replaced by the requested name, when they differ.
fn rewrite_model(raw: &Bytes, current: Option<&str>, requested: &str) -> Bytes {
    if current.is_none_or(|c| c == requested) {
        return raw.clone();
    }
    let Ok(sk) = skim(raw, &JsonLimits::default()) else {
        return raw.clone();
    };
    let Some(m) = sk.member("model") else {
        return raw.clone();
    };
    let mut splice = Splice::default();
    splice.replace_span(
        m.value.clone(),
        Bytes::from(serde_json::to_vec(requested).unwrap_or_default()),
    );
    splice.apply(raw)
}
