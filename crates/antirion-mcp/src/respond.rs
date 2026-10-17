//! Building responses: JSON, empty, SSE and JSON-RPC errors, each with the response-path header.

use antirion_core::headers as h;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use http::header::CONTENT_TYPE;
use http::{HeaderMap, HeaderName, HeaderValue, Response, StatusCode};
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::Frame;
use serde_json::Value;

use crate::http::Env;
use crate::rpc::Fail;
use crate::{Body, BoxError};

/// Response headers an MCP answer adds besides its body: degraded components and missing servers.
#[derive(Clone, Debug, Default)]
pub(crate) struct Extra {
    pub(crate) headers: HeaderMap,
}

impl Extra {
    pub(crate) fn set(&mut self, name: &'static str, value: &str) {
        if let Ok(v) = HeaderValue::from_str(value) {
            self.headers.insert(HeaderName::from_static(name), v);
        }
    }

    pub(crate) fn merge_into(self, resp: &mut Response<Body>) {
        for (name, value) in self.headers {
            if let Some(name) = name {
                resp.headers_mut().insert(name, value);
            }
        }
    }
}

fn full(bytes: Bytes) -> Body {
    Full::new(bytes).map_err(|never| match never {}).boxed()
}

fn path(resp: &mut Response<Body>, value: &'static str) {
    resp.headers_mut().insert(
        HeaderName::from_static(h::RESPONSE_PATH),
        HeaderValue::from_static(value),
    );
}

/// A JSON answer relayed from or built for an upstream exchange.
pub(crate) fn json(status: u16, body: &Value, response_path: &'static str) -> Response<Body> {
    let bytes = Bytes::from(serde_json::to_vec(body).unwrap_or_default());
    let mut resp = Response::new(full(bytes));
    *resp.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    path(&mut resp, response_path);
    resp
}

/// `202 Accepted` with an empty body.
pub(crate) fn accepted() -> Response<Body> {
    empty(StatusCode::ACCEPTED)
}

pub(crate) fn empty(status: StatusCode) -> Response<Body> {
    let mut resp = Response::new(full(Bytes::new()));
    *resp.status_mut() = status;
    path(&mut resp, "upstream");
    resp
}

/// An SSE answer whose events come from `events` (each item is one complete event's bytes).
pub(crate) fn sse<S>(events: S) -> Response<Body>
where
    S: Stream<Item = Bytes> + Send + 'static,
{
    let frames =
        Exclusive(parking_lot::Mutex::new(Box::pin(events))).map(|b| Ok::<_, BoxError>(Frame::data(b)));
    let mut resp = Response::new(BodyExt::boxed(StreamBody::new(frames)));
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    resp.headers_mut()
        .insert(http::header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    path(&mut resp, "upstream");
    resp
}

/// One SSE event carrying one JSON-RPC message.
pub(crate) fn event(msg: &Value) -> Bytes {
    event_with_id(msg, None)
}

/// One SSE event carrying one JSON-RPC message, with an event id when there is one.
pub(crate) fn event_with_id(msg: &Value, id: Option<&str>) -> Bytes {
    let data = serde_json::to_string(msg).unwrap_or_default();
    match id {
        Some(id) => Bytes::from(format!("id: {id}\nevent: message\ndata: {data}\n\n")),
        None => Bytes::from(format!("event: message\ndata: {data}\n\n")),
    }
}

/// A JSON-RPC error answer, recorded for the request log.
pub(crate) fn error(fail: &Fail, env: &Env) -> Response<Body> {
    env.log.update(|f| {
        f.jsonrpc_error_code = Some(i64::from(fail.jsonrpc_code()));
        f.error = Some(fail.err.clone());
    });
    let (status, headers, body) = fail.render(&env.request_id);
    let bytes = Bytes::from(serde_json::to_vec(&body).unwrap_or_default());
    let mut resp = Response::new(full(bytes));
    *resp.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    *resp.headers_mut() = headers;
    resp
}

/// Makes a `Send` stream `Sync` by owning it exclusively: it is only ever polled through `&mut`.
struct Exclusive<S>(parking_lot::Mutex<std::pin::Pin<Box<S>>>);

impl<S: Stream> Stream for Exclusive<S> {
    type Item = S::Item;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<S::Item>> {
        self.get_mut().0.get_mut().as_mut().poll_next(cx)
    }
}
