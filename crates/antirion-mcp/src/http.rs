//! The HTTP side of MCP and A2A endpoints: the checks made before authentication, body decoding,
//! JSON-RPC parsing, era detection, and dispatch to the legacy and modern handlers.

use std::sync::Arc;

use antirion_core::{RequestId, Stage, UtcMicros};
use antirion_error::reason::hygiene;
use antirion_error::{Code, GatewayError, Message};
use bytes::Bytes;
use http::header::{ACCEPT, ALLOW, CONTENT_LENGTH, CONTENT_TYPE, HOST, ORIGIN};
use http::{HeaderMap, HeaderValue, Method, Response};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::caller::Caller;
use crate::compile::{GatewayRt, McpSnapshot};
use crate::egress::Egress;
use crate::hooks::Hooks;
use crate::log::CallLog;
use crate::rpc::{Fail, Msg, classify};
use crate::state::McpState;
use crate::token::Keyring;
use crate::upstream::{self, MODERN};
use crate::{Body, a2a, legacy, modern, reasons, respond};

/// What a request needs from the running node.
#[derive(Clone, Debug)]
pub struct Env {
    pub snap: Arc<McpSnapshot>,
    pub state: Arc<McpState>,
    pub egress: Egress,
    pub now: UtcMicros,
    pub request_id: RequestId,
    pub tasks: TaskTracker,
    pub shutdown: CancellationToken,
    /// `scheme://host[:port][/prefix]` of the gateway as clients reach it.
    pub public_base: String,
    pub hooks: Hooks,
    /// The facts the request's log record carries.
    pub log: Arc<CallLog>,
}

/// Which MCP or A2A endpoint a request addressed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpPath {
    /// `/mcp` (gateway `default`) or `/mcp/<name>`.
    Gateway(String),
    /// `POST /a2a/<agent>`.
    Agent(String),
    /// `GET /a2a/<agent>/.well-known/agent-card.json` (or `agent.json`).
    AgentCard(String),
    /// `GET /mcp/<name>/sse`: the client-facing HTTP+SSE stream.
    SseStream(String),
    /// `POST /mcp/<name>/message?sessionId=<handle>`.
    SseMessage(String, Option<String>),
}

impl McpPath {
    /// The MCP gateway the path addresses, on any of its transports.
    #[must_use]
    pub fn gateway(&self) -> Option<&str> {
        match self {
            McpPath::Gateway(g) | McpPath::SseStream(g) | McpPath::SseMessage(g, _) => Some(g),
            McpPath::Agent(_) | McpPath::AgentCard(_) => None,
        }
    }
}

/// One request to a gateway, after authentication.
#[derive(Clone, Debug)]
pub(crate) struct Req {
    pub(crate) env: Env,
    pub(crate) gw: Arc<GatewayRt>,
    pub(crate) caller: Caller,
    pub(crate) headers: HeaderMap,
    pub(crate) keys: Keyring,
}

impl Req {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    pub(crate) fn error(&self, fail: &Fail) -> Response<Body> {
        respond::error(fail, &self.env)
    }
}

fn header(headers: &HeaderMap, name: impl http::header::AsHeaderName) -> Option<&str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// Whether an `Accept` header admits `wanted` (`type/subtype`), counting `*/*` and `type/*`.
fn accepts(accept: Option<&str>, wanted: &str) -> bool {
    let Some(accept) = accept else {
        return false;
    };
    let main = wanted.split('/').next().unwrap_or_default();
    accept.split(',').any(|part| {
        let media = part
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        media == wanted || media == "*/*" || media == format!("{main}/*")
    })
}

fn gateway_error(env: &Env, fail: &Fail) -> Response<Body> {
    respond::error(fail, env)
}

/// The checks made from the request head before authentication: the gateway's `Origin` allow-list and
/// its body limit against a declared `Content-Length`. `None` lets the request continue.
#[must_use]
pub fn precheck(env: &Env, path: &McpPath, headers: &HeaderMap) -> Option<Response<Body>> {
    let name = path.gateway()?;
    let gw = env.snap.gateway(name)?;
    if let Some(origin) = header(headers, ORIGIN)
        && !gw.allowed_origins.iter().any(|o| o == origin)
    {
        let fail = Fail::gateway(
            reasons::ORIGIN_NOT_ALLOWED,
            Stage::Accept,
            Message::Fixed("The request origin is not allowed for this MCP gateway."),
        );
        return Some(gateway_error(env, &fail));
    }
    let declared = header(headers, CONTENT_LENGTH).and_then(|v| v.trim().parse::<u64>().ok());
    if declared.is_some_and(|n| n > gw.max_body) {
        return Some(gateway_error(env, &too_large(Stage::Accept)));
    }
    None
}

fn too_large(stage: Stage) -> Fail {
    Fail::from(
        GatewayError::with_reason(
            hygiene::BODY_TOO_LARGE,
            stage,
            Message::Fixed("The request body is too large."),
        )
        .closing(),
    )
}

fn first_item(v: &str) -> &str {
    v.split(',').next().unwrap_or_default().trim()
}

/// The gateway's public base URL: `server.public_url` when set, else built from the request (`https`
/// when it arrived over TLS), where the forwarding headers count only when the TCP peer is a trusted
/// proxy.
#[must_use]
pub fn public_base(
    public_url: Option<&str>,
    headers: &HeaderMap,
    secure: bool,
    trusted_peer: bool,
) -> String {
    if let Some(u) = public_url.filter(|u| !u.is_empty()) {
        return u.trim_end_matches('/').to_owned();
    }
    let mut scheme = if secure { "https" } else { "http" }.to_owned();
    let mut host = header(headers, HOST).unwrap_or("localhost").to_owned();
    let mut prefix = String::new();
    if trusted_peer {
        if let Some(fwd) = header(headers, "forwarded") {
            for pair in first_item(fwd).split(';') {
                let Some((k, v)) = pair.split_once('=') else {
                    continue;
                };
                let v = v.trim().trim_matches('"');
                match k.trim().to_ascii_lowercase().as_str() {
                    "proto" => scheme = v.to_ascii_lowercase(),
                    "host" => host = v.to_owned(),
                    _ => {}
                }
            }
        }
        if let Some(p) = header(headers, "x-forwarded-proto") {
            scheme = first_item(p).to_ascii_lowercase();
        }
        if let Some(h) = header(headers, "x-forwarded-host") {
            host = first_item(h).to_owned();
        }
        if let Some(p) = header(headers, "x-forwarded-prefix") {
            prefix = first_item(p).trim_end_matches('/').to_owned();
        }
    }
    format!("{scheme}://{host}{prefix}")
}

/// Whether the agent card at `path` needs a credential (`public_card` lifts it for cards only).
#[must_use]
pub fn a2a_requires_credential(snap: &McpSnapshot, path: &McpPath) -> bool {
    match path {
        McpPath::AgentCard(name) => !snap.agents.get(name).is_some_and(|a| a.enabled && a.public_card),
        _ => true,
    }
}

/// The largest decoded body the endpoint accepts: the gateway's limit within the global one.
#[must_use]
pub fn body_limit(snap: &McpSnapshot, path: &McpPath, global: u64) -> u64 {
    match path {
        McpPath::Gateway(name) | McpPath::SseMessage(name, _) => {
            snap.gateway(name).map_or(global, |g| g.max_body.min(global))
        }
        _ => global,
    }
}

/// The checks made before the body is read: content type and accepted media types. `None` lets the
/// request continue (and its body be read).
#[must_use]
pub fn before_body(
    env: &Env,
    path: &McpPath,
    method: &Method,
    headers: &HeaderMap,
) -> Option<Response<Body>> {
    let accept = header(headers, ACCEPT);
    match path {
        McpPath::Gateway(_) => {}
        McpPath::SseStream(_) => {
            return (!accepts(accept, "text/event-stream")).then(|| gateway_error(env, &not_acceptable()));
        }
        McpPath::SseMessage(..) => {
            let json = header(headers, CONTENT_TYPE)
                .and_then(|c| c.split(';').next())
                .is_some_and(|c| c.trim().eq_ignore_ascii_case("application/json"));
            return (!json).then(|| {
                gateway_error(
                    env,
                    &Fail::from(GatewayError::new(
                        Code::UnsupportedMediaType,
                        Stage::Parse,
                        Message::Fixed("MCP requests must have Content-Type: application/json."),
                    )),
                )
            });
        }
        McpPath::Agent(_) | McpPath::AgentCard(_) => return None,
    }
    let modern = header(headers, "mcp-protocol-version") == Some(MODERN);
    if *method == Method::POST {
        let ctype = header(headers, CONTENT_TYPE)
            .and_then(|c| c.split(';').next())
            .map(|c| c.trim().to_ascii_lowercase());
        if ctype.as_deref() != Some("application/json") {
            let fail = Fail::from(GatewayError::new(
                Code::UnsupportedMediaType,
                Stage::Parse,
                Message::Fixed("MCP requests must have Content-Type: application/json."),
            ));
            return Some(gateway_error(env, &fail));
        }
        if !(accepts(accept, "application/json") && accepts(accept, "text/event-stream")) {
            return Some(gateway_error(env, &not_acceptable()));
        }
    } else if *method == Method::GET {
        if modern {
            return Some(gateway_error(env, &method_not_allowed()));
        }
        if !accepts(accept, "text/event-stream") {
            return Some(gateway_error(env, &not_acceptable()));
        }
    } else if *method == Method::DELETE && modern {
        return Some(gateway_error(env, &method_not_allowed()));
    }
    None
}

fn not_acceptable() -> Fail {
    Fail::from(GatewayError::new(
        Code::NotAcceptable,
        Stage::Accept,
        Message::Fixed("The Accept header must admit application/json and text/event-stream."),
    ))
}

pub(crate) fn method_not_allowed() -> Fail {
    Fail::from(
        GatewayError::new(
            Code::MethodNotAllowed,
            Stage::Accept,
            Message::Fixed("Method not allowed on this MCP endpoint."),
        )
        .header(ALLOW, HeaderValue::from_static("GET, POST, DELETE")),
    )
}

/// Serves one authenticated MCP or A2A request.
pub async fn serve(
    env: Env,
    path: McpPath,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
    caller: Option<Caller>,
) -> Response<Body> {
    match path {
        McpPath::Gateway(name) => {
            let caller = caller.unwrap_or_default();
            gateway(env, &name, method, headers, body, caller).await
        }
        McpPath::SseStream(name) => match request(env, &name, headers, caller.unwrap_or_default(), &body) {
            Ok(req) => crate::clientsse::open(&req),
            Err(r) => *r,
        },
        McpPath::SseMessage(name, handle) => {
            match request(env, &name, headers, caller.unwrap_or_default(), &body) {
                Ok(req) => crate::clientsse::post(req, handle.as_deref(), body),
                Err(r) => *r,
            }
        }
        McpPath::Agent(name) => a2a::rpc(env, &name, headers, body, caller.unwrap_or_default()).await,
        McpPath::AgentCard(name) => a2a::card(env, &name, caller).await,
    }
}

/// The request to an existing gateway, with the node's session keys.
fn request(
    env: Env,
    name: &str,
    headers: HeaderMap,
    caller: Caller,
    body: &[u8],
) -> Result<Req, Box<Response<Body>>> {
    let Some(gw) = env.snap.gateway(name).cloned() else {
        let id = rpc_id(body);
        let fail = Fail::gateway(
            hygiene::MCP_GATEWAY_NOT_FOUND,
            Stage::Resolve,
            Message::built(format!("MCP gateway not found: {name}")),
        )
        .id(&id);
        return Err(Box::new(gateway_error(&env, &fail)));
    };
    let Some(keys) = Keyring::new(env.state.keys(&env.snap.session_keys)) else {
        let fail = Fail::gateway(
            reasons::SESSION_STORE_UNAVAILABLE,
            Stage::Authenticate,
            Message::Fixed("The MCP session key is not available on this node."),
        )
        .id(&rpc_id(body));
        return Err(Box::new(gateway_error(&env, &fail)));
    };
    Ok(Req {
        env,
        gw,
        caller,
        headers,
        keys,
    })
}

async fn gateway(
    env: Env,
    name: &str,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
    caller: Caller,
) -> Response<Body> {
    let req = match request(env, name, headers, caller, &body) {
        Ok(r) => r,
        Err(resp) => return *resp,
    };
    match method {
        Method::GET => legacy::get(req).await,
        Method::DELETE => legacy::delete(req).await,
        _ => post(req, body).await,
    }
}

/// The JSON-RPC id of a request body, or null when it has none or cannot be parsed.
fn rpc_id(body: &[u8]) -> Value {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("id").cloned())
        .unwrap_or(Value::Null)
}

pub(crate) async fn post(req: Req, body: Bytes) -> Response<Body> {
    let value: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            let f = Fail::protocol(
                -32700,
                400,
                hygiene::INVALID_JSON,
                Stage::Parse,
                Message::Fixed("Parse error: the body is not valid JSON."),
            );
            return req.error(&f);
        }
    };
    let msg = match classify(&value) {
        Ok(m) => {
            let modern = req.header("mcp-protocol-version") == Some(MODERN);
            req.env.log.update(|f| {
                f.method = m.method().map(str::to_owned);
                f.request_id = m.id().cloned();
                f.legacy = !modern;
                f.protocol_version = Some(
                    req.header("mcp-protocol-version")
                        .unwrap_or(upstream::DEFAULT_LEGACY)
                        .to_owned(),
                );
            });
            m
        }
        Err(id) => {
            let f = Fail::gateway(
                reasons::INVALID_JSONRPC,
                Stage::Parse,
                Message::Fixed("The body is not a JSON-RPC 2.0 message."),
            )
            .id(&id);
            return req.error(&f);
        }
    };
    let version = req.header("mcp-protocol-version").map(str::to_owned);
    match version.as_deref() {
        Some(MODERN) => modern::post(req, msg).await,
        None => legacy::post(req, msg, value, None).await,
        Some(v) if upstream::is_legacy(v) => {
            let v = v.to_owned();
            legacy::post(req, msg, value, Some(v)).await
        }
        Some(v) => req.error(&unsupported(v, &msg)),
    }
}

/// A version header outside the supported set: a modern-style error when the body carries the modern
/// `_meta` version, else the legacy one with no protocol-defined data.
fn unsupported(v: &str, msg: &Msg) -> Fail {
    let id = msg.id().cloned().unwrap_or(Value::Null);
    let message = Message::built(format!("unsupported MCP protocol version: {v}"));
    let has_meta = msg
        .params()
        .and_then(|p| p.get("_meta"))
        .and_then(|m| m.get(crate::modern::META_PV))
        .is_some();
    if has_meta {
        Fail::protocol(
            -32022,
            400,
            reasons::MCP_PROTOCOL_VERSION_UNSUPPORTED,
            Stage::Parse,
            message,
        )
        .data("supported", json!(upstream::supported()))
        .data("requested", Value::from(v))
        .id(&id)
    } else {
        Fail::gateway(reasons::MCP_PROTOCOL_VERSION_UNSUPPORTED, Stage::Parse, message).id(&id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_forms() {
        for ok in [
            "*/*",
            "application/*, text/*",
            "application/json, text/event-stream",
        ] {
            assert!(
                accepts(Some(ok), "application/json") && accepts(Some(ok), "text/event-stream"),
                "{ok}"
            );
        }
        assert!(!accepts(Some("application/json"), "text/event-stream"));
        assert!(!accepts(None, "text/event-stream"));
    }

    #[test]
    fn the_rpc_id_keeps_its_json_type() {
        assert_eq!(
            rpc_id(br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#),
            Value::from(1)
        );
        assert_eq!(rpc_id(br#"{"id":"a"}"#), Value::from("a"));
        assert_eq!(rpc_id(b"not json"), Value::Null);
    }
}
