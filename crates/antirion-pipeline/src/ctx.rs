//! The per-request context and the headers every data-plane response carries.

use std::net::IpAddr;
use std::sync::Arc;

use std::time::Instant;

use antirion_core::{Envelope, RequestId, UtcMicros, headers as h};
use antirion_error::GatewayError;
use antirion_runtime::{Runtime, Snapshot};
use bytes::Bytes;
use http::header::{
    ACCESS_CONTROL_ALLOW_CREDENTIALS, ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_EXPOSE_HEADERS, CONNECTION,
    STRICT_TRANSPORT_SECURITY, VARY,
};
use http::{HeaderName, HeaderValue, Response};
use http_body_util::BodyExt;
use http_body_util::combinators::BoxBody;
use tokio_util::sync::CancellationToken;

use crate::trace::TraceCx;

/// Errors a response body can end with.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
/// The body of every data-plane response.
pub type Body = BoxBody<Bytes, BoxError>;

pub(crate) fn full(bytes: Bytes) -> Body {
    http_body_util::Full::new(bytes)
        .map_err(|never| match never {})
        .boxed()
}

pub(crate) fn empty() -> Body {
    full(Bytes::new())
}

/// What every stage of one request shares.
pub(crate) struct Ctx {
    pub(crate) rt: Arc<Runtime>,
    pub(crate) snap: Arc<Snapshot>,
    pub(crate) request_id: RequestId,
    pub(crate) envelope: Envelope,
    pub(crate) origin: Option<String>,
    pub(crate) http1: bool,
    /// The request announced a body.
    pub(crate) has_body: bool,
    /// The body was read to its end.
    pub(crate) body_read: bool,
    /// The request-log record in progress, once the request passed `authenticate`.
    pub(crate) log: crate::reqlog::Draft,
    /// The request's reservation until it is settled, and the headers it produces.
    pub(crate) account: crate::account::Slot,
    /// The client IP: the TCP peer, or the address a trusted proxy reported. The single source
    /// for IP allow-lists, limits and the request log (`client_ip.rs`).
    pub(crate) client_ip: Option<IpAddr>,
    /// The request arrived on a TLS listener.
    pub(crate) secure: bool,
    /// Cancelled when the client goes away before the response head was handed over.
    pub(crate) gone: CancellationToken,
    /// `t_start`: the request head was read.
    pub(crate) started: Instant,
    pub(crate) started_at: UtcMicros,
    /// The log id the request's record gets, if one is written.
    pub(crate) log_id: String,
    pub(crate) trace: TraceCx,
    /// The `operation` and `dialect` metric labels.
    pub(crate) labels: (&'static str, &'static str),
    /// Guardrail verdicts and degraded components for the response headers.
    pub(crate) guard: crate::guard::Trail,
    /// Headers the response gets whatever it is (a deprecated Model's notices).
    pub(crate) extra_headers: Vec<(HeaderName, HeaderValue)>,
    /// The request came as `/antirion/decide/<path>`: it is answered with its routing decision.
    pub(crate) decide: bool,
    /// A Completions request served as Chat: the answer is turned back into text completions.
    pub(crate) completions_as_chat: bool,
}

impl Ctx {
    /// The error rendered in this path's envelope, with every common header.
    pub(crate) fn error(&self, err: &GatewayError) -> Response<Body> {
        self.error_for(err, None)
    }

    /// `error`, echoing the JSON-RPC id of the request it answers.
    pub(crate) fn error_for(&self, err: &GatewayError, rpc_id: Option<&serde_json::Value>) -> Response<Body> {
        self.log.record_error(err);
        let rendered = antirion_error::render(err, self.envelope, &self.request_id, rpc_id);
        let mut resp = Response::new(full(rendered.body));
        *resp.status_mut() = rendered.status;
        *resp.headers_mut() = rendered.headers;
        self.finish(&mut resp, err.close_connection);
        resp
    }

    /// Adds the headers every response carries: request id, test mode, security and CORS headers,
    /// and `Connection: close` when the connection cannot be reused.
    pub(crate) fn finish(&self, resp: &mut Response<Body>, close: bool) {
        let headers = resp.headers_mut();
        if let Ok(v) = HeaderValue::from_str(self.request_id.as_str()) {
            headers.insert(HeaderName::from_static(h::X_REQUEST_ID), v.clone());
            if self.envelope == Envelope::Anthropic {
                headers.insert(HeaderName::from_static(h::REQUEST_ID_ANTHROPIC), v);
            }
        }
        if self.snap.obs.version_header {
            headers.insert(
                HeaderName::from_static(h::VERSION),
                HeaderValue::from_static(h::BUILD_VERSION),
            );
        }
        if self.rt.test.is_some() {
            headers.insert(
                HeaderName::from_static(h::TEST_MODE),
                HeaderValue::from_static("true"),
            );
        }
        headers.insert(
            HeaderName::from_static(h::X_CONTENT_TYPE_OPTIONS),
            HeaderValue::from_static("nosniff"),
        );
        if self.secure
            && let Some(v) = self
                .snap
                .security
                .hsts
                .as_deref()
                .and_then(|v| HeaderValue::from_str(v).ok())
        {
            headers.insert(STRICT_TRANSPORT_SECURITY, v);
        }
        if let Some(origin) = &self.origin {
            headers.append(VARY, HeaderValue::from_static("Origin"));
            if let Some(cors) = self.snap.security.cors_data.as_ref().filter(|c| c.allows(origin)) {
                if let Ok(v) = HeaderValue::from_str(cors.allow_origin_value(origin)) {
                    headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, v);
                }
                if let Ok(v) = HeaderValue::from_str(&cors.expose_headers) {
                    headers.insert(ACCESS_CONTROL_EXPOSE_HEADERS, v);
                }
                if cors.allow_credentials {
                    headers.insert(ACCESS_CONTROL_ALLOW_CREDENTIALS, HeaderValue::from_static("true"));
                }
            }
        }
        if self.http1 && (close || (self.has_body && !self.body_read)) {
            headers.insert(CONNECTION, HeaderValue::from_static("close"));
        }
    }
}

type DynStream = std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, BoxError>> + Send>>;

/// A body over a stream of byte chunks. The mutex only makes the body `Sync`; it is never contended,
/// since polling takes `&mut self`.
struct StreamedBody {
    inner: std::sync::Mutex<DynStream>,
}

impl http_body::Body for StreamedBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, BoxError>>> {
        let stream = match self.get_mut().inner.get_mut() {
            Ok(s) => s,
            Err(poisoned) => poisoned.into_inner(),
        };
        stream
            .as_mut()
            .poll_next(cx)
            .map(|item| item.map(|r| r.map(http_body::Frame::data)))
    }
}

pub(crate) fn streamed(
    s: impl futures_util::Stream<Item = Result<Bytes, BoxError>> + Send + 'static,
) -> Body {
    StreamedBody {
        inner: std::sync::Mutex::new(Box::pin(s)),
    }
    .boxed()
}
