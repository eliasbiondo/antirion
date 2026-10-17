//! Responses over a WebSocket: `GET /v1/responses` with an upgrade opens a socket whose
//! `response.create` events are each served as a `POST /v1/responses` stream through this node's
//! whole data plane, so every event is authenticated, admitted, limited, settled and logged as a
//! request of its own; the socket itself counts nothing.

use std::sync::Arc;
use std::time::Duration;

use antirion_core::{Peer, Stage};
use antirion_dialects::{Endpoint, Matched};
use antirion_error::{GatewayError, Message as ErrorMessage};
use antirion_runtime::Runtime;
use antirion_runtime::playground::{LocalPlane, PlaneBody, PlaneFuture};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http::header::{
    ACCEPT, AUTHORIZATION, CONNECTION, CONTENT_LENGTH, CONTENT_TYPE, HOST, SEC_WEBSOCKET_ACCEPT,
    SEC_WEBSOCKET_KEY, SEC_WEBSOCKET_PROTOCOL, SEC_WEBSOCKET_VERSION, UPGRADE,
};
use http::request::Parts;
use http::{HeaderMap, HeaderValue, Method, Response, StatusCode};
use http_body_util::BodyExt;
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::error::Error as WsError;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role, WebSocketConfig};
use tokio_tungstenite::tungstenite::{Message, Utf8Bytes};

use crate::ctx::{Body, Ctx, empty};
use crate::realtime::{accept_key, lists, offered};
use crate::reasons;

/// Whether a request asks to open a Responses socket.
pub(crate) fn is_upgrade(parts: &Parts) -> bool {
    parts.method == Method::GET
        && antirion_dialects::normalise(parts.uri.path()).as_deref() == Some("/v1/responses")
        && lists(&parts.headers, UPGRADE, "websocket")
}

/// The socket endpoint, unless `api.endpoints` turns it or the Responses endpoint off.
pub(crate) fn matched(api: Option<&Value>) -> Matched {
    let off = |id: &str| {
        api.and_then(|a| a.pointer(&format!("/endpoints/{id}/enabled"))) == Some(&Value::Bool(false))
    };
    if off("responses") || off("responses_websocket") {
        Matched::NotFound
    } else {
        Matched::Found(antirion_dialects::responses_socket())
    }
}

fn upgrade_required(message: &'static str) -> GatewayError {
    GatewayError::with_reason(
        reasons::UPGRADE_REQUIRED,
        Stage::Accept,
        ErrorMessage::Fixed(message),
    )
}

/// Headers of the socket's request that each event's request does not carry.
fn connection_specific(name: &http::HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "upgrade"
            | "host"
            | "content-length"
            | "transfer-encoding"
            | "te"
            | "keep-alive"
            | "sec-websocket-key"
            | "sec-websocket-version"
            | "sec-websocket-extensions"
            | "sec-websocket-protocol"
            | "x-request-id"
            | "accept"
            | "content-type"
            | "origin"
    )
}

/// What every event's request carries of the socket's: its headers, with a credential sent as a
/// subprotocol item moved to `Authorization`.
fn event_headers(parts: &Parts) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (name, value) in &parts.headers {
        if !connection_specific(name) {
            out.append(name.clone(), value.clone());
        }
    }
    if !out.contains_key(AUTHORIZATION)
        && let Some(key) = crate::auth::subprotocol_key(parts)
        && let Ok(v) = HeaderValue::from_str(&format!("Bearer {key}"))
    {
        out.insert(AUTHORIZATION, v);
    }
    out.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    out.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
    out
}

/// Opens a socket: the 101 that starts it, or the error that refuses it.
pub(crate) async fn serve(cx: &mut Ctx, parts: &Parts, endpoint: &Endpoint) -> Response<Body> {
    let started = std::time::Instant::now();
    if let Err(e) = crate::auth::run(cx, parts, endpoint).await {
        crate::reqlog::begin_unauthenticated(cx, parts, endpoint, started);
        return crate::reqlog::finish(cx, cx.error(&e));
    }
    match open(cx, parts) {
        Ok(resp) => resp,
        Err(e) => cx.error(&e),
    }
}

fn open(cx: &Ctx, parts: &Parts) -> Result<Response<Body>, GatewayError> {
    let h = &parts.headers;
    if !lists(h, CONNECTION, "upgrade")
        || h.get(SEC_WEBSOCKET_VERSION).and_then(|v| v.to_str().ok()) != Some("13")
    {
        return Err(upgrade_required(
            "A Responses socket is a WebSocket: send an HTTP/1.1 upgrade with version 13.",
        ));
    }
    let Some(key) = h.get(SEC_WEBSOCKET_KEY).and_then(|v| v.to_str().ok()) else {
        return Err(upgrade_required("The upgrade request has no Sec-WebSocket-Key."));
    };
    let Some(on) = parts.extensions.get::<hyper::upgrade::OnUpgrade>().cloned() else {
        return Err(upgrade_required(
            "A Responses socket is a WebSocket: send an HTTP/1.1 upgrade request.",
        ));
    };
    let Some(plane) = cx.rt.state.playground.plane() else {
        return Err(GatewayError::new(
            antirion_error::Code::Internal,
            Stage::Accept,
            ErrorMessage::Fixed("The data plane cannot be reached on this node."),
        ));
    };
    let peer = parts.extensions.get::<Peer>().cloned().unwrap_or(Peer {
        addr: std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 0)),
        tls: false,
        client_cert: None,
    });
    let session = Session {
        rt: Arc::clone(&cx.rt),
        plane,
        peer,
        headers: event_headers(parts),
        limit: usize::try_from(cx.snap.security.max_body).unwrap_or(usize::MAX),
        idle: cx.rt.statics.http.idle,
    };
    cx.rt.tasks.spawn(session.run(on));
    let mut resp = Response::new(empty());
    *resp.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    let out = resp.headers_mut();
    out.insert(UPGRADE, HeaderValue::from_static("websocket"));
    out.insert(CONNECTION, HeaderValue::from_static("Upgrade"));
    if let Ok(v) = HeaderValue::from_str(&accept_key(key.trim().as_bytes())) {
        out.insert(SEC_WEBSOCKET_ACCEPT, v);
    }
    if let Some(v) = offered(h).first().and_then(|p| HeaderValue::from_str(p).ok()) {
        out.insert(SEC_WEBSOCKET_PROTOCOL, v);
    }
    cx.finish(&mut resp, false);
    Ok(resp)
}

/// One open socket.
struct Session {
    rt: Arc<Runtime>,
    plane: Arc<dyn LocalPlane>,
    peer: Peer,
    headers: HeaderMap,
    /// The largest client message.
    limit: usize,
    /// How long the socket stays open without a message while no response is in progress.
    idle: Duration,
}

/// The response in progress.
enum Current {
    /// Waiting for the answer's head.
    Head(PlaneFuture),
    /// Reading the answer's body.
    Body {
        status: StatusCode,
        stream: bool,
        body: PlaneBody,
        pending: Vec<u8>,
    },
}

fn close(code: CloseCode, reason: &'static str) -> Message {
    Message::Close(Some(CloseFrame {
        code,
        reason: Utf8Bytes::from_static(reason),
    }))
}

fn error_event(status: u16, code: &str, message: &str) -> Message {
    let v = json!({
        "type": "error",
        "status": status,
        "error": {"type": "invalid_request_error", "code": code, "message": message, "param": null},
    });
    Message::Text(Utf8Bytes::from(v.to_string()))
}

/// The request body of a `response.create` event.
fn create_body(event: &Value) -> Option<Value> {
    let obj = event.as_object()?;
    if obj.get("type").and_then(Value::as_str) != Some("response.create") {
        return None;
    }
    let mut body = match obj.get("response") {
        Some(Value::Object(r)) => r.clone(),
        _ => {
            let mut b = obj.clone();
            b.remove("type");
            b
        }
    };
    body.insert("stream".into(), Value::Bool(true));
    Some(Value::Object(body))
}

/// The JSON of each complete SSE event in `pending`, which keeps what is incomplete.
fn complete_events(pending: &mut Vec<u8>) -> Vec<String> {
    let Some(end) = pending.windows(2).rposition(|w| w == b"\n\n") else {
        return Vec::new();
    };
    let done: Vec<u8> = pending.drain(..end + 2).collect();
    String::from_utf8_lossy(&done)
        .split("\n\n")
        .filter_map(|block| {
            let data: Vec<&str> = block
                .lines()
                .filter_map(|l| l.strip_prefix("data:").map(str::trim_start))
                .collect();
            let joined = data.join("\n");
            (!joined.is_empty() && joined != "[DONE]").then_some(joined)
        })
        .collect()
}

/// The message an answer that is not a stream becomes: an `error` event for an error status, the
/// body itself otherwise.
fn whole_answer(status: StatusCode, body: &[u8]) -> Message {
    let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    if status.is_success() {
        return Message::Text(Utf8Bytes::from(v.to_string()));
    }
    let error = v.get("error").cloned().unwrap_or(v);
    let out = json!({"type": "error", "status": status.as_u16(), "error": error});
    Message::Text(Utf8Bytes::from(out.to_string()))
}

impl Session {
    async fn run(self, on: hyper::upgrade::OnUpgrade) {
        let Ok(io) = on.await else {
            return;
        };
        let cfg = WebSocketConfig::default()
            .max_message_size(Some(self.limit))
            .max_frame_size(Some(self.limit));
        let mut ws = WebSocketStream::from_raw_socket(TokioIo::new(io), Role::Server, Some(cfg)).await;
        let mut current: Option<Current> = None;
        let cut = self.rt.drain.cut.clone();
        let shutdown = self.rt.shutdown.clone();
        let mut idle_at = tokio::time::Instant::now() + self.idle;
        loop {
            let busy = current.is_some();
            tokio::select! {
                () = cut.cancelled() => {
                    let _ = ws.send(close(CloseCode::Away, "the gateway is stopping")).await;
                    return;
                }
                () = shutdown.cancelled() => {
                    let _ = ws.send(close(CloseCode::Away, "the gateway is stopping")).await;
                    return;
                }
                () = tokio::time::sleep_until(idle_at), if !busy => {
                    let _ = ws.send(close(CloseCode::Away, "idle")).await;
                    return;
                }
                m = ws.next() => {
                    idle_at = tokio::time::Instant::now() + self.idle;
                    match m {
                        Some(Ok(Message::Text(t))) => {
                            if let Some(reply) = self.on_text(&t, &mut current)
                                && ws.send(reply).await.is_err()
                            {
                                return;
                            }
                        }
                        Some(Ok(Message::Binary(_))) => {
                            let _ = ws.send(close(CloseCode::Unsupported, "send JSON text messages")).await;
                            return;
                        }
                        Some(Ok(Message::Close(_))) => {
                            let _ = ws.send(close(CloseCode::Normal, "")).await;
                            let _ = ws.flush().await;
                            return;
                        }
                        Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {
                            if ws.flush().await.is_err() {
                                return;
                            }
                        }
                        Some(Err(WsError::Capacity(_))) => {
                            let _ = ws.send(close(CloseCode::Size, "message too big")).await;
                            return;
                        }
                        Some(Err(_)) | None => return,
                    }
                }
                step = advance(&mut current), if busy => {
                    let (messages, finished) = step;
                    for m in messages {
                        if ws.send(m).await.is_err() {
                            return;
                        }
                    }
                    if finished {
                        current = None;
                        idle_at = tokio::time::Instant::now() + self.idle;
                    }
                }
            }
        }
    }

    /// Starts the response a client message asks for, or the error message it gets.
    fn on_text(&self, t: &Utf8Bytes, current: &mut Option<Current>) -> Option<Message> {
        let event: Value = match serde_json::from_str(t.as_str()) {
            Ok(v @ Value::Object(_)) => v,
            _ => {
                return Some(error_event(
                    400,
                    "invalid_event",
                    "Each message must be a JSON object.",
                ));
            }
        };
        let Some(body) = create_body(&event) else {
            return Some(error_event(
                400,
                "invalid_event",
                "Send events of type response.create.",
            ));
        };
        if current.is_some() {
            return Some(error_event(
                409,
                "response_in_progress",
                "A response is in progress on this socket; wait for it to finish.",
            ));
        }
        let mut req = http::Request::new(Bytes::from(body.to_string()));
        *req.method_mut() = Method::POST;
        *req.uri_mut() = http::Uri::from_static("/v1/responses");
        *req.headers_mut() = self.headers.clone();
        req.headers_mut().remove(CONTENT_LENGTH);
        req.headers_mut()
            .insert(HOST, HeaderValue::from_static("localhost"));
        *current = Some(Current::Head(self.plane.send_from(req, self.peer.clone())));
        None
    }
}

/// Moves the response in progress one step: the messages it produced and whether it is over.
async fn advance(current: &mut Option<Current>) -> (Vec<Message>, bool) {
    match current {
        None => std::future::pending().await,
        Some(Current::Head(f)) => match f.await {
            Ok(resp) => {
                let status = resp.status();
                let stream = resp
                    .headers()
                    .get(CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|ct| ct.starts_with("text/event-stream"));
                *current = Some(Current::Body {
                    status,
                    stream,
                    body: resp.into_body(),
                    pending: Vec::new(),
                });
                (Vec::new(), false)
            }
            Err(_) => (
                vec![error_event(
                    503,
                    "unavailable",
                    "The gateway could not serve the event; send it again.",
                )],
                true,
            ),
        },
        Some(Current::Body {
            status,
            stream,
            body,
            pending,
        }) => match body.frame().await {
            Some(Ok(frame)) => {
                let Ok(data) = frame.into_data() else {
                    return (Vec::new(), false);
                };
                pending.extend_from_slice(&data);
                if !*stream {
                    return (Vec::new(), false);
                }
                let messages = complete_events(pending)
                    .into_iter()
                    .map(|e| Message::Text(Utf8Bytes::from(e)))
                    .collect();
                (messages, false)
            }
            Some(Err(_)) => (Vec::new(), true),
            None => {
                let messages = if *stream {
                    pending.extend_from_slice(b"\n\n");
                    complete_events(pending)
                        .into_iter()
                        .map(|e| Message::Text(Utf8Bytes::from(e)))
                        .collect()
                } else {
                    vec![whole_answer(*status, pending)]
                };
                (messages, true)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_become_stream_bodies() {
        let b =
            create_body(&json!({"type": "response.create", "model": "m", "input": "hi", "stream": false}))
                .unwrap();
        assert_eq!(b, json!({"model": "m", "input": "hi", "stream": true}));
        let nested = create_body(&json!({"type": "response.create", "response": {"model": "m"}})).unwrap();
        assert_eq!(nested, json!({"model": "m", "stream": true}));
        assert!(create_body(&json!({"type": "response.cancel"})).is_none());
    }

    #[test]
    fn complete_sse_events_only() {
        let mut p = b"event: response.created\ndata: {\"a\":1}\n\ndata: [DONE]\n\ndata: {\"b\"".to_vec();
        assert_eq!(complete_events(&mut p), ["{\"a\":1}"]);
        assert_eq!(p, b"data: {\"b\"");
        let m = whole_answer(
            StatusCode::TOO_MANY_REQUESTS,
            br#"{"error": {"message": "slow down"}}"#,
        );
        let Message::Text(t) = m else {
            panic!("a text message")
        };
        let v: Value = serde_json::from_str(t.as_str()).unwrap();
        assert_eq!(v["status"], 429);
        assert_eq!(v["error"]["message"], "slow down");
    }

    #[test]
    fn the_socket_request_keeps_controls_but_not_connection_headers() {
        let req = http::Request::builder()
            .method(Method::GET)
            .uri("/v1/responses")
            .header("upgrade", "websocket")
            .header("sec-websocket-protocol", "openai-insecure-api-key.sk-x, realtime")
            .header("x-antirion-metadata", "a=1")
            .header("x-request-id", "r1")
            .body(())
            .unwrap();
        let (parts, ()) = req.into_parts();
        let h = event_headers(&parts);
        assert_eq!(h.get(AUTHORIZATION).unwrap(), "Bearer sk-x");
        assert_eq!(h.get("x-antirion-metadata").unwrap(), "a=1");
        assert!(h.get("upgrade").is_none() && h.get("x-request-id").is_none());
        assert!(is_upgrade(&parts));
    }
}
