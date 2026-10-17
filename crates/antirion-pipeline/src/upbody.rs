//! The upstream body of one attempt: the client's bytes with the edits identity cells need, or the
//! request translated for the deployment's dialect.

use antirion_core::Stage;
use antirion_egress::Payload;
use antirion_error::{GatewayError, Message};
use antirion_json::Splice;
use antirion_providers::{ClientAnthropic, DeploymentRt, split_betas};
use bytes::Bytes;
use http::HeaderValue;
use serde_json::Value;

use crate::attempt::Plan;
use crate::cell::Cell;
use crate::ctx::Ctx;
use crate::parse::Parsed;
use crate::xlate::AttemptXlate;
use antirion_dialects::xlate::envelope;

/// The JSON body for `deployment`: the client's bytes with only the necessary edits (the upstream
/// model id, and usage requested on streams), sharing the client's bytes around them.
fn json_body(plan: &Plan<'_>, raw: &Bytes, skim: &antirion_json::Skim, d: &DeploymentRt) -> Payload {
    let mut splice = Splice::default();
    let model_json = serde_json::to_vec(&d.model).unwrap_or_default();
    let mut inserts: Vec<u8> = Vec::new();
    match skim.member("model") {
        Some(m) => {
            if raw.get(m.value.clone()) != Some(model_json.as_slice()) {
                splice.replace_span(m.value.clone(), Bytes::from(model_json));
            }
        }
        None if plan.model_absent => {
            inserts.extend_from_slice(b"\"model\":");
            inserts.extend_from_slice(&model_json);
        }
        None => {}
    }
    let usage_dialect = matches!(plan.endpoint.id, "chat_completions" | "completions");
    if plan.stream && usage_dialect && d.capabilities.flag("stream_usage") {
        match skim.member("stream_options") {
            Some(m) => {
                let current: Value = raw
                    .get(m.value.clone())
                    .and_then(|b| serde_json::from_slice(b).ok())
                    .unwrap_or(Value::Null);
                if current.get("include_usage") != Some(&Value::Bool(true)) {
                    let mut obj = current.as_object().cloned().unwrap_or_default();
                    obj.insert("include_usage".into(), Value::Bool(true));
                    splice.replace_span(
                        m.value.clone(),
                        Bytes::from(serde_json::to_vec(&obj).unwrap_or_default()),
                    );
                }
            }
            None => {
                if !inserts.is_empty() {
                    inserts.push(b',');
                }
                inserts.extend_from_slice(br#""stream_options":{"include_usage":true}"#);
            }
        }
    }
    if !inserts.is_empty()
        && let Some(close) = raw.iter().rposition(|b| *b == b'}')
    {
        let mut ins = Vec::with_capacity(inserts.len() + 1);
        if !skim.members.is_empty() {
            ins.push(b',');
        }
        ins.extend_from_slice(&inserts);
        splice.replace_span(close..close, Bytes::from(ins));
    }
    Payload::from_parts(splice.apply_parts(raw))
}

/// The Chat Completions changes a deployment's flags require: `max_tokens` renamed for one that does
/// not accept it, and the `developer` role sent as `system` to one that lacks it.
fn chat_adjust(plan: &Plan<'_>, d: &DeploymentRt, body: Payload) -> Payload {
    if plan.endpoint.id != "chat_completions" {
        return body;
    }
    let caps = &d.capabilities;
    let rename = !caps.flag("accepts_max_tokens");
    let developer = !caps.flag("developer_role");
    let markers = !caps.flag("accepts_cache_control") && body.parts().iter().any(|p| contains_marker(p));
    if !rename && !developer && !markers {
        return body;
    }
    let Ok(mut v) = serde_json::from_slice::<Value>(&body.contiguous()) else {
        return body;
    };
    let mut changed = false;
    if let Some(o) = v.as_object_mut() {
        if rename
            && !o.contains_key("max_completion_tokens")
            && let Some(n) = o.remove("max_tokens")
        {
            o.insert("max_completion_tokens".into(), n);
            changed = true;
        }
        if developer {
            for m in o
                .get_mut("messages")
                .and_then(Value::as_array_mut)
                .into_iter()
                .flatten()
            {
                if m.get("role").and_then(Value::as_str) == Some("developer")
                    && let Some(r) = m.get_mut("role")
                {
                    *r = Value::from("system");
                    changed = true;
                }
            }
        }
    }
    if markers {
        strip_markers(&mut v);
        changed = true;
    }
    if changed {
        Payload::from(serde_json::to_vec(&v).unwrap_or_default())
    } else {
        body
    }
}

/// The upstream body of `parsed` (the client's body, with any route params applied) for `d`, its
/// content type, and, for a translated cell, what the translation produced. `edited` says route
/// params or the reasoning repair changed the client's body, which a translated cell then decodes
/// again; the repair (`repair`) also leaves out what the replay store kept.
pub(crate) async fn upstream_body(
    cx: &Ctx,
    plan: &Plan<'_>,
    parsed: &Parsed,
    d: &DeploymentRt,
    cell: Cell,
    edited: bool,
    repair: bool,
) -> Result<(Payload, Option<HeaderValue>, Option<AttemptXlate>), GatewayError> {
    let json_type = Some(HeaderValue::from_static("application/json"));
    match parsed {
        Parsed::Json { raw, skim, .. } => match (cell, plan.xlate.as_deref()) {
            (Cell::Translate { client, upstream }, Some(x)) => {
                let redecoded = match crate::xlate::client_of(client) {
                    Some(c) if edited => {
                        let mut ir = crate::xlate::decode_body(c, parsed, plan.endpoint)?.0;
                        if let (Some((project, _)), false) = (&x.replay, repair) {
                            let _ = crate::xlate::restore(&cx.rt, project, &mut ir);
                        }
                        Some(ir)
                    }
                    _ => None,
                };
                let ir = redecoded.as_ref().unwrap_or(&x.ir);
                let (encoded, dialect) = crate::xlate::body(cx, x, ir, d, upstream).await?;
                let mut out = encoded.body;
                if plan.endpoint.counts_tokens() {
                    count_body(&mut out);
                }
                let state = AttemptXlate {
                    warnings: encoded.warnings,
                    restore: encoded.restore,
                    upstream: dialect,
                };
                Ok((
                    Payload::from(serde_json::to_vec(&out).unwrap_or_default()),
                    json_type,
                    Some(state),
                ))
            }
            (Cell::Translate { .. } | Cell::Embed(_) | Cell::Rerank(_), None)
            | (Cell::Embed(_) | Cell::Rerank(_), _) => Err(untranslatable()),
            (Cell::InvokeAsMessages, _) => Ok((raw.clone().into(), json_type, None)),
            (Cell::Identity, _) if plan.endpoint.upstream_path.is_some() => Ok((
                chat_adjust(plan, d, json_body(plan, raw, skim, d)),
                json_type,
                None,
            )),
            (Cell::Identity, _) if plan.endpoint.id == "messages" => Ok((
                foreign_reasoning(raw, envelope::unwrap_anthropic_history).into(),
                json_type,
                None,
            )),
            (Cell::Identity, _) if plan.endpoint.dialect == antirion_dialects::Dialect::Gemini => Ok((
                foreign_reasoning(raw, envelope::unwrap_gemini_history).into(),
                json_type,
                None,
            )),
            (Cell::Identity, _) if plan.endpoint.dialect == antirion_dialects::Dialect::BedrockConverse => {
                Ok((
                    foreign_reasoning(raw, envelope::unwrap_converse_history).into(),
                    json_type,
                    None,
                ))
            }
            (Cell::Identity, _) => Ok((raw.clone().into(), json_type, None)),
        },
        Parsed::Multipart { raw, form, rest, .. } => {
            let body = multipart_body(plan, raw, form, d);
            let body = match rest {
                Some(r) => body.with_tail(r.tail()),
                None => body,
            };
            Ok((body, plan.client_content_type.clone(), None))
        }
    }
}

/// A multipart body with its `model` part's value replaced by the upstream id, sharing the
/// client's bytes around it; a request routed without a `model` part (an endpoint default) gets one
/// before its first part.
fn multipart_body(
    plan: &Plan<'_>,
    raw: &Bytes,
    form: &antirion_wire::multipart::Form,
    d: &DeploymentRt,
) -> Payload {
    let model = Bytes::from(d.model.clone());
    if let Some(p) = form.parts().iter().find(|p| p.name.as_deref() == Some("model")) {
        if raw.get(p.value.clone()) == Some(model.as_ref()) {
            return Payload::from(raw.clone());
        }
        return Payload::from_parts(vec![raw.slice(..p.value.start), model, raw.slice(p.value.end..)]);
    }
    let boundary = plan
        .client_content_type
        .as_ref()
        .and_then(|v| v.to_str().ok())
        .and_then(antirion_wire::multipart::form_data_boundary);
    match boundary {
        Some(b) if plan.model_absent => {
            let part = format!("--{b}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\n");
            Payload::from_parts(vec![
                Bytes::from(part),
                model,
                Bytes::from_static(b"\r\n"),
                raw.clone(),
            ])
        }
        _ => Payload::from(raw.clone()),
    }
}

/// A native body whose history carries reasoning wrapped for another family: wrapped payloads of the
/// dialect's own family are unwrapped, others removed (they cannot be replayed).
fn foreign_reasoning(raw: &Bytes, unwrap: fn(&mut Value) -> bool) -> Bytes {
    if !raw.windows(6).any(|w| w == b"\"ant1.") {
        return raw.clone();
    }
    let Ok(mut v) = serde_json::from_slice::<Value>(raw) else {
        return raw.clone();
    };
    if unwrap(&mut v) {
        Bytes::from(serde_json::to_vec(&v).unwrap_or_default())
    } else {
        raw.clone()
    }
}

/// A translated body cut to what the count APIs take: Gemini `contents`; Anthropic's model,
/// messages, system prompt, tools and thinking; or the Responses request without its generation
/// settings.
fn count_body(out: &mut Value) {
    if let Some(contents) = out.get("contents").cloned() {
        *out = serde_json::json!({"contents": contents});
        return;
    }
    let responses = out.get("input").is_some();
    if let Some(o) = out.as_object_mut() {
        o.retain(|k, _| {
            if responses {
                matches!(
                    k.as_str(),
                    "model"
                        | "input"
                        | "instructions"
                        | "tools"
                        | "tool_choice"
                        | "parallel_tool_calls"
                        | "reasoning"
                        | "text"
                )
            } else {
                matches!(
                    k.as_str(),
                    "model" | "messages" | "system" | "tools" | "tool_choice" | "thinking"
                )
            }
        });
    }
}

fn contains_marker(body: &[u8]) -> bool {
    body.windows(15).any(|w| w == b"\"cache_control\"")
}

/// Removes the `cache_control` extension markers a Chat deployment does not accept.
fn strip_markers(v: &mut Value) {
    let strip = |o: &mut Value| {
        if let Some(m) = o.as_object_mut() {
            m.shift_remove("cache_control");
        }
    };
    for m in v
        .get_mut("messages")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        strip(m);
        for p in m
            .get_mut("content")
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
        {
            strip(p);
        }
        for c in m
            .get_mut("tool_calls")
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
        {
            strip(c);
        }
    }
    for t in v
        .get_mut("tools")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        if let Some(f) = t.get_mut("function") {
            strip(f);
        }
    }
}

/// A translated cell without a decoded request: it is never sent.
fn untranslatable() -> GatewayError {
    GatewayError::with_reason(
        crate::reasons::UNTRANSLATABLE_REQUEST,
        Stage::Attempt,
        Message::Fixed("The request cannot be translated for this deployment."),
    )
}

/// The Anthropic headers of an Anthropic-dialect client, forwarded to Anthropic upstreams.
pub(crate) fn client_anthropic(plan: &Plan<'_>) -> Option<ClientAnthropic> {
    if plan.endpoint.dialect != antirion_dialects::Dialect::AnthropicMessages {
        return None;
    }
    let h = plan.client_headers;
    Some(ClientAnthropic {
        version: h
            .get("anthropic-version")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        betas: split_betas(h.get_all("anthropic-beta").iter().filter_map(|v| v.to_str().ok())),
    })
}
