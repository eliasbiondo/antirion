//! Requests to upstream MCP servers over Streamable HTTP: header assembly, answer classification
//! (JSON, SSE, accepted, failure) and the retry rule that never re-sends a written non-idempotent call.

use std::time::Duration;

use antirion_core::Stage;
use antirion_egress::{InboundBody, TransportError};
use antirion_error::reason::hygiene;
use antirion_error::{Code, GatewayError, Message};
use bytes::Bytes;
use http::header::{ACCEPT, CONTENT_TYPE, USER_AGENT};
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use http_body_util::BodyExt;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::compile::ServerRt;
use crate::egress::Egress;
use crate::reasons;
use crate::rpc::Fail;
use crate::sse::{ReadError, Reader};

pub(crate) const LEGACY_VERSIONS: [&str; 4] = ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];
pub(crate) const LATEST_LEGACY: &str = "2025-11-25";
pub(crate) const MODERN: &str = "2026-07-28";
/// Assumed for a legacy request without version information.
pub(crate) const DEFAULT_LEGACY: &str = "2025-03-26";
const MAX_JSON_ANSWER: usize = 64 << 20;

pub(crate) fn is_legacy(v: &str) -> bool {
    LEGACY_VERSIONS.contains(&v)
}

/// Every version the gateway supports, ascending.
pub(crate) fn supported() -> Vec<&'static str> {
    let mut all = LEGACY_VERSIONS.to_vec();
    all.push(MODERN);
    all
}

/// The era an upstream request is sent in.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Era {
    Legacy,
    Modern,
}

/// How one upstream request is addressed.
#[derive(Debug)]
pub(crate) struct Call<'a> {
    pub(crate) server: &'a ServerRt,
    pub(crate) era: Era,
    /// The protocol version the server speaks in this exchange.
    pub(crate) version: Option<&'a str>,
    pub(crate) session: Option<&'a str>,
    /// Per-request headers: client headers allowed for this server, identity headers, and the modern
    /// routing headers.
    pub(crate) headers: &'a HeaderMap,
    pub(crate) timeout: Duration,
}

/// An answer with the upstream session id it carried, if any.
#[derive(Debug)]
pub(crate) struct Reply {
    pub(crate) answer: Answer,
    pub(crate) session: Option<String>,
}

/// What an upstream answered.
#[derive(Debug)]
pub(crate) enum Answer {
    Json(Value),
    Stream(Source),
    Empty,
}

/// Messages of one streamed answer: an SSE response body, or the share of an HTTP+SSE session's
/// stream that belongs to this request.
#[derive(Debug)]
pub(crate) enum Source {
    Sse(Reader),
    Channel(mpsc::UnboundedReceiver<Value>),
}

impl Source {
    /// The next JSON-RPC message, skipping events that are not one.
    pub(crate) async fn next(&mut self, idle: Duration) -> Result<Value, ReadError> {
        self.next_event(idle).await.map(|(v, _)| v)
    }

    /// The next JSON-RPC message with the SSE event id it came with, if any.
    pub(crate) async fn next_event(&mut self, idle: Duration) -> Result<(Value, Option<String>), ReadError> {
        match self {
            Source::Sse(reader) => loop {
                let ev = reader.next(idle).await?;
                if ev.event.as_deref().is_some_and(|e| e != "message") {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<Value>(&ev.data) {
                    return Ok((v, ev.id.filter(|i| !i.is_empty())));
                }
            },
            Source::Channel(rx) => match tokio::time::timeout(idle, rx.recv()).await {
                Err(_) => Err(ReadError::Idle),
                Ok(None) => Err(ReadError::Ended),
                Ok(Some(v)) => Ok((v, None)),
            },
        }
    }
}

/// Why an upstream exchange failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UpError {
    Connect,
    /// The egress rules refused the server's address.
    Denied,
    Tls,
    Reset,
    Timeout,
    Status(u16),
    NotMcp,
    SessionGone,
    Auth(u16),
    /// The server answered a gateway request with a JSON-RPC error or an unusable result.
    Rejected,
    /// The server exchanges the caller's JWT for its token, and the caller presented none.
    SubjectRequired,
}

impl UpError {
    /// What went wrong, in words for an operator.
    pub(crate) fn describe(&self) -> String {
        match self {
            UpError::Connect => "could not connect".to_owned(),
            UpError::Denied => "the egress rules refuse its address".to_owned(),
            UpError::Tls => "the TLS handshake failed".to_owned(),
            UpError::Reset => "the connection was reset".to_owned(),
            UpError::Timeout => "it did not answer in time".to_owned(),
            UpError::Status(s) => format!("it answered HTTP {s}"),
            UpError::NotMcp => "it did not answer as an MCP server".to_owned(),
            UpError::SessionGone => "its session ended".to_owned(),
            UpError::Auth(s) => format!("it refused the gateway's credentials (HTTP {s})"),
            UpError::Rejected => "it answered with an error".to_owned(),
            UpError::SubjectRequired => "it needs the caller's token, and the caller sent none".to_owned(),
        }
    }

    fn retryable(&self, idempotent: bool) -> bool {
        match self {
            UpError::Connect => true,
            UpError::Reset => idempotent,
            UpError::Status(s) => idempotent && (*s >= 500 || *s == 429),
            UpError::Timeout
            | UpError::Denied
            | UpError::Tls
            | UpError::NotMcp
            | UpError::SessionGone
            | UpError::Auth(_)
            | UpError::Rejected
            | UpError::SubjectRequired => false,
        }
    }

    /// The client-facing failure when this server was the one the request targeted.
    pub(crate) fn fail(&self, server: &str) -> Fail {
        let (err, status) = match self {
            UpError::Connect | UpError::Reset => (
                GatewayError::with_reason(
                    hygiene::CONNECTION_FAILED,
                    Stage::Attempt,
                    Message::Fixed("The MCP server could not be reached."),
                ),
                None,
            ),
            UpError::Denied => (
                GatewayError::with_reason(
                    antirion_egress::reasons::EGRESS_DENIED,
                    Stage::Attempt,
                    Message::Fixed("The egress rules do not allow the MCP server's address."),
                ),
                None,
            ),
            UpError::Tls => (
                GatewayError::with_reason(
                    antirion_egress::reasons::TLS_HANDSHAKE_FAILED,
                    Stage::Attempt,
                    Message::Fixed("The TLS handshake with the MCP server failed."),
                ),
                None,
            ),
            UpError::Timeout => (
                GatewayError::new(
                    Code::UpstreamTimeout,
                    Stage::Attempt,
                    Message::Fixed("The MCP server did not answer in time."),
                ),
                None,
            ),
            UpError::Rejected => (
                GatewayError::with_reason(
                    hygiene::UPSTREAM_SERVER_ERROR,
                    Stage::Attempt,
                    Message::Fixed("The MCP server refused the request."),
                ),
                None,
            ),
            UpError::Status(s) => (
                GatewayError::with_reason(
                    hygiene::UPSTREAM_SERVER_ERROR,
                    Stage::Attempt,
                    Message::Fixed("The MCP server failed."),
                ),
                Some(*s),
            ),
            UpError::NotMcp => (
                GatewayError::with_reason(
                    reasons::UPSTREAM_NOT_MCP,
                    Stage::Attempt,
                    Message::Fixed("The MCP server did not answer with MCP."),
                ),
                None,
            ),
            UpError::SessionGone => (
                GatewayError::with_reason(
                    reasons::UPSTREAM_SESSION_NOT_FOUND,
                    Stage::Attempt,
                    Message::Fixed("The MCP server no longer knows this session; initialize again."),
                ),
                None,
            ),
            UpError::Auth(s) => (
                GatewayError::with_reason(
                    hygiene::UPSTREAM_CREDENTIAL_REJECTED,
                    Stage::Attempt,
                    Message::Fixed("The MCP server refused the gateway's credential."),
                ),
                Some(*s).filter(|s| *s != 0),
            ),
            UpError::SubjectRequired => (
                GatewayError::with_reason(
                    reasons::SUBJECT_TOKEN_REQUIRED,
                    Stage::Attempt,
                    Message::Fixed("This MCP server needs the caller's JWT, and the caller presented none."),
                ),
                None,
            ),
        };
        let mut f = Fail::from(err).provider(server);
        if let Some(s) = status {
            f = f.upstream_status(s);
        }
        f
    }
}

fn header(name: &'static str, value: &str, into: &mut HeaderMap) {
    if let Ok(v) = HeaderValue::from_str(value) {
        into.insert(HeaderName::from_static(name), v);
    }
}

fn build_headers(call: &Call<'_>, json_body: bool) -> HeaderMap {
    let mut h = call.server.headers.clone();
    for (name, value) in call.headers {
        h.insert(name.clone(), value.clone());
    }
    if json_body {
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        h.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
    } else {
        h.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
    }
    h.insert(
        USER_AGENT,
        HeaderValue::from_static(concat!("antirion/", env!("CARGO_PKG_VERSION"))),
    );
    let send_version = match call.era {
        Era::Modern => Some(MODERN),
        Era::Legacy => call.version.filter(|v| *v >= "2025-06-18"),
    };
    if let Some(v) = send_version {
        header("mcp-protocol-version", v, &mut h);
    }
    if let Some(s) = call.session.filter(|_| call.era == Era::Legacy) {
        header("mcp-session-id", s, &mut h);
    }
    h
}

fn content_type(headers: &HeaderMap) -> String {
    headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

/// Sends one HTTP request to `uri` with the call's headers; one attempt.
pub(crate) async fn send_once(
    egress: &Egress,
    call: &Call<'_>,
    method: Method,
    uri: Uri,
    body: Option<Bytes>,
) -> Result<Reply, UpError> {
    let mut headers = build_headers(call, body.is_some());
    if let Some(auth) = &call.server.auth {
        let value = auth
            .authorization(egress, egress.subject.as_deref(), call.server.init_timeout)
            .await?;
        headers.insert(http::header::AUTHORIZATION, value);
    }
    let resp = egress
        .send(method, uri, headers, body.unwrap_or_default(), call.timeout)
        .await
        .map_err(|e| match e {
            TransportError::Connect
            | TransportError::ConnectTimeout
            | TransportError::Dns
            | TransportError::UnsupportedScheme(_) => UpError::Connect,
            TransportError::EgressDenied(_) => UpError::Denied,
            TransportError::Tls => UpError::Tls,
            TransportError::Timeout => UpError::Timeout,
            TransportError::Reset | TransportError::RefusedStream => UpError::Reset,
            TransportError::UnexpectedEncoding => UpError::NotMcp,
        })?;
    let ctype = content_type(&resp.headers);
    let session = resp
        .headers
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let answer = classify(call, resp.status, &ctype, resp.body).await?;
    Ok(Reply { answer, session })
}

async fn classify(
    call: &Call<'_>,
    status: StatusCode,
    ctype: &str,
    body: InboundBody,
) -> Result<Answer, UpError> {
    if status == StatusCode::NOT_FOUND && call.session.is_some() && call.era == Era::Legacy {
        return Err(UpError::SessionGone);
    }
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Err(UpError::Auth(status.as_u16()));
    }
    if ctype == "text/event-stream" && status.is_success() {
        return Ok(Answer::Stream(Source::Sse(Reader::new(body))));
    }
    if status == StatusCode::ACCEPTED || status == StatusCode::NO_CONTENT {
        return Ok(Answer::Empty);
    }
    let bytes = match tokio::time::timeout(call.timeout, body.collect()).await {
        Err(_) => return Err(UpError::Timeout),
        Ok(Err(_)) => return Err(UpError::Reset),
        Ok(Ok(c)) => c.to_bytes(),
    };
    if bytes.len() > MAX_JSON_ANSWER {
        return Err(UpError::NotMcp);
    }
    if bytes.is_empty() {
        return if status.is_success() {
            Ok(Answer::Empty)
        } else {
            Err(UpError::Status(status.as_u16()))
        };
    }
    let parsed = (ctype == "application/json")
        .then(|| serde_json::from_slice::<Value>(&bytes).ok())
        .flatten()
        .filter(|v| v.get("jsonrpc").is_some());
    match parsed {
        Some(v) if status.is_success() || status == StatusCode::BAD_REQUEST => Ok(Answer::Json(v)),
        _ if status.is_server_error() || status.is_client_error() => Err(UpError::Status(status.as_u16())),
        Some(v) => Ok(Answer::Json(v)),
        None => Err(UpError::NotMcp),
    }
}

/// Sends one request with the retry rule: idempotent methods are retried up to the server's
/// `retries` on retryable failures; others only when the connection was never made.
pub(crate) async fn send(
    egress: &Egress,
    call: &Call<'_>,
    method: Method,
    body: Option<Bytes>,
    idempotent: bool,
) -> Result<Reply, UpError> {
    let extra = if idempotent { call.server.retries } else { 1 };
    let mut attempt = 0u8;
    loop {
        let r = send_once(
            egress,
            call,
            method.clone(),
            call.server.url.clone(),
            body.clone(),
        )
        .await;
        match r {
            Err(e) if attempt < extra && e.retryable(idempotent) => attempt += 1,
            other => return other,
        }
    }
}

/// Reads an answer until the response to `id`, ignoring other messages.
pub(crate) async fn response_to(answer: Answer, id: &Value, idle: Duration) -> Result<Value, UpError> {
    match answer {
        Answer::Json(v) => Ok(v),
        Answer::Empty => Err(UpError::NotMcp),
        Answer::Stream(mut src) => loop {
            match src.next(idle).await {
                Ok(m)
                    if m.get("id") == Some(id) && (m.get("result").is_some() || m.get("error").is_some()) =>
                {
                    return Ok(m);
                }
                Ok(_) => {}
                Err(ReadError::Idle) => return Err(UpError::Timeout),
                Err(ReadError::Ended) => return Err(UpError::Reset),
            }
        },
    }
}

/// Methods that may be retried (they have no side effects).
pub(crate) fn idempotent(method: &str) -> bool {
    matches!(
        method,
        "server/discover"
            | "initialize"
            | "tools/list"
            | "prompts/list"
            | "prompts/get"
            | "resources/list"
            | "resources/templates/list"
            | "resources/read"
            | "completion/complete"
            | "tasks/get"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_rule() {
        assert!(UpError::Reset.retryable(true));
        assert!(!UpError::Reset.retryable(false));
        assert!(UpError::Connect.retryable(false));
        assert!(UpError::Status(503).retryable(true));
        assert!(!UpError::Status(503).retryable(false));
        assert!(!UpError::Timeout.retryable(true));
    }

    #[test]
    fn versions() {
        assert!(is_legacy("2025-06-18") && !is_legacy(MODERN));
        assert_eq!(supported().last(), Some(&MODERN));
        assert!(idempotent("tools/list") && !idempotent("tools/call"));
    }
}
