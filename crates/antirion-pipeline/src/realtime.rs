//! The Realtime endpoint: a WebSocket upgrade (credential in a header or the
//! `openai-insecure-api-key.<key>` subprotocol), routed on the `model` query parameter and bridged
//! to the deployment's Realtime endpoint with the provider's credential. Messages are relayed both
//! ways as they are, except the model of a client's `session.update`; each `response.done` the
//! upstream sends is settled as usage. The session holds its admission (requests and concurrency)
//! until it closes; a client message larger than the body limit closes it with 1009.

use std::sync::Arc;
use std::time::Duration;

use antirion_core::Stage;
use antirion_dialects::Endpoint;
use antirion_egress::{Payload, UpgradeAnswer};
use antirion_error::{GatewayError, Message as ErrorMessage};
use antirion_providers::{DeploymentRt, UpstreamCall, UpstreamOp};
use antirion_resilience::Class;
use antirion_runtime::Runtime;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http::header::{
    CONNECTION, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_KEY, SEC_WEBSOCKET_PROTOCOL, SEC_WEBSOCKET_VERSION,
    UPGRADE,
};
use http::request::Parts;
use http::{HeaderMap, HeaderValue, Method, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::error::Error as WsError;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role, WebSocketConfig};
use tokio_tungstenite::tungstenite::{Message, Utf8Bytes};

use crate::account::{self, Admission, Deferred, DeferredCall, End, StreamEnd};
use crate::auth::{Authenticated, SUBPROTOCOL_KEY};
use crate::ctx::{Body, Ctx, empty};
use crate::reasons;

const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// The ledger line of a session's first response; the session's own line comes first.
const FIRST_RESPONSE_LINE: u32 = 2;

/// `Sec-WebSocket-Accept` for a key.
pub(crate) fn accept_key(key: &[u8]) -> String {
    use sha1::Digest;
    let mut h = sha1::Sha1::new();
    h.update(key);
    h.update(WS_GUID.as_bytes());
    STANDARD.encode(h.finalize())
}

fn upgrade_required(message: &'static str) -> GatewayError {
    GatewayError::with_reason(
        reasons::UPGRADE_REQUIRED,
        Stage::Accept,
        ErrorMessage::Fixed(message),
    )
}

/// Whether a header lists `token` (case-insensitive, comma-separated).
pub(crate) fn lists(headers: &HeaderMap, name: http::header::HeaderName, token: &str) -> bool {
    headers
        .get_all(name)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|t| t.trim().eq_ignore_ascii_case(token))
}

/// The subprotocols the client offered, without the credential item.
pub(crate) fn offered(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|t| !t.is_empty() && !t.starts_with(SUBPROTOCOL_KEY))
        .map(str::to_owned)
        .collect()
}

/// A query parameter, percent-decoded.
fn query_param(parts: &Parts, name: &str) -> Option<String> {
    parts.uri.query()?.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        (k == name).then(|| percent_decode(v))
    })
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%'
            && let Some(hex) = bytes.get(i + 1..i + 3)
            && let Ok(hex) = std::str::from_utf8(hex)
            && let Ok(v) = u8::from_str_radix(hex, 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(if b == b'+' { b' ' } else { b });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The checks of the upgrade request itself.
pub(crate) fn check_upgrade(parts: &Parts) -> Result<(hyper::upgrade::OnUpgrade, String), GatewayError> {
    let h = &parts.headers;
    if parts.method != Method::GET
        || !lists(h, UPGRADE, "websocket")
        || !lists(h, CONNECTION, "upgrade")
        || h.get(SEC_WEBSOCKET_VERSION).and_then(|v| v.to_str().ok()) != Some("13")
    {
        return Err(upgrade_required(
            "The Realtime endpoint is a WebSocket: send an HTTP/1.1 upgrade request.",
        ));
    }
    let Some(key) = h.get(SEC_WEBSOCKET_KEY).and_then(|v| v.to_str().ok()) else {
        return Err(upgrade_required("The upgrade request has no Sec-WebSocket-Key."));
    };
    let Some(on) = parts.extensions.get::<hyper::upgrade::OnUpgrade>().cloned() else {
        return Err(upgrade_required(
            "The Realtime endpoint is a WebSocket: send an HTTP/1.1 upgrade request.",
        ));
    };
    Ok((on, accept_key(key.trim().as_bytes())))
}

/// Serves a Realtime upgrade: the 101 that starts the session, or the error.
pub(crate) async fn serve(
    cx: &mut Ctx,
    parts: &Parts,
    endpoint: &Endpoint,
    auth: &Authenticated,
) -> Response<Body> {
    match open(cx, parts, endpoint, auth).await {
        Ok(r) => r,
        Err(e) => {
            let slot = cx.account.clone();
            account::settle(&slot, &cx.log, End::Failed).await;
            cx.error(&e)
        }
    }
}

async fn open(
    cx: &mut Ctx,
    parts: &Parts,
    endpoint: &Endpoint,
    auth: &Authenticated,
) -> Result<Response<Body>, GatewayError> {
    let (client_upgrade, accept) = check_upgrade(parts)?;
    let key = Arc::clone(&auth.key);
    cx.log.stage(Stage::Resolve);
    let requested = match query_param(parts, "model").filter(|m| !m.trim().is_empty()) {
        Some(m) => m,
        None => cx
            .snap
            .config
            .doc
            .pointer("/routing/default_models/realtime")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                GatewayError::with_reason(
                    antirion_error::reason::hygiene::MISSING_MODEL,
                    Stage::Resolve,
                    ErrorMessage::Fixed("A model is required: name it in the model query parameter."),
                )
                .param("model")
            })?,
    };
    cx.log
        .update(|r| r.requested_model = Some(requested.chars().take(256).collect()));
    let snap = Arc::clone(&cx.snap);
    let target = crate::authorize::resolve(&snap, &key, &requested)?;
    crate::authorize::lifecycle(&target, &key, &requested, cx.rt.now())?;
    let resolved = crate::infer::model::resolution(&snap, &requested, &target)?;
    let levels = snap.identity.access_levels(&key);
    let candidates: Vec<Arc<DeploymentRt>> = resolved
        .known
        .iter()
        .filter(|d| {
            d.usable()
                && d.provider.kind.openai_family()
                && d.capabilities.operation("realtime")
                && crate::authorize::deployment_allowed(&levels, d)
        })
        .cloned()
        .collect();
    if candidates.is_empty() {
        return Err(GatewayError::with_reason(
            reasons::CAP_DIALECTS,
            Stage::Resolve,
            ErrorMessage::built(format!(
                "No deployment of model `{requested}` serves the Realtime API."
            )),
        )
        .param("model"));
    }
    let model_name = resolved.model_name.clone();
    cx.log.update(|r| {
        r.model = Some(model_name.clone());
        r.route = Some(resolved.route.tree.name.clone());
    });
    let end_user = auth.end_user.clone();
    crate::access::require_end_user(auth, end_user.as_deref())?;
    let tags = crate::files::tags(cx, auth);
    crate::access::check(
        cx,
        &crate::access::Subject {
            parts,
            auth,
            end_user: end_user.as_deref(),
            requested: &requested,
            model: &model_name,
            tags: &tags,
        },
    )
    .await?;
    crate::files::uncacheable(cx, &key);
    cx.log.stage(Stage::Admit);
    let raw = Bytes::from_static(b"{}");
    let skim =
        antirion_json::skim(&raw, &antirion_json::JsonLimits::default()).map_err(crate::parse::json_error)?;
    let parsed = crate::parse::Parsed::json(raw, skim, Some(Value::Object(serde_json::Map::new())));
    let refs: Vec<&Arc<DeploymentRt>> = candidates.iter().collect();
    account::admit(
        cx,
        &cx.account,
        &Admission {
            endpoint,
            parsed: &parsed,
            key: &key,
            end_user: end_user.as_deref(),
            model: &model_name,
            route: resolved.policy_route.as_deref(),
            tags: &tags,
            candidates: &refs,
            stream: true,
            hedge: 1,
        },
    )
    .await?;
    cx.log.stage(Stage::Route);
    cx.log.update(|r| r.routed = true);
    let protocols = offered(&parts.headers);
    let (deployment, headers, upstream) = connect(cx, parts, &candidates, &protocols).await?;
    let chosen = headers
        .get(SEC_WEBSOCKET_PROTOCOL)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|p| protocols.iter().any(|o| o == p))
        .map(str::to_owned)
        .or_else(|| protocols.first().cloned());
    let session = Session {
        rt: Arc::clone(&cx.rt),
        snap: Arc::clone(&cx.snap),
        deployment,
        limit: usize::try_from(cx.snap.security.max_body).unwrap_or(usize::MAX),
        slot: cx.account.clone(),
        log: cx.log.hand_off(),
        deferred: cx.account.deferred(),
    };
    cx.rt.tasks.spawn(session.run(client_upgrade, upstream));
    let mut resp = Response::new(empty());
    *resp.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    let h = resp.headers_mut();
    h.insert(UPGRADE, HeaderValue::from_static("websocket"));
    h.insert(CONNECTION, HeaderValue::from_static("Upgrade"));
    if let Ok(v) = HeaderValue::from_str(&accept) {
        h.insert(SEC_WEBSOCKET_ACCEPT, v);
    }
    if let Some(v) = chosen.and_then(|p| HeaderValue::from_str(&p).ok()) {
        h.insert(SEC_WEBSOCKET_PROTOCOL, v);
    }
    cx.finish(&mut resp, false);
    Ok(resp)
}

/// Opens the upstream WebSocket on the first candidate that answers; a candidate that cannot be
/// reached is skipped for the next, an upstream refusal ends the request.
async fn connect(
    cx: &Ctx,
    parts: &Parts,
    candidates: &[Arc<DeploymentRt>],
    protocols: &[String],
) -> Result<(Arc<DeploymentRt>, HeaderMap, hyper::upgrade::Upgraded), GatewayError> {
    let mut last: Option<GatewayError> = None;
    for (i, d) in candidates.iter().enumerate() {
        let attempts = u16::try_from(i + 1).unwrap_or(u16::MAX);
        let started = std::time::Instant::now();
        let started_at = cx.rt.now();
        let prepared = d.build_request(UpstreamCall {
            op: UpstreamOp::Realtime,
            stream: false,
            body: Payload::default(),
            content_type: None,
            request_id: &cx.request_id,
            response_timeout: Duration::from_secs(30),
            connect_timeout: None,
            anthropic: None,
            key: None,
            byok: None,
        });
        let Ok(prepared) = prepared else {
            continue;
        };
        let mut request = prepared.request;
        request.method = Method::GET;
        request.resend_when_closed = false;
        let h = &mut request.headers;
        h.remove(http::header::CONTENT_TYPE);
        h.insert(UPGRADE, HeaderValue::from_static("websocket"));
        h.insert(CONNECTION, HeaderValue::from_static("Upgrade"));
        h.insert(SEC_WEBSOCKET_VERSION, HeaderValue::from_static("13"));
        let nonce = STANDARD.encode(antirion_core::ResourceUid::random().0.to_le_bytes());
        if let Ok(v) = HeaderValue::from_str(&nonce) {
            h.insert(SEC_WEBSOCKET_KEY, v);
        }
        if !protocols.is_empty()
            && let Ok(v) = HeaderValue::from_str(&protocols.join(", "))
        {
            h.insert(SEC_WEBSOCKET_PROTOCOL, v);
        }
        if let Some(beta) = parts.headers.get("openai-beta") {
            h.insert(http::header::HeaderName::from_static("openai-beta"), beta.clone());
        }
        let tls = cx
            .snap
            .security
            .egress_tls
            .clone()
            .unwrap_or_else(|| d.provider.tls.clone());
        if d.authorize(&mut request, &cx.rt.creds, &tls).await.is_err() {
            last = Some(
                crate::failure::fail(
                    d,
                    attempts,
                    Class::CredentialRejected,
                    antirion_error::Code::UpstreamAuthFailed,
                    Some(reasons::CREDENTIAL_ACQUISITION_FAILED),
                    None,
                    ErrorMessage::built(format!(
                        "The gateway could not obtain a credential for provider `{}`.",
                        d.provider.name
                    )),
                )
                .error
                .as_ref()
                .clone(),
            );
            continue;
        }
        let answer = tokio::select! {
            () = cx.gone.cancelled() => return Err(cancelled()),
            r = cx.rt.egress.upgrade(request, 64 << 10) => r,
        };
        let status = match &answer {
            Ok(UpgradeAnswer::Switched { .. }) => Some(101),
            Ok(UpgradeAnswer::Refused { status, .. }) => Some(status.as_u16()),
            Err(_) => None,
        };
        let class = match &answer {
            Ok(UpgradeAnswer::Switched { .. }) => Class::Success,
            Ok(UpgradeAnswer::Refused { status, .. }) => {
                antirion_resilience::classify::status(status.as_u16(), false)
            }
            Err(t) => antirion_resilience::classify::transport(t),
        };
        record_attempt(cx, d, attempts, started, started_at, status, class);
        match answer {
            Ok(UpgradeAnswer::Switched { headers, io }) => {
                let expected = accept_key(nonce.as_bytes());
                let matches = headers
                    .get(SEC_WEBSOCKET_ACCEPT)
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|v| v.trim() == expected);
                if !matches {
                    return Err(GatewayError::with_reason(
                        antirion_error::reason::hygiene::INVALID_BODY,
                        Stage::Attempt,
                        ErrorMessage::Fixed(
                            "The upstream answered the upgrade without a valid Sec-WebSocket-Accept.",
                        ),
                    ));
                }
                return Ok((Arc::clone(d), headers, io));
            }
            Ok(UpgradeAnswer::Refused { status, body, .. }) => {
                let message = serde_json::from_slice::<Value>(&body)
                    .ok()
                    .and_then(|v| {
                        v.pointer("/error/message")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .unwrap_or_else(|| format!("The upstream refused the upgrade with status {status}."));
                let f = crate::failure::of_class(
                    d,
                    attempts,
                    class,
                    Some(status.as_u16()),
                    ErrorMessage::built(message.chars().take(512).collect::<String>()),
                );
                return Err(*f.error);
            }
            Err(_) => {
                last = Some(
                    *crate::failure::of_class(
                        d,
                        attempts,
                        class,
                        None,
                        ErrorMessage::Fixed("The upstream Realtime endpoint could not be reached."),
                    )
                    .error,
                );
            }
        }
    }
    Err(last.unwrap_or_else(crate::respstate::affinity_unavailable))
}

fn cancelled() -> GatewayError {
    GatewayError::new(
        antirion_error::Code::Internal,
        Stage::Attempt,
        ErrorMessage::Fixed("The client went away."),
    )
}

/// Adds one connection attempt to the request log.
fn record_attempt(
    cx: &Ctx,
    d: &Arc<DeploymentRt>,
    attempts: u16,
    started: std::time::Instant,
    started_at: antirion_core::UtcMicros,
    status: Option<u16>,
    class: Class,
) {
    cx.log.update(|r| {
        r.attempts.push(crate::reqlog::AttemptFact {
            reason: if attempts == 1 { "primary" } else { "fallback" },
            provider: d.provider.name.clone(),
            provider_type: d.provider.kind.as_str().to_owned(),
            deployment: d.name.clone(),
            upstream_model: d.model.clone(),
            credential: None,
            key: None,
            started_at,
            start: started,
            end: std::time::Instant::now(),
            ttft: None,
            status,
            class: class.as_str(),
            retry_after_ms: None,
            upstream_request_id: None,
            error_message: None,
            error_body: None,
            usage: None,
            usage_raw: None,
            span: antirion_obs::trace::SpanId::random(),
            server: None,
            wait_ms: 0,
            via: None,
            timeout: None,
            tail: false,
        });
    });
}

/// One bridged session.
struct Session {
    rt: Arc<Runtime>,
    snap: Arc<antirion_runtime::Snapshot>,
    deployment: Arc<DeploymentRt>,
    /// The largest client message relayed.
    limit: usize,
    slot: account::Slot,
    log: crate::reqlog::Draft,
    deferred: Option<Deferred>,
}

/// How a session ended.
enum Ended {
    Client,
    Upstream,
    TooLarge,
    Stopped,
}

fn close(code: CloseCode, reason: &'static str) -> Message {
    Message::Close(Some(CloseFrame {
        code,
        reason: Utf8Bytes::from_static(reason),
    }))
}

impl Session {
    async fn run(self, client: hyper::upgrade::OnUpgrade, upstream: hyper::upgrade::Upgraded) {
        let ended = match client.await {
            Ok(io) => self.relay(io, upstream).await,
            Err(_) => Ended::Client,
        };
        let outcome = match ended {
            Ended::Upstream => StreamEnd::UpstreamFailed,
            Ended::Client | Ended::TooLarge | Ended::Stopped => StreamEnd::Completed,
        };
        let usage = crate::reqlog::Usage {
            input: Some(0),
            output: Some(0),
            ..crate::reqlog::Usage::default()
        };
        account::settle(
            &self.slot,
            &self.log,
            End::Stream {
                usage,
                raw: None,
                outcome,
            },
        )
        .await;
        crate::reqlog::settle_stream(&self.rt, &self.log);
    }

    async fn relay(&self, client: hyper::upgrade::Upgraded, upstream: hyper::upgrade::Upgraded) -> Ended {
        let client_cfg = WebSocketConfig::default()
            .max_message_size(Some(self.limit))
            .max_frame_size(Some(self.limit));
        let upstream_cfg = WebSocketConfig::default()
            .max_message_size(None)
            .max_frame_size(None);
        let mut client =
            WebSocketStream::from_raw_socket(TokioIo::new(client), Role::Server, Some(client_cfg)).await;
        let mut upstream =
            WebSocketStream::from_raw_socket(TokioIo::new(upstream), Role::Client, Some(upstream_cfg)).await;
        let cut = self.rt.drain.cut.clone();
        let shutdown = self.rt.shutdown.clone();
        let mut line = FIRST_RESPONSE_LINE;
        loop {
            tokio::select! {
                () = cut.cancelled() => {
                    let _ = client.send(close(CloseCode::Away, "the gateway is stopping")).await;
                    let _ = upstream.close(None).await;
                    return Ended::Stopped;
                }
                () = shutdown.cancelled() => {
                    let _ = client.send(close(CloseCode::Away, "the gateway is stopping")).await;
                    let _ = upstream.close(None).await;
                    return Ended::Stopped;
                }
                m = client.next() => match m {
                    Some(Ok(Message::Text(t))) => {
                        let t = self.rewrite_model(t);
                        if upstream.send(Message::Text(t)).await.is_err() {
                            let _ = client.send(close(CloseCode::Error, "the upstream connection failed")).await;
                            return Ended::Upstream;
                        }
                    }
                    Some(Ok(Message::Binary(b))) => {
                        if upstream.send(Message::Binary(b)).await.is_err() {
                            let _ = client.send(close(CloseCode::Error, "the upstream connection failed")).await;
                            return Ended::Upstream;
                        }
                    }
                    Some(Ok(Message::Close(frame))) => {
                        let _ = upstream.send(Message::Close(frame)).await;
                        let _ = client.flush().await;
                        return Ended::Client;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(WsError::Capacity(_))) => {
                        let _ = client.send(close(CloseCode::Size, "message too big")).await;
                        let _ = upstream.close(None).await;
                        return Ended::TooLarge;
                    }
                    Some(Err(_)) | None => {
                        let _ = upstream.close(None).await;
                        return Ended::Client;
                    }
                },
                m = upstream.next() => match m {
                    Some(Ok(Message::Text(t))) => {
                        if self.settle_response(&t, line).await {
                            line = line.saturating_add(1);
                        }
                        if client.send(Message::Text(t)).await.is_err() {
                            let _ = upstream.close(None).await;
                            return Ended::Client;
                        }
                    }
                    Some(Ok(Message::Binary(b))) => {
                        if client.send(Message::Binary(b)).await.is_err() {
                            let _ = upstream.close(None).await;
                            return Ended::Client;
                        }
                    }
                    Some(Ok(Message::Close(frame))) => {
                        let _ = client.send(Message::Close(frame)).await;
                        return Ended::Upstream;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => {
                        let _ = client.send(close(CloseCode::Error, "the upstream connection failed")).await;
                        return Ended::Upstream;
                    }
                },
            }
        }
    }

    /// A client message with `session.update`'s model set to the deployment's upstream id.
    fn rewrite_model(&self, t: Utf8Bytes) -> Utf8Bytes {
        if !t.as_str().contains("session.update") {
            return t;
        }
        let Ok(mut v) = serde_json::from_str::<Value>(t.as_str()) else {
            return t;
        };
        if v.get("type").and_then(Value::as_str) != Some("session.update") {
            return t;
        }
        let Some(m) = v.pointer_mut("/session/model") else {
            return t;
        };
        *m = Value::from(self.deployment.model.as_str());
        Utf8Bytes::from(serde_json::to_string(&v).unwrap_or_default())
    }

    /// Settles a `response.done` event's usage as one ledger line; whether it was one.
    async fn settle_response(&self, t: &Utf8Bytes, line: u32) -> bool {
        if !t.as_str().contains("response.done") {
            return false;
        }
        let Ok(v) = serde_json::from_str::<Value>(t.as_str()) else {
            return false;
        };
        if v.get("type").and_then(Value::as_str) != Some("response.done") {
            return false;
        }
        let Some(u) = v.pointer("/response/usage").filter(|u| u.is_object()) else {
            return true;
        };
        let n = |p: &str| u.pointer(p).and_then(Value::as_u64).unwrap_or(0);
        let usage = antirion_limits::usage::Usage {
            input: n("/input_tokens"),
            input_cache_read: n("/input_token_details/cached_tokens"),
            input_audio: n("/input_token_details/audio_tokens"),
            output: n("/output_tokens"),
            output_audio: n("/output_token_details/audio_tokens"),
            ..antirion_limits::usage::Usage::default()
        };
        if let Some(d) = &self.deferred {
            let _ = account::settle_deferred(
                &self.rt,
                &self.snap,
                d,
                DeferredCall {
                    deployment: &self.deployment,
                    line,
                    usage,
                    raw: Some(u.clone()),
                    batch: false,
                    purpose: antirion_limits::ledger::Purpose::Primary,
                    status: antirion_limits::ledger::LineStatus::Success,
                },
            )
            .await;
        }
        self.log.update(|r| {
            let total = r.usage.input.unwrap_or(0) + usage.input;
            let out = r.usage.output.unwrap_or(0) + usage.output;
            r.usage.input = Some(total);
            r.usage.output = Some(out);
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_key_of_the_rfc_example() {
        assert_eq!(
            accept_key(b"dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn credential_items_are_never_offered_upstream() {
        let mut h = HeaderMap::new();
        h.append(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("realtime, openai-insecure-api-key.ak-x"),
        );
        h.append(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("openai-beta.realtime-v1"),
        );
        assert_eq!(offered(&h), ["realtime", "openai-beta.realtime-v1"]);
        assert_eq!(percent_decode("gpt%2Drt+x"), "gpt-rt x");
    }
}
