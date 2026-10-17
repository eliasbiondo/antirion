//! The HTTP server: HTTP/1.1 and h2 with prior knowledge on one listener, a fixed answer per path.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use bytes::Bytes;
use futures_util::stream;
use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Empty, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::profile::Profile;
use crate::responses::Responses;
use crate::timer::Timer;

type Body = UnsyncBoxBody<Bytes, Infallible>;

/// Requests the mock answered as an upstream (control paths excluded).
#[derive(Debug, Default)]
pub struct Stats {
    upstream: AtomicU64,
    sink: AtomicU64,
}

impl Stats {
    #[must_use]
    pub fn upstream(&self) -> u64 {
        self.upstream.load(Ordering::Relaxed)
    }
}

#[derive(Debug)]
struct State {
    timer: Timer,
    responses: Responses,
    default: Profile,
    stats: Arc<Stats>,
    sessions: AtomicU64,
}

/// A request body is a stream when it asks for one; the bodies are the corpus's compact JSON.
const STREAM_TRUE: &[u8] = br#""stream":true"#;
/// Bodies at least this large get the large chat answer.
const LARGE_REQUEST: usize = 16 * 1024;

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn full(bytes: Bytes) -> Body {
    Full::new(bytes).boxed_unsync()
}

fn empty() -> Body {
    Empty::new().boxed_unsync()
}

fn respond(status: StatusCode, content_type: &'static str, body: Body) -> Response<Body> {
    let mut r = Response::new(body);
    *r.status_mut() = status;
    r.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    r
}

fn json_response(bytes: Bytes) -> Response<Body> {
    respond(StatusCode::OK, "application/json", full(bytes))
}

/// An event stream on a fixed timeline from `start`: `events[0]` after the profile's first delay,
/// event `i` one spacing after event `i - 1`, the closing events right after the last content event.
/// Deadlines are absolute, so timer overshoot never accumulates over a stream.
fn event_stream(
    state: &Arc<State>,
    events: Arc<[Bytes]>,
    profile: Profile,
    start: Instant,
) -> Response<Body> {
    let first = profile.first_event();
    let spacing = profile.spacing();
    let idles = profile.idles();
    let last = events.len().saturating_sub(1);
    let state = Arc::clone(state);
    let frames = stream::unfold(0usize, move |i| {
        let events = Arc::clone(&events);
        let state = Arc::clone(&state);
        async move {
            if idles && i == 1 {
                std::future::pending::<()>().await;
            }
            let ev = events.get(i)?.clone();
            let steps = u32::try_from(i.min(last.saturating_sub(1))).unwrap_or(u32::MAX);
            state.timer.at(start + first + spacing * steps).await;
            Some((Ok::<_, Infallible>(Frame::data(ev)), i + 1))
        }
    });
    let mut r = respond(
        StatusCode::OK,
        "text/event-stream",
        StreamBody::new(frames).boxed_unsync(),
    );
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    r
}

/// Splits `/<profile>/rest` into the profile and `/rest`; paths without one use the default.
fn split_profile(path: &str, default: Profile) -> (Profile, &str) {
    let trimmed = path.strip_prefix('/').unwrap_or(path);
    let head = trimmed.split('/').next().unwrap_or_default();
    match head.parse::<Profile>() {
        Ok(p) => (
            p,
            trimmed.get(head.len()..).filter(|r| !r.is_empty()).unwrap_or("/"),
        ),
        Err(_) => (default, path),
    }
}

async fn handle(req: Request<Incoming>, state: Arc<State>) -> Result<Response<Body>, Infallible> {
    let arrived = Instant::now();
    let path = req.uri().path().to_owned();
    let method = req.method().clone();
    if let Some(control) = path.strip_prefix("/__bench/") {
        return Ok(control_path(control, req, &state).await);
    }
    let headers = req.headers().clone();
    let Ok(body) = req
        .into_body()
        .collect()
        .await
        .map(http_body_util::Collected::to_bytes)
    else {
        return Ok(respond(StatusCode::BAD_REQUEST, "text/plain", empty()));
    };
    state.stats.upstream.fetch_add(1, Ordering::Relaxed);
    let (profile, rest) = split_profile(&path, state.default);
    let stream = contains(&body, STREAM_TRUE);
    let r = &state.responses;
    let response = match (method, rest) {
        (Method::POST, "/v1/chat/completions") if stream => {
            return Ok(event_stream(
                &state,
                r.chat_stream.clone().into(),
                profile,
                arrived,
            ));
        }
        (Method::POST, "/v1/messages") if stream => {
            return Ok(event_stream(
                &state,
                r.messages_stream.clone().into(),
                profile,
                arrived,
            ));
        }
        (Method::POST, "/v1/chat/completions") if body.len() >= LARGE_REQUEST => {
            json_response(r.chat_large.clone())
        }
        (Method::POST, "/v1/chat/completions") => json_response(r.chat_small.clone()),
        (Method::POST, "/v1/messages") => json_response(r.messages.clone()),
        (Method::POST, "/v1/embeddings") => json_response(r.embeddings.clone()),
        (Method::POST, "/mcp") => mcp(&body, &headers, &state),
        (Method::DELETE, "/mcp") => respond(StatusCode::OK, "text/plain", empty()),
        (Method::GET, "/mcp") => respond(StatusCode::METHOD_NOT_ALLOWED, "text/plain", empty()),
        _ => respond(StatusCode::NOT_FOUND, "text/plain", empty()),
    };
    state.timer.at(arrived + profile.head_delay()).await;
    Ok(response)
}

async fn control_path(control: &str, req: Request<Incoming>, state: &State) -> Response<Body> {
    if control.starts_with("null") {
        let _ = req.into_body().collect().await;
        state.stats.sink.fetch_add(1, Ordering::Relaxed);
        return respond(StatusCode::OK, "text/plain", empty());
    }
    if control.starts_with("hang") {
        std::future::pending::<()>().await;
    }
    match control {
        "health" => respond(StatusCode::OK, "text/plain", full(Bytes::from_static(b"ok"))),
        "stats" => {
            let body = json!({
                "upstream_requests": state.stats.upstream(),
                "sink_requests": state.stats.sink.load(Ordering::Relaxed),
            });
            json_response(Bytes::from(body.to_string()))
        }
        _ => respond(StatusCode::NOT_FOUND, "text/plain", empty()),
    }
}

const MODERN: &str = "2026-07-28";

fn mcp(body: &[u8], headers: &http::HeaderMap, state: &State) -> Response<Body> {
    let Ok(msg) = serde_json::from_slice::<Value>(body) else {
        return respond(StatusCode::BAD_REQUEST, "application/json", empty());
    };
    let Some(id) = msg.get("id").cloned() else {
        return respond(StatusCode::ACCEPTED, "application/json", empty());
    };
    let modern = headers
        .get("mcp-protocol-version")
        .is_some_and(|v| v.as_bytes() == MODERN.as_bytes());
    let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
    let info = json!({"name": "bench", "version": "1.0.0"});
    let caps = json!({"tools": {"listChanged": false}});
    let tool = json!({"name": "echo", "description": "Echoes a fixed text.",
                      "inputSchema": {"type": "object"}});
    let mut session = None;
    let result = match method {
        "initialize" => {
            let asked = msg
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("2025-06-18");
            if !modern {
                let n = state.sessions.fetch_add(1, Ordering::Relaxed);
                session = Some(format!("bench-session-{n}"));
            }
            json!({"protocolVersion": asked, "capabilities": caps, "serverInfo": info})
        }
        "server/discover" => json!({
            "supportedVersions": ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25", MODERN],
            "capabilities": caps, "serverInfo": info}),
        "tools/list" => json!({"tools": [tool]}),
        "tools/call" => json!({"content": [{"type": "text", "text": state.responses.mcp_tool_text}],
                               "isError": false}),
        "ping" => json!({}),
        other => {
            let err = json!({"jsonrpc": "2.0", "id": id,
                             "error": {"code": -32601, "message": format!("Method not found: {other}")}});
            return json_response(Bytes::from(err.to_string()));
        }
    };
    let answer = json!({"jsonrpc": "2.0", "id": id, "result": result});
    let mut r = json_response(Bytes::from(answer.to_string()));
    if let Some(sid) = session.and_then(|s| HeaderValue::from_str(&s).ok()) {
        r.headers_mut().insert("mcp-session-id", sid);
    }
    r
}

/// Serves until `shutdown` is cancelled; every connection ends with it.
pub async fn serve(
    listener: TcpListener,
    default: Profile,
    stats: Arc<Stats>,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    let state = Arc::new(State {
        timer: Timer::start()?,
        responses: Responses::build(),
        default,
        stats,
        sessions: AtomicU64::new(0),
    });
    let tracker = TaskTracker::new();
    loop {
        let (socket, _peer): (_, SocketAddr) = tokio::select! {
            () = shutdown.cancelled() => break,
            accepted = listener.accept() => match accepted {
                Ok(pair) => pair,
                Err(_) => continue,
            },
        };
        let _ = socket.set_nodelay(true);
        let state = Arc::clone(&state);
        let stop = shutdown.clone();
        tracker.spawn(async move {
            let service = service_fn(move |req| handle(req, Arc::clone(&state)));
            let builder = auto::Builder::new(TokioExecutor::new());
            let conn = builder.serve_connection(TokioIo::new(socket), service);
            tokio::select! {
                _ = conn => {}
                () = stop.cancelled() => {}
            }
        });
    }
    tracker.close();
    tracker.wait().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_prefixes_are_split() {
        assert_eq!(
            split_profile("/L60/v1/messages", Profile::L0),
            (Profile::L60, "/v1/messages")
        );
        assert_eq!(
            split_profile("/LS-idle/mcp", Profile::L0),
            (Profile::LsIdle, "/mcp")
        );
        assert_eq!(
            split_profile("/v1/embeddings", Profile::Ls),
            (Profile::Ls, "/v1/embeddings")
        );
    }

    #[test]
    fn stream_requests_are_detected() {
        assert!(contains(br#"{"model":"m","stream":true}"#, STREAM_TRUE));
        assert!(!contains(br#"{"model":"m","stream":false}"#, STREAM_TRUE));
    }
}
