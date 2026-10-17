//! The client-facing 2024-11-05 HTTP+SSE transport: `GET /mcp/<name>/sse` opens a stream whose
//! `endpoint` event names `/mcp/<name>/message?sessionId=<handle>`, and each message POSTed there is
//! answered `202` and served as on the Streamable HTTP endpoint, its answers sent on the stream. The
//! handle is a session token bound to the gateway and the principal; the stream lives on the node
//! that holds it, so a POST for a handle this node does not hold is 404 and never queued.

use std::sync::Arc;

use antirion_core::Stage;
use antirion_error::Message;
use bytes::Bytes;
use http::{HeaderName, HeaderValue, Response};
use http_body_util::BodyExt;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::http::Req;
use crate::rpc::Fail;
use crate::sse::Parser;
use crate::token::SessionClaims;
use crate::{Body, reasons, respond};

/// The protocol version the transport belongs to.
const TRANSPORT_VERSION: &str = "2024-11-05";
/// Messages waiting for a slow client before newer ones are dropped.
const QUEUE: usize = 1024;

/// One open client stream.
#[derive(Debug)]
pub(crate) struct ClientStream {
    tx: mpsc::Sender<Value>,
    /// The Streamable HTTP session `initialize` issued, used by the stream's later messages.
    session: Mutex<Option<String>>,
}

/// Removes the stream from the node's registry when the client goes away.
struct Registered {
    state: Arc<crate::state::McpState>,
    handle: String,
}

impl Drop for Registered {
    fn drop(&mut self) {
        self.state.client_streams.lock().remove(&self.handle);
    }
}

fn not_found() -> Fail {
    Fail::gateway(
        reasons::MCP_SESSION_NOT_FOUND,
        Stage::Parse,
        Message::Fixed("The MCP session is unknown or has ended; open a new stream."),
    )
}

/// `GET /mcp/<name>/sse`.
pub(crate) fn open(req: &Req) -> Response<Body> {
    let now = req.env.now.0 / 1_000_000;
    let claims = SessionClaims {
        v: 1,
        gw: req.gw.name.clone(),
        sub: req.caller.principal.clone(),
        iat: now,
        exp: req.env.now.saturating_add(req.env.snap.max_lifetime).0 / 1_000_000,
        pv: TRANSPORT_VERSION.to_owned(),
        caps: json!({}),
        ci: json!({}),
        up: Vec::new(),
    };
    let Some(handle) = req.keys.seal_session(&claims) else {
        return req.error(&Fail::gateway(
            reasons::SESSION_STORE_UNAVAILABLE,
            Stage::Respond,
            Message::Fixed("The MCP session could not be sealed."),
        ));
    };
    let (tx, rx) = mpsc::channel(QUEUE);
    req.env.state.client_streams.lock().insert(
        handle.clone(),
        Arc::new(ClientStream {
            tx,
            session: Mutex::new(None),
        }),
    );
    let guard = Registered {
        state: Arc::clone(&req.env.state),
        handle: handle.clone(),
    };
    let endpoint = format!(
        "{}/mcp/{}/message?sessionId={handle}",
        req.env.public_base, req.gw.name
    );
    let first = Bytes::from(format!("event: endpoint\ndata: {endpoint}\n\n"));
    let rest = futures_util::stream::unfold((rx, guard), |(mut rx, guard)| async move {
        let msg = rx.recv().await?;
        Some((respond::event(&msg), (rx, guard)))
    });
    respond::sse(futures_util::StreamExt::chain(
        futures_util::stream::once(async move { first }),
        rest,
    ))
}

/// Sends every JSON-RPC message of an answer to the client stream.
async fn forward(resp: Response<Body>, tx: &mpsc::Sender<Value>) {
    let streamed = resp
        .headers()
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.starts_with("text/event-stream"));
    let mut body = resp.into_body();
    let mut parser = Parser::default();
    let mut whole = Vec::new();
    while let Some(frame) = body.frame().await {
        let Ok(frame) = frame else { break };
        let Ok(data) = frame.into_data() else { continue };
        if !streamed {
            whole.extend_from_slice(&data);
            continue;
        }
        parser.push(&data);
        while let Some(ev) = parser.next_event() {
            if let Ok(v) = serde_json::from_str::<Value>(&ev.data)
                && tx.send(v).await.is_err()
            {
                return;
            }
        }
    }
    if !streamed && let Ok(v) = serde_json::from_slice::<Value>(&whole) {
        let _ = tx.send(v).await;
    }
}

/// `POST /mcp/<name>/message?sessionId=<handle>`.
pub(crate) fn post(mut req: Req, handle: Option<&str>, body: Bytes) -> Response<Body> {
    let stream = handle.and_then(|h| {
        let claims = req.keys.open_session(&req.gw.name, h)?;
        let now = req.env.now.0 / 1_000_000;
        if claims.sub != req.caller.principal || claims.exp <= now {
            return None;
        }
        req.env.state.client_streams.lock().get(h).cloned()
    });
    let Some(stream) = stream else {
        return req.error(&not_found());
    };
    req.headers.remove("mcp-protocol-version");
    req.headers.remove("mcp-session-id");
    if let Some(sid) = stream.session.lock().clone()
        && let Ok(v) = HeaderValue::from_str(&sid)
    {
        req.headers.insert(HeaderName::from_static("mcp-session-id"), v);
    }
    let tasks = req.env.tasks.clone();
    tasks.spawn(async move {
        let resp = crate::http::post(req, body).await;
        if let Some(sid) = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
            *stream.session.lock() = Some(sid.to_owned());
        }
        forward(resp, &stream.tx).await;
    });
    respond::accepted()
}
