//! The request-log record of a request that passed `authenticate`: facts gathered by the stages while
//! the request runs. It is settled once the response head is built and, for a committed stream, the
//! stream has ended: whichever comes last writes the record, observes the metrics and ends the
//! trace.

mod emit;
mod json;
pub(crate) mod mcptel;
mod settle;
mod usage;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use antirion_core::{RequestId, Stage, UtcMicros};
use antirion_dialects::{Dialect, Endpoint};
use antirion_error::GatewayError;
use antirion_obs::settings::ClientIpMode;
use antirion_obs::trace::SpanId;
use antirion_runtime::Snapshot;
use http::request::Parts;
use serde_json::{Map, Value};

pub(crate) use self::settle::{CostFact, InFlight, finish, settle_stream, stream_overhead, tail_ended};
pub(crate) use self::usage::{Convention, Usage};
use crate::auth::Authenticated;
use crate::ctx::Ctx;
use crate::trace::TraceCx;

/// One upstream attempt, as the record, the metrics and the trace report it.
#[derive(Clone, Debug)]
pub(crate) struct AttemptFact {
    /// `primary`, `retry` or `fallback`.
    pub(crate) reason: &'static str,
    pub(crate) provider: String,
    pub(crate) provider_type: String,
    pub(crate) deployment: String,
    pub(crate) upstream_model: String,
    pub(crate) credential: Option<String>,
    /// The provider key's name, as the attempt trail shows it.
    pub(crate) key: Option<String>,
    pub(crate) started_at: UtcMicros,
    pub(crate) start: Instant,
    pub(crate) end: Instant,
    /// From the attempt's start to its first upstream content, for streams.
    pub(crate) ttft: Option<Duration>,
    pub(crate) status: Option<u16>,
    /// `success`, or the failure class.
    pub(crate) class: &'static str,
    pub(crate) retry_after_ms: Option<u64>,
    pub(crate) upstream_request_id: Option<String>,
    pub(crate) error_message: Option<String>,
    pub(crate) error_body: Option<String>,
    pub(crate) usage: Option<Usage>,
    /// The usage object a failed attempt's error body reported, as reported.
    pub(crate) usage_raw: Option<Value>,
    pub(crate) span: SpanId,
    /// `(host, port)` of the upstream.
    pub(crate) server: Option<(String, u16)>,
    /// The attempt loop's wait before the attempt.
    pub(crate) wait_ms: u64,
    /// The transition label of an attempt that moved to a new tree position.
    pub(crate) via: Option<String>,
    /// The timer that ended a timed-out attempt.
    pub(crate) timeout: Option<&'static str>,
    /// A lost hedge attempt whose exchange was still awaited: its ledger line is written when the
    /// exchange ends, not with the request's.
    pub(crate) tail: bool,
}

impl AttemptFact {
    /// The attempt's `outcome` label, as request logs record it.
    pub(crate) fn outcome(&self) -> &'static str {
        match (self.class, self.status) {
            ("success", _) => "success",
            ("client_cancelled", _) => "cancelled",
            ("timeout" | "connect_timeout", _) => "timeout",
            (_, Some(429)) => "upstream_429",
            (_, Some(s)) if (400..500).contains(&s) => "upstream_4xx",
            (_, Some(s)) if s >= 500 => "upstream_5xx",
            (_, Some(_)) => "stream_error",
            (_, None) => "connect_error",
        }
    }

    pub(crate) fn failed(&self) -> bool {
        self.class != "success"
    }
}

/// The `stream` member of a streamed request's record.
#[derive(Clone, Debug)]
pub(crate) struct StreamFact {
    pub(crate) end_reason: &'static str,
    pub(crate) committed: bool,
    pub(crate) relay_mode: &'static str,
}

/// The instants of a relayed stream.
#[derive(Clone, Debug, Default)]
pub(crate) struct StreamTimes {
    pub(crate) first_byte_sent: Option<Instant>,
    pub(crate) first_upstream_content: Option<Instant>,
    pub(crate) first_content_sent: Option<Instant>,
    pub(crate) last_content_sent: Option<Instant>,
    pub(crate) relay: Duration,
}

#[derive(Clone, Debug)]
pub(crate) struct ErrorFact {
    code: &'static str,
    reason: Option<&'static str>,
    stage: &'static str,
    message: String,
    /// The message came from upstream text.
    upstream: bool,
}

impl From<&GatewayError> for ErrorFact {
    fn from(e: &GatewayError) -> ErrorFact {
        ErrorFact {
            code: e.code.as_str(),
            reason: e.reason.map(|r| r.as_str()),
            stage: e.stage.as_str(),
            message: antirion_obs::truncate_utf8(e.message.as_str(), 2048).to_owned(),
            upstream: e.upstream.is_some(),
        }
    }
}

/// Whether and how content is recorded for this request.
#[derive(Clone, Debug, Default)]
pub(crate) struct ContentFact {
    /// Content logging applies (policy, header, and no zero data retention).
    pub(crate) enabled: bool,
    /// Zero data retention applies.
    pub(crate) zdr: bool,
    pub(crate) request: Option<Value>,
    pub(crate) response: Option<Value>,
    /// The request's messages and the response's output, for span attributes.
    pub(crate) input_messages: Option<String>,
    pub(crate) output_messages: Option<String>,
    /// How PII and secrets are removed from the stored copy, when they are.
    pub(crate) redact_pii: Option<PiiAction>,
}

/// `policies.observability.content.on_pii`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum PiiAction {
    Mask,
    Hash,
    DropContent,
}

impl ContentFact {
    /// Removes PII and secrets from the recorded content as `redact_pii` says; drops it all when
    /// the action is `drop_content` and anything matched.
    pub(crate) fn scrub(&mut self, hash_key: Option<&[u8]>) {
        use antirion_guardrails::def::Replacement;
        use antirion_guardrails::scrub::{holds_sensitive, scrub};
        let Some(action) = self.redact_pii.filter(|_| self.enabled) else {
            return;
        };
        let how = match action {
            PiiAction::Mask => Replacement::Label,
            PiiAction::Hash => Replacement::Hash,
            PiiAction::DropContent => {
                let found = self.request.as_ref().is_some_and(holds_sensitive)
                    || self.response.as_ref().is_some_and(holds_sensitive);
                if found {
                    self.enabled = false;
                    self.request = None;
                    self.response = None;
                    self.input_messages = None;
                    self.output_messages = None;
                }
                return;
            }
        };
        for v in [&mut self.request, &mut self.response].into_iter().flatten() {
            scrub(v, how, hash_key);
        }
        for t in [&mut self.input_messages, &mut self.output_messages]
            .into_iter()
            .flatten()
        {
            let mut v = Value::String(std::mem::take(t));
            scrub(&mut v, how, hash_key);
            *t = v.as_str().map(str::to_owned).unwrap_or_default();
        }
    }
}

/// The `cache` member.
#[derive(Clone, Debug, Default)]
pub(crate) struct CacheFact {
    pub(crate) status: Option<&'static str>,
    pub(crate) key: Option<String>,
    /// The similarity of a semantic hit.
    pub(crate) similarity: Option<f64>,
    /// The deployment that produced the entry a hit replayed.
    pub(crate) origin: Option<String>,
    /// Why a miss wrote no entry (`skipped_read_only`).
    pub(crate) write: Option<&'static str>,
    /// The prompt prefix provider prompt caching works on, when the request uses it.
    pub(crate) prefix: Option<String>,
}

/// The record's `operation`: the endpoint id, with every MCP and A2A endpoint under `mcp` and `a2a`.
pub(crate) fn operation(endpoint: &Endpoint) -> &'static str {
    match endpoint.family {
        "mcp" => "mcp",
        "a2a" => "a2a",
        _ => endpoint.id,
    }
}

/// The record's `dialect`: the client-facing protocol family of the endpoint.
pub(crate) fn dialect(endpoint: &Endpoint) -> &'static str {
    match endpoint.dialect {
        Dialect::AnthropicMessages => "anthropic",
        Dialect::Gemini => "gemini",
        Dialect::BedrockConverse | Dialect::BedrockInvoke => "bedrock",
        Dialect::Mcp if endpoint.family == "a2a" => "a2a",
        Dialect::Mcp => "mcp",
        _ => "openai",
    }
}

/// The route template of an endpoint: its path, with the path parameter named.
pub(crate) fn template(endpoint: &Endpoint, path: &str) -> String {
    let Some(param) = endpoint.path_param.as_deref().filter(|p| !p.is_empty()) else {
        return path.to_owned();
    };
    let name = match endpoint.dialect {
        Dialect::Gemini | Dialect::BedrockConverse | Dialect::BedrockInvoke => "{model}",
        Dialect::Mcp => "{name}",
        _ => "{id}",
    };
    if path.contains(param) {
        path.replacen(param, name, 1)
    } else {
        endpoint.id.to_owned()
    }
}

fn client_ip(ip: Option<std::net::IpAddr>, mode: ClientIpMode) -> Option<String> {
    let ip = ip?;
    match (mode, ip) {
        (ClientIpMode::None, _) => None,
        (ClientIpMode::Full, ip) => Some(ip.to_string()),
        (ClientIpMode::Truncated, std::net::IpAddr::V4(v4)) => {
            let [a, b, c, _] = v4.octets();
            Some(format!("{a}.{b}.{c}.0"))
        }
        (ClientIpMode::Truncated, std::net::IpAddr::V6(v6)) => {
            let s = v6.segments();
            let mut kept = [0u16; 8];
            for (k, v) in kept.iter_mut().zip(s.iter().take(3)) {
                *k = *v;
            }
            Some(std::net::Ipv6Addr::from(kept).to_string())
        }
    }
}

fn bounded_text(s: &str, max: usize) -> String {
    antirion_obs::truncate_utf8(s, max).to_owned()
}

/// The output part of a response, for the `gen_ai.output.messages` span attribute.
fn output_text(parsed: Option<&Value>, bytes: &[u8], max: usize) -> String {
    let part = parsed.and_then(|v| {
        v.get("choices")
            .or_else(|| v.get("content"))
            .or_else(|| v.get("output"))
            .or_else(|| v.get("candidates"))
    });
    match part {
        Some(p) => bounded_text(&p.to_string(), max),
        None => bounded_text(&String::from_utf8_lossy(bytes), max),
    }
}

/// A record in progress.
/// One INTERNAL span of a request: a stage that did work.
#[derive(Clone, Debug)]
pub(crate) struct InternalSpan {
    pub(crate) name: String,
    pub(crate) start: Instant,
    pub(crate) end: Instant,
    pub(crate) attrs: Vec<(&'static str, &'static str)>,
}

impl InternalSpan {
    pub(crate) fn new(name: String, start: Instant, end: Instant) -> InternalSpan {
        InternalSpan {
            name,
            start,
            end,
            attrs: Vec::new(),
        }
    }

    pub(crate) fn attr(mut self, key: &'static str, value: &'static str) -> InternalSpan {
        self.attrs.push((key, value));
        self
    }
}

/// Adds an INTERNAL span to the request's record.
pub(crate) fn span(cx: &Ctx, s: InternalSpan) {
    cx.log.update(|r| r.internal_spans.push(s));
}

#[derive(Clone, Debug)]
pub(crate) struct Record {
    pub(crate) id: String,
    pub(crate) snap: Arc<Snapshot>,
    pub(crate) request_id: RequestId,
    pub(crate) generation: u64,
    pub(crate) started_at: UtcMicros,
    pub(crate) started: Instant,
    pub(crate) method: String,
    pub(crate) endpoint: String,
    pub(crate) operation: &'static str,
    pub(crate) dialect: &'static str,
    pub(crate) client_ip: Option<String>,
    pub(crate) user_agent: Option<String>,
    pub(crate) client_retry_count: Option<u64>,
    /// Organization, team, project and key name; `None` for a request refused at `authenticate`.
    pub(crate) tenant: Option<[String; 4]>,
    /// The key's owner.
    pub(crate) owner: Option<String>,
    pub(crate) tags: Vec<String>,
    pub(crate) metadata: Map<String, Value>,
    pub(crate) metadata_dropped: u64,
    pub(crate) attempt_trail: bool,
    pub(crate) route_debug: bool,
    /// A record is written (request logs are enabled): the response names it with its log id.
    pub(crate) logged: bool,
    pub(crate) trace: TraceCx,
    /// Root span attributes taken from request headers.
    pub(crate) header_attributes: Vec<(String, String)>,
    pub(crate) request_headers: Map<String, Value>,
    pub(crate) response_headers: Map<String, Value>,
    pub(crate) end_user: Option<String>,
    pub(crate) session_id: Option<String>,
    pub(crate) requested_model: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) route: Option<String>,
    /// The route paths of the plan positions the request reached, in order.
    pub(crate) plan: Vec<String>,
    /// The route path of the final attempt.
    pub(crate) route_path: Option<String>,
    /// The conditional rules the served position was reached through: `{node, index, name}`.
    pub(crate) route_rules: Vec<Value>,
    pub(crate) show_route_path: bool,
    /// The routing-identity headers are hidden from the client.
    pub(crate) conceal_identity: bool,
    /// The `x-antirion-affinity` outcome.
    pub(crate) affinity: Option<&'static str>,
    /// Candidates passed over without an attempt, with the reason.
    pub(crate) skipped: Vec<(String, &'static str)>,
    /// The request reached stage `route`, so every response carries the attempt count.
    pub(crate) routed: bool,
    /// Classifier choices, for the routing debug header.
    pub(crate) classes: Vec<(String, String)>,
    /// The score of the first classifier's choice, when it has one.
    pub(crate) class_score: Option<f64>,
    /// The first cost-quality decision, as the routing debug header shows it.
    pub(crate) cost_quality: Option<Value>,
    /// The experiment assignment: experiment, arm, and whether the client sees it.
    pub(crate) experiment: Option<(String, String, bool)>,
    pub(crate) attempts: Vec<AttemptFact>,
    pub(crate) waits: Vec<(Instant, Instant)>,
    pub(crate) stream: Option<StreamFact>,
    pub(crate) stream_times: StreamTimes,
    pub(crate) usage: Usage,
    /// The usage object the upstream reported, as reported.
    pub(crate) usage_raw: Option<Value>,
    /// The non-token units the served call produced (images, audio seconds, characters, web
    /// searches), priced at settlement.
    pub(crate) units: antirion_limits::usage::Units,
    /// An image generation's `"<size>/<quality>"` and the images it asked for (`n`).
    pub(crate) images_requested: Option<(String, u64)>,
    /// The settled cost, once the request was settled.
    pub(crate) cost: Option<CostFact>,
    /// The response was replayed from the cache.
    pub(crate) cached: bool,
    pub(crate) cache: CacheFact,
    pub(crate) admission: Option<Value>,
    pub(crate) guardrails: Vec<Value>,
    /// What safety events need of `guardrails` entries beyond the record.
    pub(crate) safety: Vec<antirion_runtime::safety::Mark>,
    pub(crate) extensions: Vec<Value>,
    /// The INTERNAL spans of the stages that did work: admission, guardrails, extensions, the cache
    /// lookup.
    pub(crate) internal_spans: Vec<InternalSpan>,
    pub(crate) degraded: Vec<(&'static str, Option<&'static str>)>,
    pub(crate) client_disconnected: bool,
    pub(crate) upstream_request_id: Option<String>,
    pub(crate) response_model: Option<String>,
    pub(crate) response_id: Option<String>,
    pub(crate) finish_reasons: Vec<String>,
    /// The service tier the served response named, else the one the request asked for: the
    /// price's `service_tiers` multiplier applies to it.
    pub(crate) service_tier: Option<String>,
    /// Shadow copies to compare with this request's answer once it is settled.
    pub(crate) comparisons: Vec<std::sync::Arc<crate::compare::Pair>>,
    /// The finish reason and text of the answer, read for `comparisons`.
    pub(crate) answer: Option<(Option<String>, Option<String>)>,
    /// Tool calls in the response, once a response was read.
    pub(crate) tool_call_count: Option<u64>,
    /// The record this request replays (a playground replay).
    pub(crate) replay_of: Option<String>,
    /// When each lifecycle stage after `authenticate` began.
    pub(crate) stage_marks: Vec<(Stage, Instant)>,
    /// The request's query parameters, credentials removed.
    pub(crate) query: Option<Map<String, Value>>,
    pub(crate) content: ContentFact,
    /// `gen_ai.request.*` attributes of the attempt spans.
    pub(crate) request_params: Vec<(&'static str, antirion_obs::trace::Value)>,
    /// The `mcp` member: what an MCP gateway request did.
    pub(crate) mcp: Option<Value>,
    /// The spans and metrics an MCP or A2A request adds.
    pub(crate) mcp_telemetry: Option<mcptel::McpTelemetry>,
    /// The `a2a` member: what an A2A request did.
    pub(crate) a2a: Option<Value>,
    /// The `params` overrides and removals applied to the upstream body.
    pub(crate) params_applied: Option<antirion_providers::params::Applied>,
    pub(crate) error: Option<ErrorFact>,
    /// The scrubbed body of the latest upstream error, for the attempt that receives it.
    pub(crate) pending_error_body: Option<String>,
    /// The usage the latest upstream error body reported, for the attempt that receives it.
    pub(crate) pending_error_usage: Option<(Usage, Value)>,
    /// When `authenticate` ran, for its span.
    pub(crate) auth_span: Option<(Instant, Instant)>,
    /// The translation warning entries, request-side then response-side.
    pub(crate) translation_warnings: Vec<String>,
    /// `{budget, from, to}` when a budget downgraded the request.
    pub(crate) budget_downgrade: Option<Value>,
}

/// The header a playground replay carries; it counts only on a key the playground minted.
const REPLAY_OF: &str = "x-antirion-replay-of";

/// Query parameters that carry credentials and are never recorded.
const CREDENTIAL_PARAMS: &[&str] = &[
    "key",
    "api_key",
    "api-key",
    "access_token",
    "token",
    "x-goog-api-key",
];

/// The request's query parameters as recorded: credentials removed, `None` without a query.
fn query_of(query: Option<&str>) -> Option<Map<String, Value>> {
    let q = query.filter(|q| !q.is_empty())?;
    let mut out = Map::new();
    for pair in q.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if CREDENTIAL_PARAMS.iter().any(|c| c.eq_ignore_ascii_case(k)) {
            continue;
        }
        out.insert(k.to_owned(), Value::String(v.to_owned()));
    }
    Some(out)
}

/// Starts the record of a request refused at `authenticate` when `include_unauthenticated` asks for
/// one.
pub(crate) fn begin_unauthenticated(cx: &Ctx, parts: &Parts, endpoint: &Endpoint, auth_start: Instant) {
    if cx.snap.obs.request_logs.include_unauthenticated {
        cx.log.begin(Record::anonymous(
            cx,
            parts,
            endpoint,
            (auth_start, Instant::now()),
        ));
    }
}

impl Record {
    /// The index of the attempt whose answer the request had: the last one, a hedge cancelled
    /// because another attempt won excepted.
    pub(crate) fn serving_index(&self) -> Option<usize> {
        self.attempts.iter().rposition(|a| a.class != "hedge_cancelled")
    }

    /// The attempt of [`Record::serving_index`].
    pub(crate) fn serving_attempt(&self) -> Option<&AttemptFact> {
        self.serving_index().and_then(|i| self.attempts.get(i))
    }

    /// The record of an authenticated request.
    pub(crate) fn start(
        cx: &Ctx,
        parts: &Parts,
        endpoint: &Endpoint,
        auth: &Authenticated,
        auth_span: (Instant, Instant),
    ) -> Record {
        let key = &auth.key;
        let tags = cx
            .snap
            .identity
            .chain(key)
            .iter()
            .flatten()
            .flat_map(|t| t.tags.iter().cloned())
            .chain(key.tags.iter().cloned())
            .chain(auth.controls.tags.iter().cloned())
            .collect();
        let (metadata, metadata_dropped) =
            crate::meta::recorded(cx, auth.controls.metadata.as_ref(), &auth.key);
        Record {
            tenant: Some([
                key.organization.clone(),
                key.team.clone(),
                key.project.clone(),
                key.name.clone(),
            ]),
            owner: key.owner.clone(),
            tags,
            metadata,
            metadata_dropped,
            attempt_trail: auth.controls.attempt_trail,
            route_debug: auth.controls.route_debug,
            end_user: auth.end_user.clone(),
            session_id: auth.controls.session.clone(),
            show_route_path: key.permissions.has(antirion_identity::KeyPermission::RouteDebug)
                && cx.snap.routing.settings.expose_route_path,
            conceal_identity: false,
            replay_of: parts
                .headers
                .get(REPLAY_OF)
                .and_then(|v| v.to_str().ok())
                .filter(|_| cx.rt.is_playground_key(&key.name))
                .map(str::to_owned),
            ..Record::anonymous(cx, parts, endpoint, auth_span)
        }
    }

    /// The record of a request refused at `authenticate`, written only with
    /// `include_unauthenticated`: every identity field is null.
    pub(crate) fn anonymous(
        cx: &Ctx,
        parts: &Parts,
        endpoint: &Endpoint,
        auth_span: (Instant, Instant),
    ) -> Record {
        let obs = &cx.snap.obs;
        let header_text = |name: &str| -> Option<String> {
            let values: Vec<String> = parts
                .headers
                .get_all(name)
                .iter()
                .map(|v| antirion_obs::record::header_text(v.as_bytes()))
                .collect();
            (!values.is_empty()).then(|| values.join(", "))
        };
        let request_headers = obs
            .request_logs
            .request_headers
            .iter()
            .filter_map(|name| {
                let value = header_text(name)?;
                let shown = if antirion_egress::headers::always_denied(name)
                    || matches!(name.as_str(), "set-cookie")
                {
                    "[REDACTED]".to_owned()
                } else {
                    value
                };
                Some((name.clone(), Value::String(shown)))
            })
            .collect();
        let header_attributes = obs
            .traces
            .header_attributes
            .iter()
            .filter_map(|(h, attr)| header_text(h).map(|v| (attr.clone(), v)))
            .collect();
        let user_agent = obs
            .request_logs
            .user_agent
            .then(|| header_text(http::header::USER_AGENT.as_str()))
            .flatten()
            .map(|u| antirion_obs::truncate_utf8(&u, 256).to_owned());
        let client_retry_count = parts
            .headers
            .get("x-stainless-retry-count")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|n| *n <= 1000);
        Record {
            id: cx.log_id.clone(),
            snap: Arc::clone(&cx.snap),
            request_id: cx.request_id.clone(),
            generation: cx.snap.generation,
            started_at: cx.started_at,
            started: cx.started,
            method: parts.method.as_str().to_owned(),
            endpoint: template(endpoint, parts.uri.path()),
            operation: operation(endpoint),
            dialect: dialect(endpoint),
            client_ip: client_ip(cx.client_ip, obs.request_logs.client_ip),
            user_agent,
            client_retry_count,
            tenant: None,
            owner: None,
            tags: Vec::new(),
            metadata: Map::new(),
            metadata_dropped: 0,
            attempt_trail: false,
            route_debug: false,
            logged: obs.request_logs.enabled,
            trace: cx.trace.clone(),
            header_attributes,
            request_headers,
            response_headers: Map::new(),
            end_user: None,
            session_id: None,
            requested_model: None,
            model: None,
            route: None,
            plan: Vec::new(),
            route_path: None,
            route_rules: Vec::new(),
            show_route_path: false,
            conceal_identity: false,
            affinity: None,
            skipped: Vec::new(),
            routed: false,
            classes: Vec::new(),
            class_score: None,
            cost_quality: None,
            experiment: None,
            attempts: Vec::new(),
            waits: Vec::new(),
            stream: None,
            stream_times: StreamTimes::default(),
            usage: Usage::default(),
            usage_raw: None,
            units: antirion_limits::usage::Units::default(),
            images_requested: None,
            cost: None,
            cached: false,
            cache: CacheFact::default(),
            admission: None,
            guardrails: Vec::new(),
            safety: Vec::new(),
            extensions: Vec::new(),
            internal_spans: Vec::new(),
            degraded: Vec::new(),
            client_disconnected: false,
            upstream_request_id: None,
            response_model: None,
            response_id: None,
            finish_reasons: Vec::new(),
            service_tier: None,
            comparisons: Vec::new(),
            answer: None,
            tool_call_count: None,
            replay_of: None,
            stage_marks: Vec::new(),
            query: query_of(parts.uri.query()),
            content: ContentFact::default(),
            request_params: Vec::new(),
            mcp: None,
            mcp_telemetry: None,
            a2a: None,
            params_applied: None,
            error: None,
            pending_error_body: None,
            pending_error_usage: None,
            auth_span: Some(auth_span),
            translation_warnings: Vec::new(),
            budget_downgrade: None,
        }
    }

    pub(crate) fn fail(&mut self, e: &GatewayError) {
        self.error = Some(e.into());
    }
}

#[derive(Debug, Default)]
struct State {
    record: Option<Record>,
    /// A committed stream ends the request: the record is settled once both the head and the
    /// stream are done.
    streaming: bool,
    head_done: Option<HeadFacts>,
    stream_done: bool,
    /// Lost hedge attempts whose exchange is still awaited: the record waits for them.
    tails: u32,
    /// When the head was built, for a record settled after it.
    head_at: Option<Instant>,
    /// The code of the last error rendered, also for requests without a record.
    error_code: Option<(&'static str, Option<&'static str>)>,
    in_flight: Option<settle::InFlight>,
    /// The admission slot, held until the response is complete.
    slot: Option<antirion_runtime::admission::Permit>,
}

/// What the response head contributes to a record settled after it.
#[derive(Clone, Debug)]
pub(crate) struct HeadFacts {
    pub(crate) status: u16,
    pub(crate) response_path: String,
}

/// The record of one request, shared by its stages and, for a committed stream, the relay task.
#[derive(Clone, Debug, Default)]
pub(crate) struct Draft(Arc<Mutex<State>>);

impl Draft {
    pub(crate) fn begin(&self, record: Record) {
        if let Ok(mut s) = self.0.lock() {
            s.record = Some(record);
        }
    }

    /// Changes the record; a no-op before `authenticate`.
    pub(crate) fn update(&self, f: impl FnOnce(&mut Record)) {
        if let Ok(mut s) = self.0.lock()
            && let Some(r) = s.record.as_mut()
        {
            f(r);
        }
    }

    /// Notes that `stage` begins now.
    pub(crate) fn stage(&self, stage: Stage) {
        let now = Instant::now();
        self.update(|r| r.stage_marks.push((stage, now)));
    }

    /// Changes the record while the request is not settled; `false` once it is (or before
    /// `authenticate`).
    pub(crate) fn update_live(&self, f: impl FnOnce(&mut Record)) -> bool {
        match self.0.lock() {
            Ok(mut s) => match s.record.as_mut() {
                Some(r) => {
                    f(r);
                    true
                }
                None => false,
            },
            Err(_) => false,
        }
    }

    /// Reads the record.
    pub(crate) fn read<T>(&self, f: impl FnOnce(&Record) -> T) -> Option<T> {
        let s = self.0.lock().ok()?;
        s.record.as_ref().map(f)
    }

    pub(crate) fn record_error(&self, e: &GatewayError) {
        if let Ok(mut s) = self.0.lock() {
            s.error_code = Some((e.code.as_str(), e.reason.map(|r| r.as_str())));
            if let Some(r) = s.record.as_mut() {
                r.fail(e);
            }
        }
    }

    /// Counts the request in flight until it is settled.
    pub(crate) fn in_flight(&self, guard: settle::InFlight) {
        if let Ok(mut s) = self.0.lock() {
            s.in_flight = Some(guard);
        }
    }

    pub(crate) fn hold_slot(&self, permit: antirion_runtime::admission::Permit) {
        if let Ok(mut s) = self.0.lock() {
            s.slot = Some(permit);
        }
    }

    fn end_in_flight(&self) {
        if let Ok(mut s) = self.0.lock() {
            s.in_flight = None;
            s.slot = None;
        }
    }

    pub(crate) fn is_streaming(&self) -> bool {
        self.0.lock().is_ok_and(|s| s.streaming)
    }

    fn error_code(&self) -> Option<(&'static str, Option<&'static str>)> {
        self.0.lock().ok()?.error_code
    }

    /// A committed stream takes over the end of the request; the relay calls [`Draft::stream_ended`].
    pub(crate) fn hand_off(&self) -> Draft {
        if let Ok(mut s) = self.0.lock() {
            s.streaming = true;
        }
        self.clone()
    }

    /// The head is built: returns the record to settle now, unless a stream still runs or a lost
    /// hedge attempt is still awaited.
    fn head_built(&self, head: HeadFacts) -> Option<(Record, HeadFacts)> {
        let mut s = self.0.lock().ok()?;
        if (s.streaming && !s.stream_done) || s.tails > 0 {
            s.head_done = Some(head);
            s.head_at = Some(Instant::now());
            return None;
        }
        s.record.take().map(|r| (r, head))
    }

    /// The relay ended the stream: returns the record to settle now, if the head is built.
    pub(crate) fn stream_ended(&self) -> Option<(Record, HeadFacts)> {
        let mut s = self.0.lock().ok()?;
        s.stream_done = true;
        if s.tails > 0 {
            return None;
        }
        let head = s.head_done.clone()?;
        s.record.take().map(|r| (r, head))
    }

    /// A lost hedge attempt's exchange is awaited: the record waits for [`Draft::tail_ended`].
    pub(crate) fn begin_tail(&self) {
        if let Ok(mut s) = self.0.lock() {
            s.tails += 1;
        }
    }

    /// A tail ended: returns the record to settle now and when its head was built, when this was
    /// the last tail and nothing else holds the record.
    fn tail_ended(&self) -> Option<(Record, HeadFacts, Option<Instant>)> {
        let mut s = self.0.lock().ok()?;
        s.tails = s.tails.saturating_sub(1);
        if s.tails > 0 || (s.streaming && !s.stream_done) {
            return None;
        }
        let head = s.head_done.clone()?;
        let at = s.head_at;
        s.record.take().map(|r| (r, head, at))
    }

    /// Records the request body (`doc`, its parsed value) as content, when content logging applies.
    pub(crate) fn request_content(&self, bytes: &[u8], doc: Option<&Value>) {
        self.update(|r| {
            if !r.content.enabled {
                return;
            }
            let max = r.snap.obs.request_logs.max_body_bytes;
            let parsed = doc.filter(|_| bytes.len() <= max).cloned();
            r.content.input_messages = doc
                .and_then(|v| {
                    v.get("messages")
                        .or_else(|| v.get("input"))
                        .or_else(|| v.get("prompt"))
                })
                .map(|m| bounded_text(&m.to_string(), max));
            r.content.request = Some(antirion_obs::record::body(bytes, parsed, max));
        });
    }

    /// Records the response body the client received as content, when content logging applies.
    pub(crate) fn response_content(&self, bytes: &[u8]) {
        self.update(|r| {
            if !r.comparisons.is_empty() {
                r.answer = Some(crate::compare::answer(bytes));
            }
            if !r.content.enabled {
                return;
            }
            let max = r.snap.obs.request_logs.max_body_bytes;
            let parsed: Option<Value> = serde_json::from_slice(bytes).ok();
            r.content.output_messages = Some(output_text(parsed.as_ref(), bytes, max));
            r.content.response = Some(antirion_obs::record::body(bytes, parsed, max));
        });
    }

    /// Records an assembled response (a stream's) as content.
    pub(crate) fn response_value(&self, v: Value) {
        self.update(|r| {
            if !r.comparisons.is_empty() {
                r.answer = Some(crate::compare::answer_of(&v));
            }
            if !r.content.enabled {
                return;
            }
            let max = r.snap.obs.request_logs.max_body_bytes;
            let bytes = serde_json::to_vec(&v).unwrap_or_default();
            r.content.output_messages = Some(output_text(Some(&v), &bytes, max));
            r.content.response = Some(antirion_obs::record::body(&bytes, Some(v), max));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_spans_carry_their_attributes() {
        let now = Instant::now();
        let s = InternalSpan::new("antirion.guardrail g".into(), now, now)
            .attr("antirion.guardrail.stage", "input");
        assert_eq!(s.attrs, [("antirion.guardrail.stage", "input")]);
        assert!(
            InternalSpan::new("antirion.admit".into(), now, now)
                .attrs
                .is_empty()
        );
    }

    fn attempt(class: &'static str, status: Option<u16>) -> AttemptFact {
        let now = Instant::now();
        AttemptFact {
            reason: "primary",
            provider: "p".into(),
            provider_type: "openai".into(),
            deployment: "d".into(),
            upstream_model: "m".into(),
            credential: None,
            started_at: UtcMicros(0),
            start: now,
            end: now,
            ttft: None,
            status,
            class,
            retry_after_ms: None,
            upstream_request_id: None,
            error_message: None,
            error_body: None,
            usage: None,
            usage_raw: None,
            span: SpanId([1; 8]),
            server: None,
            key: None,
            wait_ms: 0,
            via: None,
            timeout: None,
            tail: false,
        }
    }

    #[test]
    fn outcomes() {
        assert_eq!(attempt("success", Some(200)).outcome(), "success");
        assert_eq!(attempt("rate_limited", Some(429)).outcome(), "upstream_429");
        assert_eq!(attempt("server_error", Some(500)).outcome(), "upstream_5xx");
        assert_eq!(attempt("caller_error", Some(400)).outcome(), "upstream_4xx");
        assert_eq!(attempt("connect_error", None).outcome(), "connect_error");
        assert_eq!(attempt("timeout", None).outcome(), "timeout");
        assert_eq!(attempt("upstream_reset", Some(200)).outcome(), "stream_error");
        assert_eq!(attempt("client_cancelled", None).outcome(), "cancelled");
    }

    #[test]
    fn truncated_client_ips() {
        let v4 = "10.1.2.3".parse().ok();
        assert_eq!(
            client_ip(v4, ClientIpMode::Truncated).as_deref(),
            Some("10.1.2.0")
        );
        assert_eq!(client_ip(v4, ClientIpMode::None), None);
        let v6 = "2001:db8:1:2:3:4:5:6".parse().ok();
        assert_eq!(
            client_ip(v6, ClientIpMode::Truncated).as_deref(),
            Some("2001:db8:1::")
        );
    }
}
