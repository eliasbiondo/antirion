//! The data plane: every request runs the lifecycle stages in order on its own tracked task, so a
//! client disconnect never cancels the work and a panic is contained to one 500 response.

#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unreachable
)]

/// Entry points of the fuzz targets (`fuzz/`) into parsers that are otherwise private.
#[cfg(fuzzing)]
pub mod fuzzing {
    /// Parses an `x-antirion-tags` value.
    #[must_use]
    pub fn tags(value: &str) -> Option<usize> {
        crate::controls::tag_items(value).map(|t| t.len())
    }

    /// Reads an upstream error body.
    #[must_use]
    pub fn upstream_error(status: u16, content_type: Option<&str>, body: &[u8]) -> Option<String> {
        crate::upstream_error::extract(status, content_type, body, &crate::scrub::Scrub::default()).message
    }
}

mod accept;
mod access;
mod account;
mod aggregate;
mod alongside;
mod apis;
mod asyncguard;
mod attempt;
mod auth;
mod authorize;
mod batches;
mod body;
mod byok;
mod cache;
mod caps;
mod cell;
mod certbind;
mod client_ip;
mod compare;
mod completions;
mod controls;
mod ctx;
mod decide;
mod decision;
mod downgrade;
mod egress;
mod embed;
mod exposure;
mod extend;
mod external;
mod failure;
mod fallbacks;
mod fanout;
mod feedback;
mod files;
mod guard;
mod guardsel;
mod guardsvc;
pub mod guardtest;
mod hedge_tail;
mod infer;
mod internal;
mod jwt;
mod local;
mod mcp;
mod mcpguard;
mod mcplimits;
mod media;
mod meta;
mod mirror;
mod mount_timers;
mod observe;
mod outcome;
mod parse;
mod passthrough;
mod picker;
mod policy;
mod postcommit;
mod provider_call;
mod realtime;
mod reasoning;
mod relay;
mod reqlog;
mod respstate;
mod route;
mod routing;
mod scrub;
mod select;
mod selfservice;
mod socket;
mod stored;
mod streamed;
mod templating;
mod toolloop;
mod trace;
mod upbody;
mod upload;
mod upstream_error;
mod xlate;

use std::sync::Arc;
use std::time::Instant;

use antirion_core::{Peer, RequestId, Stage, headers as h};
use antirion_dialects::{Kind, envelope_for_path};
use antirion_error::reason::hygiene;
use antirion_error::{GatewayError, Message};
use antirion_runtime::Runtime;
use http::header::{CONTENT_LENGTH, ORIGIN, TRANSFER_ENCODING};
use http::{Request, Response, Version};
use hyper::body::Incoming;
use tokio_util::sync::CancellationToken;

pub use ctx::{Body, BoxError};

/// Reasons registered for the routing and server rules this crate applies.
pub mod reasons {
    antirion_error::reasons! {
        PIN_NOT_IN_ROUTE = "pin_not_in_route" => InvalidRequest,
        BODY_READ_TIMEOUT = "body_read_timeout" => RequestTimeout,
        MISSING_REQUIRED_PARAMETER = "missing_required_parameter" => InvalidRequest,
        INVALID_TYPE = "invalid_type" => InvalidRequest,
        INVALID_VALUE = "invalid_value" => InvalidRequest,
        UNSUPPORTED_CONTENT_TYPE = "unsupported_content_type" => UnsupportedMediaType,
        EMPTY_STREAM = "empty_stream" => UpstreamBadResponse,
        UPSTREAM_TRUNCATED = "upstream_truncated" => UpstreamBadResponse,
        UPSTREAM_MALFORMED = "upstream_malformed" => UpstreamBadResponse,
        UPSTREAM_EVENT_TOO_LARGE = "upstream_event_too_large" => UpstreamBadResponse,
        UPSTREAM_IDLE_TIMEOUT = "upstream_idle_timeout" => UpstreamTimeout,
        TOTAL_TIMEOUT = "total_timeout" => UpstreamTimeout,
        CAP_STREAMING = "streaming" => UnsupportedCapability,
        CAP_TOOLS = "tools" => UnsupportedCapability,
        CAP_TOOL_CHOICE_FORCED = "tool_choice_forced" => UnsupportedCapability,
        CAP_JSON_MODE = "json_mode" => UnsupportedCapability,
        CAP_JSON_SCHEMA = "json_schema" => UnsupportedCapability,
        CAP_VISION = "vision" => UnsupportedCapability,
        CAP_AUDIO_INPUT = "audio_input" => UnsupportedCapability,
        CAP_PDF_INPUT = "pdf_input" => UnsupportedCapability,
        CAP_N_MULTIPLE = "n_multiple" => UnsupportedCapability,
        CAP_MAX_N = "max_n" => UnsupportedCapability,
        CAP_DIALECTS = "dialects" => UnsupportedCapability,
        CAP_LOGPROBS = "logprobs" => UnsupportedCapability,
        CAP_OPERATION = "operations" => UnsupportedCapability,
        CAP_PARALLEL_TOOL_CALLS = "parallel_tool_calls" => UnsupportedCapability,
        CAP_REASONING = "reasoning" => UnsupportedCapability,
        CAP_MAX_INPUT_TOKENS = "max_input_tokens" => UnsupportedCapability,
        CAP_MAX_OUTPUT_TOKENS = "max_output_tokens" => UnsupportedCapability,
        CAP_CONTEXT_WINDOW = "context_window" => UnsupportedCapability,
        ROUTER_UNAVAILABLE = "router_unavailable" => DependencyUnavailable,
        TAG_FILTER_EXCLUDED = "tag_filter_excluded" => Forbidden,
        DENY_PATTERN = "deny_pattern" => GuardrailBlocked,
        ALLOW_PATTERN = "allow_pattern" => GuardrailBlocked,
        KEYWORD = "keyword" => GuardrailBlocked,
        PII = "pii" => GuardrailBlocked,
        SECRET = "secret" => GuardrailBlocked,
        HATE = "hate" => GuardrailBlocked,
        HARASSMENT = "harassment" => GuardrailBlocked,
        VIOLENCE = "violence" => GuardrailBlocked,
        SEXUAL = "sexual" => GuardrailBlocked,
        SELF_HARM = "self_harm" => GuardrailBlocked,
        ILLICIT = "illicit" => GuardrailBlocked,
        PROMPT_INJECTION = "prompt_injection" => GuardrailBlocked,
        OFF_TOPIC = "off_topic" => GuardrailBlocked,
        SCHEMA_VIOLATION = "schema_violation" => GuardrailBlocked,
        TOOL_NOT_ALLOWED = "tool_not_allowed" => GuardrailBlocked,
        LENGTH = "length" => GuardrailBlocked,
        UNINSPECTABLE = "uninspectable" => GuardrailBlocked,
        CUSTOM = "custom" => GuardrailBlocked,
        GUARDRAIL_UNAVAILABLE = "guardrail_unavailable" => DependencyUnavailable,
        GUARDRAIL_STREAM_REJECTED = "guardrail_stream_rejected" => InvalidRequest,
        EXTENSION_UNAVAILABLE = "extension_unavailable" => DependencyUnavailable,
        EXTENSION_REJECTED = "extension_rejected" => InvalidRequest,
        EXTENSION_DENIED = "extension_denied" => Forbidden,
        CREDENTIAL_ACQUISITION_FAILED = "credential_acquisition_failed" => UpstreamAuthFailed,
        STREAM_FORMAT_UNSUPPORTED = "stream_format_unsupported" => InvalidRequest,
        URL_NOT_ALLOWED = "url_not_allowed" => InvalidRequest,
        MEDIA_FETCH_FAILED = "media_fetch_failed" => InvalidRequest,
        MEDIA_TOO_LARGE = "media_too_large" => InvalidRequest,
        MEDIA_TYPE_NOT_ALLOWED = "media_type_not_allowed" => InvalidRequest,
        MEDIA_REDIRECT = "media_redirect" => InvalidRequest,
        UNTRANSLATABLE_REQUEST = "untranslatable_request" => InvalidRequest,
        MEDIA_INVALID = "media_invalid" => InvalidRequest,
        MEDIA_TOO_MANY = "media_too_many" => InvalidRequest,
        TRANSLATION_STRICT = "translation_strict" => UnsupportedCapability,
        MALFORMED_FUNCTION_CALL = "malformed_function_call" => UpstreamBadResponse,
        UNSUPPORTED_CHARSET = "unsupported_charset" => UnsupportedMediaType,
        TOO_MANY_MESSAGES = "too_many_messages" => InvalidRequest,
        PREVIOUS_RESPONSE_NOT_FOUND = "previous_response_not_found" => NotFound,
        RESPONSE_NOT_FOUND = "response_not_found" => NotFound,
        RESPONSE_CHAIN_TOO_DEEP = "response_chain_too_deep" => InvalidRequest,
        AFFINITY_UNAVAILABLE = "affinity_unavailable" => NoHealthyDeployment,
        STORE_UNAVAILABLE = "store_unavailable" => DependencyUnavailable,
        LEDGER_UNAVAILABLE = "ledger_unavailable" => DependencyUnavailable,
        CONTEXT_LENGTH_EXCEEDED = "context_length_exceeded" => UpstreamRejected,
        CONTENT_POLICY_VIOLATION = "content_policy_violation" => UpstreamRejected,
        RESPONSE_TOO_LARGE = "response_too_large" => UpstreamBadResponse,
        STREAMING_DISABLED = "streaming_disabled" => InvalidRequest,
        INVALID_COMPRESSED_BODY = "invalid_compressed_body" => InvalidRequest,
        BYOK_CREDENTIAL_REFUSED = "byok_credential_refused" => UpstreamRejected,
        PROVIDER_KEY_REQUIRED = "provider_key_required" => Unauthenticated,
        TOOL_RESULT_UNPAIRED = "tool_result_unpaired" => InvalidRequest,
        FILES_PROVIDER_REQUIRED = "files_provider_required" => InvalidRequest,
        BATCH_MODEL_MISMATCH = "batch_model_mismatch" => InvalidRequest,
        FILE_NOT_FOUND = "file_not_found" => NotFound,
        BATCH_NOT_FOUND = "batch_not_found" => NotFound,
        UPGRADE_REQUIRED = "upgrade_required" => InvalidRequest,
        MODEL_RETIRED = "model_retired" => ModelNotFound,
        UNKNOWN_TEMPLATE = "unknown_template" => InvalidRequest,
        TEMPLATE_VARIABLE_MISSING = "template_variable_missing" => InvalidRequest,
        TEMPLATE_CONFLICT = "template_conflict" => InvalidRequest,
        TEMPLATE_REQUIRED = "template_required" => InvalidRequest,
    }
}

/// The data-plane service.
#[derive(Clone, Debug)]
pub struct DataPlane {
    rt: Arc<Runtime>,
}

impl DataPlane {
    #[must_use]
    pub fn new(rt: Arc<Runtime>) -> DataPlane {
        DataPlane { rt }
    }

    /// Serves one request.
    pub async fn serve(&self, req: Request<Incoming>) -> Response<Body> {
        self.serve_on(req, None).await
    }

    /// Serves one request, running its handler on `on` when given, on the calling runtime
    /// otherwise.
    pub async fn serve_on(
        &self,
        req: Request<Incoming>,
        on: Option<&tokio::runtime::Handle>,
    ) -> Response<Body> {
        let request_id = RequestId::from_client(req.headers().get(h::X_REQUEST_ID).map(|v| v.as_bytes()));
        let raw_path = req.uri().path().to_owned();
        let http1 = req.version() <= Version::HTTP_11;
        let secure = req.extensions().get::<Peer>().is_some_and(|p| p.tls);
        let rt = Arc::clone(&self.rt);
        // The drain deadline ends a request through the same path as a client that went away.
        let mut gone = CancelOnDrop {
            token: self.rt.drain.cut.child_token(),
            armed: true,
        };
        let handler = handle(rt, req, request_id.clone(), gone.token.clone());
        let task = match on {
            Some(on) => self.rt.tasks.spawn_on(handler, on),
            None => self.rt.tasks.spawn(handler),
        };
        let joined = task.await;
        gone.armed = false;
        let mut resp = match joined {
            Ok(resp) => resp,
            Err(_) => {
                let snap = self.rt.gens.load();
                let cx = ctx::Ctx {
                    rt: Arc::clone(&self.rt),
                    trace: trace::TraceCx::new(&snap.obs.traces, &http::HeaderMap::new(), Instant::now()),
                    snap,
                    request_id,
                    started: Instant::now(),
                    started_at: self.rt.now(),
                    log_id: String::new(),
                    labels: ("_unknown", "openai"),
                    envelope: envelope_for_path(&raw_path),
                    origin: None,
                    http1,
                    has_body: false,
                    body_read: true,
                    log: reqlog::Draft::default(),
                    account: account::Slot::default(),
                    client_ip: None,
                    secure,
                    gone: CancellationToken::new(),
                    guard: guard::Trail::default(),
                    extra_headers: Vec::new(),
                    decide: false,
                    completions_as_chat: false,
                };
                tracing::error!(path = %raw_path, "a request handler panicked");
                let e = GatewayError::with_reason(
                    hygiene::PANIC,
                    Stage::Respond,
                    Message::Fixed("Internal error."),
                );
                cx.error(&e)
            }
        };
        if !matches!(raw_path.as_str(), "/-/livez" | "/-/readyz") {
            mark_degraded(&self.rt.degraded(), resp.headers_mut());
        }
        resp
    }
}

/// Adds the node's degraded dependencies to `x-antirion-degraded`, a set.
pub(crate) fn mark_degraded(components: &[&str], headers: &mut http::HeaderMap) {
    if components.is_empty() {
        return;
    }
    let mut names: Vec<String> = headers
        .get(h::DEGRADED)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    for c in components {
        if !names.iter().any(|n| n == c) {
            names.push((*c).to_owned());
        }
    }
    if let Ok(v) = http::HeaderValue::from_str(&names.join(", ")) {
        headers.insert(http::HeaderName::from_static(h::DEGRADED), v);
    }
}

fn has_body(parts: &http::request::Parts) -> bool {
    let declared = parts
        .headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    declared.is_some_and(|n| n > 0) || parts.headers.contains_key(TRANSFER_ENCODING)
}

/// Cancels its token when dropped: the client went away while the response was being prepared.
struct CancelOnDrop {
    token: CancellationToken,
    armed: bool,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.token.cancel();
        }
    }
}

async fn handle(
    rt: Arc<Runtime>,
    req: Request<Incoming>,
    request_id: RequestId,
    gone: CancellationToken,
) -> Response<Body> {
    let started = Instant::now();
    let (mut parts, incoming) = req.into_parts();
    let decide = decision::strip(&mut parts.uri);
    let snap = rt.gens.load();
    let peer = parts.extensions.get::<Peer>().cloned();
    let client_ip = client_ip::client_ip(&snap.security, &parts);
    let mut cx = ctx::Ctx {
        trace: trace::TraceCx::new(&snap.obs.traces, &parts.headers, started),
        snap,
        started,
        started_at: rt.now(),
        log_id: RequestId::generate().as_str().to_owned(),
        labels: ("_unknown", "openai"),
        rt: Arc::clone(&rt),
        request_id,
        envelope: envelope_for_path(parts.uri.path()),
        origin: parts
            .headers
            .get(ORIGIN)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        http1: parts.version <= Version::HTTP_11,
        has_body: has_body(&parts),
        body_read: false,
        log: reqlog::Draft::default(),
        account: account::Slot::default(),
        client_ip,
        secure: peer.as_ref().is_some_and(|p| p.tls),
        gone,
        guard: guard::Trail::default(),
        extra_headers: Vec::new(),
        decide,
        completions_as_chat: false,
    };
    fault::maybe_panic(&cx, &parts, Stage::Accept);
    let probe = matches!(parts.uri.path(), "/-/livez" | "/-/readyz");
    let endpoint = match accept::run(&cx, &parts) {
        accept::Accepted::Respond(r) if probe => return r,
        accept::Accepted::Respond(r) => return reqlog::finish(&cx, r),
        accept::Accepted::Endpoint(endpoint) => endpoint,
    };
    cx.labels = (reqlog::operation(&endpoint), reqlog::dialect(&endpoint));
    let client_dialect = apis::client_dialect_id(&endpoint);
    let resp = Box::pin(serve_endpoint(&mut cx, &parts, incoming, endpoint, &rt)).await;
    with_client_dialect(resp, client_dialect)
}

/// `x-antirion-client-dialect` on a response, unless it already names one.
fn with_client_dialect(mut resp: Response<Body>, dialect: Option<&'static str>) -> Response<Body> {
    if let Some(d) = dialect {
        resp.headers_mut()
            .entry(http::HeaderName::from_static("x-antirion-client-dialect"))
            .or_insert(http::HeaderValue::from_static(d));
    }
    resp
}

async fn serve_endpoint(
    cx: &mut ctx::Ctx,
    parts: &http::request::Parts,
    incoming: Incoming,
    endpoint: antirion_dialects::Endpoint,
    rt: &Arc<Runtime>,
) -> Response<Body> {
    cx.log.in_flight(reqlog::InFlight::new(&rt.metrics, cx.labels.0));
    let auth_start = Instant::now();
    auth::catch_up_for_unknown_key(cx, parts, &endpoint).await;
    match endpoint {
        endpoint if matches!(endpoint.kind, Kind::Mcp | Kind::A2a | Kind::McpMetadata) => {
            mcp::serve(cx, parts, incoming, &endpoint).await
        }
        endpoint if endpoint.kind == Kind::ResponsesSocket => socket::serve(cx, parts, &endpoint).await,
        endpoint => match auth::run(cx, parts, &endpoint).await.map(|a| (a, Instant::now())) {
            Err(e) => {
                reqlog::begin_unauthenticated(cx, parts, &endpoint, auth_start);
                reqlog::finish(cx, cx.error(&e))
            }
            Ok((authed, authenticated)) => {
                fault::maybe_panic(cx, parts, Stage::Authenticate);
                let snap = Arc::clone(&cx.snap);
                trace::scope(cx, &policy::Levels::new(&snap, &authed.key, "", None));
                let auth_span = (auth_start, authenticated);
                cx.log
                    .begin(reqlog::Record::start(cx, parts, &endpoint, &authed, auth_span));
                let mut resp = match endpoint.kind {
                    _ if cx.decide && endpoint.kind != Kind::Inference => {
                        cx.error(&accept::unknown_endpoint(&parts.method, parts.uri.path()))
                    }
                    Kind::ModelsList => local::models(cx, parts, &endpoint, &authed.key),
                    Kind::StoredResponse => respstate::serve_stored(cx, parts, &endpoint, &authed.key).await,
                    Kind::Inference => infer::run(cx, parts, incoming, &endpoint, &authed).await,
                    Kind::Passthrough => passthrough::run(cx, parts, incoming, &endpoint, &authed).await,
                    Kind::Limits => selfservice::limits(cx, &authed.key, authed.end_user.as_deref()).await,
                    Kind::Files => files::serve(cx, parts, incoming, &endpoint, &authed).await,
                    Kind::Batches => batches::serve(cx, parts, incoming, &endpoint, &authed).await,
                    Kind::Realtime => realtime::serve(cx, parts, &endpoint, &authed).await,
                    Kind::ResponsesSocket => socket::serve(cx, parts, &endpoint).await,
                    Kind::Feedback => feedback::serve(cx, parts, incoming, &authed.key).await,
                    Kind::Mcp | Kind::A2a | Kind::McpMetadata => {
                        mcp::serve(cx, parts, incoming, &endpoint).await
                    }
                };
                exposure::conceal(cx, &authed.key, &mut resp);
                reqlog::finish(cx, resp)
            }
        },
    }
}

/// Fault injection for containment tests, compiled only into `test-hooks` builds and honoured only
/// in test mode.
mod fault {
    use antirion_core::Stage;
    use http::request::Parts;

    use crate::ctx::Ctx;

    #[cfg(feature = "test-hooks")]
    #[allow(clippy::panic)]
    pub(crate) fn maybe_panic(cx: &Ctx, parts: &Parts, stage: Stage) {
        if cx.rt.test.is_none() {
            return;
        }
        let wanted = parts
            .headers
            .get("x-antirion-test-panic")
            .and_then(|v| v.to_str().ok());
        if wanted == Some(stage.as_str()) {
            panic!("injected panic at {}", stage.as_str());
        }
    }

    #[cfg(not(feature = "test-hooks"))]
    pub(crate) fn maybe_panic(_cx: &Ctx, _parts: &Parts, _stage: Stage) {}
}
