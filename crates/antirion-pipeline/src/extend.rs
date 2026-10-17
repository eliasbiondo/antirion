//! Extensions of one inference request: `request` hooks before the input guardrails, `response`
//! hooks after the output guardrails (in reverse order), `attempt` hooks before each upstream send,
//! each call bounded by its timeout with its failure policy, and the `respond` short circuit
//! rendered in the client's dialect. `stream_chunk` hooks run in the stream relay
//! (`relay/chunks.rs`).

pub(crate) mod attempt;
pub(crate) mod carrier;

use std::sync::Arc;
use std::time::{Duration, Instant};

use antirion_core::{Clock, Stage, SystemClock, headers as h};
use antirion_dialects::Endpoint;
use antirion_error::{GatewayError, Message};
use antirion_ext::api::Input;
use antirion_ext::{Carrier, Hook, HookAction, HookFacts, Plugin, Reply};
use antirion_obs::metrics::Fam;
use antirion_runtime::extstats::CallEnd;
use bytes::Bytes;
use http::header::{CACHE_CONTROL, CONTENT_TYPE};
use http::{HeaderName, HeaderValue, Response, StatusCode};
use serde_json::{Map, Value, json};

use crate::ctx::{Body, Ctx, full};
use crate::reasons;
use crate::relay::StreamFacts;

/// The extensions attached to one request, in `request`-hook order.
#[derive(Debug)]
pub(crate) struct Hooks {
    pub(crate) list: Vec<Arc<Plugin>>,
    pub(crate) facts: HookFacts,
}

/// What the hook payloads of a request say about it.
pub(crate) struct HookInputs<'a> {
    pub(crate) parts: &'a http::request::Parts,
    pub(crate) endpoint: &'a Endpoint,
    pub(crate) auth: &'a crate::auth::Authenticated,
    pub(crate) model: &'a str,
    pub(crate) route: Option<&'a str>,
    pub(crate) stream: bool,
    pub(crate) end_user: Option<&'a str>,
}

fn dialect_label(endpoint: &Endpoint) -> &'static str {
    use antirion_dialects::Dialect;
    match endpoint.dialect {
        Dialect::AnthropicMessages => "anthropic",
        Dialect::Gemini => "gemini",
        Dialect::BedrockConverse | Dialect::BedrockInvoke => "bedrock",
        _ => "openai",
    }
}

/// The extensions attached to a request (`policies.plugins` at each level, broadest first);
/// `None` when there is none.
pub(crate) fn hooks_for(cx: &Ctx, levels: &[Option<&Value>], i: &HookInputs<'_>) -> Option<Hooks> {
    let list = antirion_ext::resolve(&cx.snap.plugins, levels);
    if list.is_empty() {
        return None;
    }
    let wanted: Vec<&String> = list
        .iter()
        .flat_map(|p| match &p.carrier {
            Carrier::Webhook { forward_headers, .. } => forward_headers.iter().collect(),
            Carrier::Builtin(_) | Carrier::Wasm(_) => Vec::new(),
        })
        .collect();
    let headers = wanted
        .into_iter()
        .filter_map(|n| {
            let v = i.parts.headers.get(n.as_str())?.to_str().ok()?;
            Some((n.clone(), v.to_owned()))
        })
        .collect();
    let key = &i.auth.key;
    let tags: Vec<String> = cx
        .snap
        .identity
        .chain(key)
        .iter()
        .flatten()
        .flat_map(|t| t.tags.iter().cloned())
        .chain(key.tags.iter().cloned())
        .chain(i.auth.controls.tags.iter().cloned())
        .collect();
    let facts = HookFacts {
        request_id: cx.request_id.as_str().to_owned(),
        log_id: cx.log_id.clone(),
        identity: json!({
            "organization": key.organization,
            "team": key.team,
            "project": key.project,
            "key": key.name,
            "end_user": i.end_user,
            "tags": tags,
            "metadata": i.auth.controls.metadata.clone().unwrap_or_else(|| json!({})),
        }),
        model: i.model.to_owned(),
        route: i.route.map(str::to_owned),
        operation: i.endpoint.id.to_owned(),
        dialect: dialect_label(i.endpoint).to_owned(),
        stream: i.stream,
        headers,
    };
    Some(Hooks { list, facts })
}

/// How the `request` hooks ended.
#[derive(Debug)]
pub(crate) enum RequestEnd {
    Continue,
    Respond(Reply),
}

/// The `respond` usage and stop facts the log records.
pub(crate) fn reply_usage(cx: &Ctx, reply: &Reply) {
    cx.log.update(|r| {
        r.usage.input = Some(reply.input);
        r.usage.output = Some(reply.output);
        r.finish_reasons = vec![reply.finish_reason.clone()];
    });
}

pub(crate) fn record(
    cx: &Ctx,
    p: &Plugin,
    hook: Hook,
    result: &str,
    started: Instant,
    failed_open: bool,
    error: Option<&str>,
) {
    let end = Instant::now();
    let took = end.saturating_duration_since(started);
    cx.log.update(|r| {
        r.extensions.push(json!({
            "name": p.name,
            "hook": hook.as_str(),
            "action": result,
            "duration_ms": antirion_obs::timing::ms3(took.as_secs_f64() * 1000.0),
            "failed_open": failed_open,
        }));
        r.internal_spans.push(
            crate::reqlog::InternalSpan::new(format!("antirion.extension {}", p.name), started, end)
                .attr("antirion.extension.hook", hook.as_str()),
        );
        if failed_open && !r.degraded.iter().any(|(c, _)| *c == "extensions") {
            r.degraded.push(("extensions", Some("extension_error")));
        }
    });
    if result == "skipped" {
        return;
    }
    cx.rt
        .metrics
        .add(Fam::ExtensionCalls, &[&p.name, hook.as_str(), result], 1.0);
    cx.rt.metrics.observe(
        Fam::ExtensionDuration,
        &[&p.name, hook.as_str()],
        took.as_secs_f64(),
    );
    let ms = took.as_secs_f64() * 1000.0;
    let call_end = match result {
        "timeout" => CallEnd::TimedOut,
        "error" => CallEnd::Failed,
        _ => CallEnd::Ok,
    };
    cx.rt.state.extensions.record(&p.name, ms, call_end, error);
}

pub(crate) fn stage_of(hook: Hook) -> Stage {
    match hook {
        Hook::Request => Stage::GuardInput,
        Hook::Attempt => Stage::Attempt,
        _ => Stage::GuardOutput,
    }
}

pub(crate) fn unavailable(hook: Hook) -> GatewayError {
    let mut e = GatewayError::with_reason(
        reasons::EXTENSION_UNAVAILABLE,
        stage_of(hook),
        Message::Fixed("An extension could not be called."),
    );
    e.retry_after = Some(Duration::from_secs(1));
    e
}

pub(crate) fn denied(hook: Hook, status: u16, message: &str) -> GatewayError {
    let reason = if status == 403 {
        reasons::EXTENSION_DENIED
    } else {
        reasons::EXTENSION_REJECTED
    };
    GatewayError::with_reason(reason, stage_of(hook), Message::built(message.to_owned()))
}

/// One hook call with its failure policy: the action to apply, or `None` when it failed open.
pub(crate) async fn run_one(
    cx: &Ctx,
    p: &Plugin,
    input: &Input<'_>,
) -> Result<Option<HookAction>, GatewayError> {
    let started = Instant::now();
    let hook = input.hook;
    let shown = input.with_body(input.body.filter(|_| p.include_body));
    match carrier::invoke(cx, p, &shown).await {
        Ok(a) => {
            record(cx, p, hook, a.name(), started, false, None);
            Ok(Some(a))
        }
        Err(f) => {
            let (result, why) = (f.result(), f.text());
            tracing::warn!(extension = %p.name, hook = hook.as_str(), error = %why, "extension call failed");
            if p.fail_open {
                record(cx, p, hook, result, started, true, Some(&why));
                cx.guard.extensions_degraded();
                Ok(None)
            } else {
                record(cx, p, hook, result, started, false, Some(&why));
                Err(unavailable(hook))
            }
        }
    }
}

/// A patch that failed to apply is a hook failure under the extension's policy.
pub(crate) fn patch_failed(cx: &Ctx, p: &Plugin, hook: Hook) -> Result<(), GatewayError> {
    if p.fail_open {
        cx.guard.extensions_degraded();
        cx.log.update(|r| {
            if let Some(last) = r.extensions.last_mut().and_then(Value::as_object_mut) {
                last.insert("failed_open".into(), Value::Bool(true));
            }
            if !r.degraded.iter().any(|(c, _)| *c == "extensions") {
                r.degraded.push(("extensions", Some("extension_error")));
            }
        });
        Ok(())
    } else {
        Err(unavailable(hook))
    }
}

pub(crate) fn merge_metadata(cx: &Ctx, metadata: Map<String, Value>) {
    if metadata.is_empty() {
        return;
    }
    cx.log.update(|r| r.metadata.extend(metadata));
}

pub(crate) fn header_pairs(set: Vec<(String, String)>) -> Vec<(HeaderName, HeaderValue)> {
    set.into_iter()
        .filter_map(|(k, v)| {
            Some((
                HeaderName::from_bytes(k.as_bytes()).ok()?,
                HeaderValue::from_str(&v).ok()?,
            ))
        })
        .collect()
}

/// Runs the `request` hooks in order. Mutations change `body` (when JSON) and add the upstream
/// headers sent on every attempt; a `respond` or a `deny` stops the remaining hooks.
pub(crate) async fn request(
    cx: &Ctx,
    hooks: &Hooks,
    body: &mut Option<Value>,
    upstream_headers: &mut Vec<(HeaderName, HeaderValue)>,
) -> Result<(RequestEnd, bool), GatewayError> {
    let mut changed = false;
    for p in hooks.list.iter().filter(|p| p.runs(Hook::Request)) {
        let input = Input::new(Hook::Request, &p.name, &p.config, &hooks.facts).with_body(body.as_ref());
        let Some(action) = run_one(cx, p, &input).await? else {
            continue;
        };
        match action {
            HookAction::Continue => {}
            HookAction::Mutate {
                patch,
                set_headers,
                metadata,
            } => {
                if !patch.is_empty() {
                    match body.as_ref().map(|b| antirion_ext::patch::apply(b, &patch)) {
                        Some(Ok(patched)) => {
                            *body = Some(patched);
                            changed = true;
                        }
                        _ => {
                            patch_failed(cx, p, Hook::Request)?;
                            continue;
                        }
                    }
                }
                upstream_headers.extend(header_pairs(set_headers));
                merge_metadata(cx, metadata);
            }
            HookAction::Deny { status, message } => return Err(denied(Hook::Request, status, &message)),
            HookAction::Respond(reply) => return Ok((RequestEnd::Respond(reply), changed)),
        }
    }
    Ok((RequestEnd::Continue, changed))
}

/// Records the `response` hooks of a streaming request as not run.
pub(crate) fn skip_response(cx: &Ctx, hooks: &Hooks) {
    for p in hooks.list.iter().rev().filter(|p| p.runs(Hook::Response)) {
        record(cx, p, Hook::Response, "skipped", Instant::now(), false, None);
    }
}

/// Whether any attached extension has a `response` hook.
pub(crate) fn has_response(hooks: &Hooks) -> bool {
    hooks.list.iter().any(|p| p.runs(Hook::Response))
}

/// Runs the `response` hooks in reverse order over a client-dialect body; mutations change it and
/// add response headers. Returns whether the body changed.
pub(crate) async fn response(
    cx: &Ctx,
    hooks: &Hooks,
    status: u16,
    body: &mut Value,
    response_headers: &mut Vec<(HeaderName, HeaderValue)>,
    attempts: u16,
) -> Result<bool, GatewayError> {
    let mut changed = false;
    for p in hooks.list.iter().rev().filter(|p| p.runs(Hook::Response)) {
        let input = Input::new(Hook::Response, &p.name, &p.config, &hooks.facts).with_response(status, body);
        let action = run_one(cx, p, &input).await.map_err(|mut e| {
            e.attempts = attempts;
            e
        })?;
        let Some(action) = action else { continue };
        match action {
            HookAction::Continue | HookAction::Respond(_) => {}
            HookAction::Mutate {
                patch,
                set_headers,
                metadata,
            } => {
                if !patch.is_empty() {
                    match antirion_ext::patch::apply(body, &patch) {
                        Ok(patched) => {
                            *body = patched;
                            changed = true;
                        }
                        Err(_) => {
                            patch_failed(cx, p, Hook::Response).map_err(|mut e| {
                                e.attempts = attempts;
                                e
                            })?;
                            continue;
                        }
                    }
                }
                response_headers.extend(header_pairs(set_headers));
                merge_metadata(cx, metadata);
            }
            HookAction::Deny { status, message } => {
                let mut e = denied(Hook::Response, status, &message);
                e.attempts = attempts;
                return Err(e);
            }
        }
    }
    Ok(changed)
}

fn now_secs() -> i64 {
    SystemClock.now().0 / 1_000_000
}

fn new_id(prefix: &str) -> String {
    format!(
        "{prefix}{}",
        antirion_core::RequestId::generate_unguessable()
            .as_str()
            .replace('-', "")
    )
}

/// A `respond` reply as a complete response body in the client's dialect.
fn reply_body(endpoint: &Endpoint, reply: &Reply, model: &str) -> Value {
    let (input, output) = (reply.input, reply.output);
    let fr = reply.finish_reason.as_str();
    match endpoint.id {
        "messages" => json!({
            "id": new_id("msg_"), "type": "message", "role": "assistant", "model": model,
            "content": [{"type": "text", "text": reply.content}],
            "stop_reason": match fr { "length" => "max_tokens", "content_filter" => "refusal", _ => "end_turn" },
            "stop_sequence": null,
            "usage": {"input_tokens": input, "output_tokens": output},
        }),
        "responses" => json!({
            "id": new_id("resp_"), "object": "response", "created_at": now_secs(),
            "status": if fr == "stop" { "completed" } else { "incomplete" },
            "incomplete_details": if fr == "stop" { Value::Null } else {
                json!({"reason": if fr == "length" { "max_output_tokens" } else { "content_filter" }})
            },
            "model": model,
            "output": [{"type": "message", "id": new_id("msg_"), "status": "completed", "role": "assistant",
                        "content": [{"type": "output_text", "text": reply.content, "annotations": []}]}],
            "usage": {"input_tokens": input, "output_tokens": output, "total_tokens": input.saturating_add(output)},
        }),
        "gemini_generate" => json!({
            "candidates": [{"content": {"role": "model", "parts": [{"text": reply.content}]},
                            "finishReason": match fr { "length" => "MAX_TOKENS", "content_filter" => "SAFETY", _ => "STOP" },
                            "index": 0}],
            "usageMetadata": {"promptTokenCount": input, "candidatesTokenCount": output,
                              "totalTokenCount": input.saturating_add(output)},
            "modelVersion": model,
        }),
        "bedrock_converse" | "bedrock_invoke" => json!({
            "output": {"message": {"role": "assistant", "content": [{"text": reply.content}]}},
            "stopReason": match fr { "length" => "max_tokens", "content_filter" => "content_filtered", _ => "end_turn" },
            "usage": {"inputTokens": input, "outputTokens": output, "totalTokens": input.saturating_add(output)},
            "metrics": {"latencyMs": 0},
        }),
        "completions" => json!({
            "id": new_id("cmpl-"), "object": "text_completion", "created": now_secs(), "model": model,
            "choices": [{"index": 0, "text": reply.content, "finish_reason": fr, "logprobs": null}],
            "usage": {"prompt_tokens": input, "completion_tokens": output, "total_tokens": input.saturating_add(output)},
        }),
        _ => json!({
            "id": new_id("chatcmpl-"), "object": "chat.completion", "created": now_secs(), "model": model,
            "choices": [{"index": 0, "message": {"role": "assistant", "content": reply.content}, "finish_reason": fr}],
            "usage": {"prompt_tokens": input, "completion_tokens": output, "total_tokens": input.saturating_add(output)},
        }),
    }
}

/// The client response of a `respond`: the reply in the client's dialect, streamed when the client
/// asked to stream.
pub(crate) fn respond(
    cx: &Ctx,
    endpoint: &Endpoint,
    reply: &Reply,
    model: &str,
    stream: bool,
    wants_usage: bool,
) -> Response<Body> {
    reply_usage(cx, reply);
    let body = reply_body(endpoint, reply, model);
    let mut facts = StreamFacts::default();
    let frames = match endpoint.id {
        _ if !stream => None,
        "messages" => crate::relay::synth::anthropic(&body, &mut facts),
        "responses" => crate::relay::synth::responses(&body, &mut facts),
        "gemini_generate" => crate::relay::synth::gemini(&body, &mut facts),
        "bedrock_converse" | "bedrock_invoke" => None,
        _ => crate::relay::synthesize(&body, None, wants_usage, &mut facts).map(|mut f| {
            f.push(Bytes::from_static(b"data: [DONE]\n\n"));
            f
        }),
    };
    let (bytes, content_type) = match frames {
        Some(f) => (Bytes::from(f.concat()), "text/event-stream; charset=utf-8"),
        None => (
            Bytes::from(serde_json::to_vec(&body).unwrap_or_default()),
            "application/json",
        ),
    };
    let mut resp = Response::new(full(bytes));
    *resp.status_mut() = StatusCode::OK;
    let hdrs = resp.headers_mut();
    hdrs.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    if content_type != "application/json" {
        hdrs.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    }
    hdrs.insert(
        HeaderName::from_static(h::RESPONSE_PATH),
        HeaderValue::from_static("extension"),
    );
    cx.finish(&mut resp, false);
    resp
}
