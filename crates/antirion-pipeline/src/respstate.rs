//! Responses continuations: `previous_response_id` pinned to the deployment that created a native
//! id, or expanded from the gateway's stored chain; the stateful fields only native deployments
//! serve; and storing translated responses.

use std::time::Duration;

use antirion_config::scalar::{parse_duration, parse_size};
use antirion_core::Stage;
use antirion_error::{GatewayError, Message};
use antirion_json::{JsonLimits, skim};
use antirion_runtime::responses::{NativeLink, StoredResponse};
use bytes::Bytes;
use serde_json::Value;

use crate::ctx::Ctx;
use crate::parse::Parsed;
use crate::reasons;

/// `api.responses`.
#[derive(Clone, Debug)]
pub(crate) struct Settings {
    pub(crate) gateway: bool,
    pub(crate) ttl: Duration,
    max_bytes: u64,
    max_depth: usize,
}

impl Settings {
    pub(crate) fn of(doc: &Value) -> Settings {
        let r = doc.pointer("/api/responses");
        let get = |k: &str| r.and_then(|r| r.get(k));
        Settings {
            gateway: get("state").and_then(Value::as_str) != Some("native_only"),
            ttl: get("ttl")
                .and_then(Value::as_str)
                .and_then(parse_duration)
                .unwrap_or(Duration::from_secs(30 * 86_400)),
            max_bytes: get("max_stored_bytes")
                .and_then(Value::as_str)
                .and_then(parse_size)
                .unwrap_or(32 << 20),
            max_depth: get("max_chain_depth")
                .and_then(Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(1000),
        }
    }
}

/// What one Responses request carries for state.
#[derive(Clone, Debug)]
pub(crate) struct Turn {
    pub(crate) settings: Settings,
    pub(crate) project: String,
    pub(crate) store: bool,
    /// The gateway-stored response this turn continues.
    pub(crate) previous: Option<String>,
    /// The deployment a native continuation is pinned to.
    pub(crate) pin: Option<String>,
    /// This turn's own input items.
    pub(crate) input: Vec<Value>,
    /// A field only a native deployment can serve, when present.
    pub(crate) stateful: Option<&'static str>,
}

fn previous_not_found() -> GatewayError {
    GatewayError::with_reason(
        reasons::PREVIOUS_RESPONSE_NOT_FOUND,
        Stage::Resolve,
        Message::Fixed("No stored response has the given previous_response_id."),
    )
    .param("previous_response_id")
}

/// The 503 of a durable read while the store is unreachable.
fn store_unavailable() -> GatewayError {
    let mut e = GatewayError::with_reason(
        reasons::STORE_UNAVAILABLE,
        Stage::Resolve,
        Message::Fixed("The store holding this state is unavailable."),
    );
    e.retry_after = Some(Duration::from_secs(1));
    e
}

/// The 404 of a stored-response read for an id the gateway does not hold.
pub(crate) fn response_not_found() -> GatewayError {
    GatewayError::with_reason(
        reasons::RESPONSE_NOT_FOUND,
        Stage::Resolve,
        Message::Fixed("No stored response has that id."),
    )
}

fn input_items(input: Option<&Value>) -> Vec<Value> {
    match input {
        Some(Value::String(s)) => vec![serde_json::json!({"role": "user", "content": s})],
        Some(Value::Array(items)) => items.clone(),
        _ => Vec::new(),
    }
}

/// Reads the state of a Responses request and, for a gateway-stored continuation, rewrites the body
/// so its input starts with the stored conversation.
pub(crate) async fn prepare(cx: &Ctx, project: &str, parsed: &mut Parsed) -> Result<Turn, GatewayError> {
    let settings = Settings::of(&cx.snap.config.doc);
    let Parsed::Json { .. } = parsed else {
        return Err(previous_not_found());
    };
    let mut body: Value = parsed.doc().cloned().unwrap_or(Value::Null);
    let present = |k: &str| {
        body.get(k)
            .is_some_and(|v| !v.is_null() && *v != Value::Bool(false))
    };
    let stateful = ["background", "conversation", "prompt", "context_management"]
        .into_iter()
        .find(|k| present(k));
    let mut turn = Turn {
        settings: settings.clone(),
        project: project.to_owned(),
        store: body.get("store") != Some(&Value::Bool(false)),
        previous: None,
        pin: None,
        input: input_items(body.get("input")),
        stateful,
    };
    let Some(prev) = body
        .get("previous_response_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return Ok(turn);
    };
    let now = cx.rt.now();
    let rt = &cx.rt;
    if let Some(link) = rt
        .native_link(&prev, project, now, settings.ttl)
        .await
        .map_err(|_| store_unavailable())?
    {
        turn.pin = Some(link.deployment);
        return Ok(turn);
    }
    if !settings.gateway {
        return Err(previous_not_found());
    }
    let mut chain: Vec<StoredResponse> = Vec::new();
    let mut next = Some(prev.clone());
    while let Some(id) = next {
        let Some(r) = rt
            .stored_response(&id, project, now, settings.ttl)
            .await
            .map_err(|_| store_unavailable())?
        else {
            return Err(previous_not_found());
        };
        if chain.len() >= settings.max_depth {
            return Err(GatewayError::with_reason(
                reasons::RESPONSE_CHAIN_TOO_DEEP,
                Stage::Resolve,
                Message::built(format!(
                    "The previous_response_id chain is longer than {} responses.",
                    settings.max_depth
                )),
            )
            .param("previous_response_id"));
        }
        next = r.previous.clone();
        chain.push(r);
    }
    let mut items: Vec<Value> = Vec::new();
    for r in chain.iter().rev() {
        items.extend(r.input.iter().cloned());
        items.extend(r.output.iter().cloned());
    }
    items.extend(turn.input.iter().cloned());
    if let Some(o) = body.as_object_mut() {
        o.shift_remove("previous_response_id");
        o.insert("input".into(), Value::Array(items));
    }
    let raw = Bytes::from(serde_json::to_vec(&body).unwrap_or_default());
    let skimmed = skim(&raw, &JsonLimits::default()).map_err(crate::parse::json_error)?;
    *parsed = Parsed::json(raw, skimmed, Some(body));
    turn.previous = Some(prev);
    Ok(turn)
}

/// A new gateway response id: `resp_` and 32 lowercase hex characters.
pub(crate) fn new_id() -> String {
    format!(
        "resp_{}",
        antirion_core::RequestId::generate_unguessable()
            .as_str()
            .replace('-', "")
    )
}

/// Remembers which deployment created a native response id.
pub(crate) async fn link(cx: &Ctx, turn: &Turn, id: &str, deployment: &str) {
    cx.rt
        .link_response(
            id,
            NativeLink {
                project: turn.project.clone(),
                deployment: deployment.to_owned(),
                created: cx.rt.now(),
            },
        )
        .await;
}

/// Stores a translated response; `false` when it is too large to store.
pub(crate) async fn keep(rt: &antirion_runtime::Runtime, turn: &Turn, id: &str, object: &Value) -> bool {
    if !turn.store || !turn.settings.gateway {
        return true;
    }
    let output: Vec<Value> = object
        .get("output")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let size = serde_json::to_vec(&turn.input).map_or(0, |b| b.len())
        + serde_json::to_vec(&output).map_or(0, |b| b.len());
    if u64::try_from(size).unwrap_or(u64::MAX) > turn.settings.max_bytes {
        return false;
    }
    rt.keep_response(
        id,
        StoredResponse {
            project: turn.project.clone(),
            previous: turn.previous.clone(),
            input: turn.input.clone(),
            output,
            object: object.clone(),
            created: rt.now(),
        },
    )
    .await;
    true
}

/// The 503 of a continuation whose deployment cannot serve it.
pub(crate) fn affinity_unavailable() -> GatewayError {
    let mut e = GatewayError::with_reason(
        reasons::AFFINITY_UNAVAILABLE,
        Stage::Route,
        Message::Fixed("The deployment that created the previous response is not available."),
    );
    e.retry_after = Some(Duration::from_secs(1));
    e
}

/// The 400 of a stateful field no eligible deployment serves.
pub(crate) fn stateful_unsupported(field: &str) -> GatewayError {
    GatewayError::with_reason(
        antirion_error::reason::hygiene::TRANSLATION_UNSUPPORTED,
        Stage::Resolve,
        Message::built(format!(
            "{field} needs a deployment that serves the Responses API natively."
        )),
    )
    .param(field.to_owned())
}

/// `GET` and `DELETE /v1/responses/{id}`: a gateway-stored response, or the native response proxied
/// to the deployment that created it.
pub(crate) async fn serve_stored(
    cx: &Ctx,
    parts: &http::request::Parts,
    endpoint: &antirion_dialects::Endpoint,
    key: &antirion_runtime::identity::KeyRt,
) -> http::Response<crate::ctx::Body> {
    let settings = Settings::of(&cx.snap.config.doc);
    let id = endpoint.path_param.clone().unwrap_or_default();
    let rt = &cx.rt;
    let now = cx.rt.now();
    let json = |status: http::StatusCode, v: &Value| {
        let mut resp = http::Response::new(crate::ctx::full(Bytes::from(
            serde_json::to_vec(v).unwrap_or_default(),
        )));
        *resp.status_mut() = status;
        resp.headers_mut().insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
        cx.finish(&mut resp, false);
        resp
    };
    match rt.native_link(&id, &key.project, now, settings.ttl).await {
        Ok(Some(link)) => {
            return crate::stored::proxy(cx, parts, endpoint, key, &link.deployment, &id).await;
        }
        Ok(None) => {}
        Err(_) => return cx.error(&store_unavailable()),
    }
    if parts.method == http::Method::DELETE {
        return match rt.delete_stored_response(&id, &key.project).await {
            Ok(true) => json(
                http::StatusCode::OK,
                &serde_json::json!({"id": id, "object": "response", "deleted": true}),
            ),
            Ok(false) => cx.error(&response_not_found()),
            Err(_) => cx.error(&store_unavailable()),
        };
    }
    match rt.stored_response(&id, &key.project, now, settings.ttl).await {
        Ok(Some(r)) => json(http::StatusCode::OK, &r.object),
        Ok(None) => cx.error(&response_not_found()),
        Err(_) => cx.error(&store_unavailable()),
    }
}
