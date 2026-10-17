//! A link: how one request talks to one upstream server, whatever the transport and era. Legacy
//! links carry the server's session (a Streamable HTTP session id or a held HTTP+SSE stream); modern
//! links carry the client's `_meta` and set the routing headers.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use base64::Engine;
use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method};
use serde_json::{Map, Value, json};

use crate::compile::{ServerRt, Transport};
use crate::http::Env;
use crate::sse::ReadError;
use crate::ssetransport::{self, SseConn};
use crate::upstream::{self, Answer, Call, Era, Source, UpError};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
/// Longer upstream session ids make that server's initialization fail, bounding the token size.
const MAX_UPSTREAM_SESSION_ID: usize = 512;

/// A fresh id for a request the gateway itself sends upstream.
pub(crate) fn gateway_id() -> Value {
    Value::String(format!("antirion-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed)))
}

/// The `Mcp-Name` (or `Mcp-Param-*`) form of a value: as is when plain header-safe ASCII, else the
/// base64 sentinel form.
pub(crate) fn header_value_form(v: &str) -> String {
    let plain = v == v.trim()
        && v.bytes().all(|b| (0x20..=0x7e).contains(&b))
        && !(v.starts_with("=?base64?") && v.ends_with("?="));
    if plain {
        v.to_owned()
    } else {
        format!(
            "=?base64?{}?=",
            base64::engine::general_purpose::STANDARD.encode(v.as_bytes())
        )
    }
}

/// Decodes the sentinel form of a header value; `None` when malformed.
pub(crate) fn decode_header_value(v: &str) -> Option<String> {
    match v.strip_prefix("=?base64?").and_then(|r| r.strip_suffix("?=")) {
        Some(b64) => base64::engine::general_purpose::STANDARD
            .decode(b64)
            .ok()
            .and_then(|b| String::from_utf8(b).ok()),
        None => Some(v.to_owned()),
    }
}

/// The item a named request addresses: `params.name`, or `params.uri` for `resources/read`.
pub(crate) fn item_of(method: &str, params: Option<&Value>) -> Option<String> {
    let key = if method == "resources/read" { "uri" } else { "name" };
    params?.get(key)?.as_str().map(str::to_owned)
}

pub(crate) const NAMED: [&str; 3] = ["tools/call", "prompts/get", "resources/read"];

/// One server as seen by one request.
#[derive(Clone, Debug)]
pub(crate) struct Link {
    pub(crate) server: Arc<ServerRt>,
    pub(crate) era: Era,
    /// The version spoken with this server (legacy).
    pub(crate) pv: String,
    pub(crate) sid: Option<String>,
    pub(crate) sse: Option<Arc<SseConn>>,
    /// Upstream headers of this request for this server.
    pub(crate) headers: HeaderMap,
    /// The client's `_meta` (modern).
    pub(crate) meta: Option<Value>,
}

impl Link {
    fn call<'a>(&'a self, headers: &'a HeaderMap, timeout: Duration) -> Call<'a> {
        Call {
            server: &self.server,
            era: self.era,
            version: Some(&self.pv),
            session: self.sid.as_deref(),
            headers,
            timeout,
        }
    }

    fn request_headers(&self, method: &str, params: Option<&Value>) -> HeaderMap {
        let mut h = self.headers.clone();
        if self.era == Era::Modern {
            if let Ok(v) = HeaderValue::from_str(method) {
                h.insert(HeaderName::from_static("mcp-method"), v);
            }
            if NAMED.contains(&method)
                && let Some(item) = item_of(method, params)
                && let Ok(v) = HeaderValue::from_str(&header_value_form(&item))
            {
                h.insert(HeaderName::from_static("mcp-name"), v);
            }
        }
        h
    }

    /// The params to send: for gateway-originated modern requests, the client's `_meta` is added.
    fn params(&self, params: Option<Value>) -> Option<Value> {
        match (&self.meta, self.era) {
            (Some(meta), Era::Modern) => {
                let mut p = match params {
                    Some(Value::Object(o)) => o,
                    _ => Map::new(),
                };
                p.entry("_meta").or_insert_with(|| meta.clone());
                Some(Value::Object(p))
            }
            _ => params,
        }
    }

    /// The answer of a server the gateway serves itself or through child processes, if this is one.
    async fn local(&self, env: &Env, msg: &Value, timeout: Duration) -> Option<Result<Value, UpError>> {
        match (&self.server.stdio, &self.server.openapi) {
            (Some(spec), _) => {
                let pool = env.state.stdio_pool(&env.snap, &self.server, spec);
                let answer = if msg.get("method").and_then(Value::as_str) == Some("initialize") {
                    pool.handshake(env, self.server.init_timeout).await
                } else {
                    pool.request(env, msg, self.server.init_timeout, timeout).await
                };
                Some(answer)
            }
            (None, Some(api)) => Some(api.answer(env, &self.server, msg).await),
            (None, None) => None,
        }
    }

    /// Sends a JSON-RPC request and returns the raw answer.
    pub(crate) async fn send(
        &self,
        env: &Env,
        id: &Value,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<Answer, UpError> {
        let params = self.params(params);
        let mut msg = json!({"jsonrpc": "2.0", "id": id, "method": method});
        if let (Some(p), Some(obj)) = (params.clone(), msg.as_object_mut()) {
            obj.insert("params".into(), p);
        }
        if let Some(answer) = self.local(env, &msg, timeout).await {
            return answer.map(Answer::Json);
        }
        let headers = self.request_headers(method, params.as_ref());
        if let Some(conn) = &self.sse {
            let src = ssetransport::request(&env.egress, conn, &self.server, &headers, &msg).await?;
            return Ok(Answer::Stream(src));
        }
        self.usable()?;
        let body = Bytes::from(serde_json::to_vec(&msg).unwrap_or_default());
        let call = self.call(&headers, timeout);
        upstream::send(
            &env.egress,
            &call,
            Method::POST,
            Some(body),
            upstream::idempotent(method),
        )
        .await
        .map(|r| r.answer)
    }

    fn usable(&self) -> Result<(), UpError> {
        match self.server.transport {
            Transport::Unsupported | Transport::Stdio | Transport::OpenApi => Err(UpError::Connect),
            Transport::Sse => Err(UpError::SessionGone),
            Transport::StreamableHttp => Ok(()),
        }
    }

    /// Whether the gateway answers for the server itself or through child processes.
    fn is_local(&self) -> bool {
        matches!(self.server.transport, Transport::Stdio | Transport::OpenApi)
    }

    /// Sends `initialize` and records the session id the server issued, if any.
    pub(crate) async fn initialize(&mut self, env: &Env, params: Value) -> Result<Value, UpError> {
        let id = gateway_id();
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": "initialize", "params": params});
        let timeout = self.server.init_timeout;
        if let Some(answer) = self.local(env, &msg, timeout).await {
            return answer;
        }
        if let Some(conn) = &self.sse {
            let headers = self.headers.clone();
            let src = ssetransport::request(&env.egress, conn, &self.server, &headers, &msg).await?;
            return upstream::response_to(Answer::Stream(src), &id, timeout).await;
        }
        self.usable()?;
        let body = Bytes::from(serde_json::to_vec(&msg).unwrap_or_default());
        let headers = self.headers.clone();
        let call = self.call(&headers, timeout);
        let reply = upstream::send(&env.egress, &call, Method::POST, Some(body), true).await?;
        if let Some(sid) = reply.session {
            if sid.len() > MAX_UPSTREAM_SESSION_ID {
                return Err(UpError::Rejected);
            }
            self.sid = Some(sid);
        }
        upstream::response_to(reply.answer, &id, timeout).await
    }

    /// Sends a gateway-originated request and waits for its response message.
    pub(crate) async fn ask(&self, env: &Env, method: &str, params: Option<Value>) -> Result<Value, UpError> {
        let id = gateway_id();
        let timeout = if method == "initialize" {
            self.server.init_timeout
        } else {
            self.server.request_timeout
        };
        let answer = self.send(env, &id, method, params, timeout).await?;
        upstream::response_to(answer, &id, timeout).await
    }

    /// Posts a notification or a response (no answer expected).
    pub(crate) async fn post(&self, env: &Env, msg: &Value) -> Result<(), UpError> {
        if self.is_local() {
            return Ok(());
        }
        let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
        let headers = self.request_headers(method, msg.get("params"));
        if let Some(conn) = &self.sse {
            return ssetransport::post(&env.egress, conn, &self.server, &headers, msg).await;
        }
        self.usable()?;
        let body = Bytes::from(serde_json::to_vec(msg).unwrap_or_default());
        let call = self.call(&headers, self.server.request_timeout);
        upstream::send(&env.egress, &call, Method::POST, Some(body), false)
            .await
            .map(|_| ())
    }

    /// Ends the upstream session: `DELETE` for Streamable HTTP, closing the stream for HTTP+SSE.
    pub(crate) async fn close(&self, env: &Env) {
        if let Some(conn) = &self.sse {
            conn.close();
            if let Some(sid) = &self.sid {
                env.state.sse.lock().remove(sid);
            }
            return;
        }
        if self.sid.is_none() || self.usable().is_err() {
            return;
        }
        let call = self.call(&self.headers, self.server.request_timeout);
        let _ = upstream::send_once(&env.egress, &call, Method::DELETE, self.server.url.clone(), None).await;
    }

    /// Opens the server-to-client stream of this session, when the server offers one; with
    /// `last_event_id`, the server is asked to resume after that event.
    pub(crate) async fn listen(&self, env: &Env, last_event_id: Option<&str>) -> Option<Source> {
        if let Some(conn) = &self.sse {
            return Some(Source::Channel(conn.listen()));
        }
        self.usable().ok()?;
        let mut headers = self.headers.clone();
        if let Some(id) = last_event_id
            && let Ok(v) = HeaderValue::from_str(id)
        {
            headers.insert(HeaderName::from_static("last-event-id"), v);
        }
        let call = self.call(&headers, self.server.request_timeout);
        match upstream::send_once(&env.egress, &call, Method::GET, self.server.url.clone(), None).await {
            Ok(upstream::Reply {
                answer: Answer::Stream(src),
                ..
            }) => Some(src),
            _ => None,
        }
    }
}

/// Reads one message of a streamed answer, mapping silence and ends to upstream failures.
pub(crate) async fn next_message(src: &mut Source, idle: Duration) -> Result<Value, UpError> {
    src.next(idle).await.map_err(|e| match e {
        ReadError::Idle => UpError::Timeout,
        ReadError::Ended => UpError::Reset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_values_use_the_sentinel_only_when_needed() {
        assert_eq!(header_value_form("add"), "add");
        let encoded = header_value_form("wetter-gr\u{f6}\u{df}e");
        assert!(encoded.starts_with("=?base64?") && encoded.ends_with("?="));
        assert_eq!(decode_header_value(&encoded).unwrap(), "wetter-gr\u{f6}\u{df}e");
        assert_eq!(decode_header_value("plain").unwrap(), "plain");
        assert!(decode_header_value("=?base64?!!?=").is_none());
        assert!(header_value_form("=?base64?eA==?=").starts_with("=?base64?PT9"));
    }
}
