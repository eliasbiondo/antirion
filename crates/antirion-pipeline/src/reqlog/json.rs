//! The record as JSON (schema version 1): every field present, `null` where it does not apply. A
//! settled record is serialised straight from its facts, by each sink as it sends it, and built as a
//! value only for the node's own log store.

use antirion_core::UtcMicros;
use antirion_limits::money::Nusd;
use antirion_obs::sink::LogRecord;
use antirion_obs::timing::{Timing, ms3};
use serde::Serialize;
use serde_json::{Map, Value};

use super::settle::CostFact;
use super::usage::UsageJson;
use super::{AttemptFact, HeadFacts, Record};

/// A settled request with everything its record reports.
#[derive(Debug)]
pub(crate) struct Settled {
    pub(crate) rec: Record,
    pub(crate) head: HeadFacts,
    pub(crate) timing: Timing,
    pub(crate) cost: CostFact,
    pub(crate) ended: UtcMicros,
    pub(crate) node_id: String,
    pub(crate) gateway_version: String,
    /// Milliseconds per lifecycle stage.
    pub(crate) stage_ms: Map<String, Value>,
    /// `{country, region, city}` of the client IP, when GeoIP is configured.
    pub(crate) geo: Option<Value>,
}

impl Settled {
    /// The record as a value, content included.
    pub(crate) fn to_value(&self) -> Value {
        serde_json::to_value(self.json(true)).unwrap_or_default()
    }

    fn json(&self, include_content: bool) -> RecordJson<'_> {
        let rec = &self.rec;
        let last = rec.serving_attempt();
        let error = rec.error.as_ref();
        let [organization, team, project, key] = rec
            .tenant
            .as_ref()
            .map(|[o, t, p, k]| {
                [
                    Some(o.as_str()),
                    Some(t.as_str()),
                    Some(p.as_str()),
                    Some(k.as_str()),
                ]
            })
            .unwrap_or_default();
        let source = if rec.cached {
            "cache"
        } else if rec.usage.is_empty() {
            "none"
        } else if rec.usage.estimated {
            "estimated"
        } else {
            "upstream"
        };
        let content = (include_content && rec.content.enabled).then_some(ContentJson {
            request: rec.content.request.as_ref(),
            response: rec.content.response.as_ref(),
        });
        RecordJson {
            schema_version: antirion_obs::record::SCHEMA_VERSION,
            id: &rec.id,
            request_id: rec.request_id.as_str(),
            trace_id: rec.trace.trace_id.hex(),
            span_id: rec.trace.root.hex(),
            node_id: &self.node_id,
            gateway_version: &self.gateway_version,
            config_generation: rec.generation,
            started_at: rec.started_at.to_rfc3339_micros(),
            ended_at: self.ended.to_rfc3339_micros(),
            method: &rec.method,
            endpoint: &rec.endpoint,
            operation: rec.operation,
            dialect: rec.dialect,
            stream: rec.stream.as_ref().map(|s| StreamJson {
                end_reason: s.end_reason,
                committed: s.committed,
                relay_mode: s.relay_mode,
            }),
            client_ip: rec.client_ip.as_deref(),
            user_agent: rec.user_agent.as_deref(),
            client_retry_count: rec.client_retry_count,
            organization,
            team,
            project,
            key,
            end_user: rec.end_user.as_deref(),
            tags: &rec.tags,
            metadata: &rec.metadata,
            metadata_dropped: rec.metadata_dropped,
            session_id: rec.session_id.as_deref(),
            requested_model: rec.requested_model.as_deref(),
            model: rec.model.as_deref(),
            route: rec.route.as_deref(),
            provider: last.map(|a| a.provider.as_str()),
            deployment: last.map(|a| a.deployment.as_str()),
            upstream_model: last.map(|a| a.upstream_model.as_str()),
            response_model: rec.response_model.as_deref(),
            upstream_request_id: rec.upstream_request_id.as_deref(),
            response_path: &self.head.response_path,
            status_code: self.head.status,
            code: error.map(|e| e.code),
            reason: error.and_then(|e| e.reason),
            stage: error.map(|e| e.stage),
            error_message: error
                .filter(|e| !(rec.content.zdr && e.upstream))
                .map(|e| e.message.as_str()),
            finish_reasons: &rec.finish_reasons,
            stop_category: None,
            client_disconnected: rec.client_disconnected,
            attempt_count: rec.attempts.len(),
            attempts: Attempts { rec, include_content },
            skipped_candidates: Skipped(&rec.skipped),
            translation_warnings: &rec.translation_warnings,
            params_applied: ParamsJson {
                overrides: rec
                    .params_applied
                    .as_ref()
                    .map_or(&[], |p| p.overrides.as_slice()),
                removed: rec.params_applied.as_ref().map_or(&[], |p| p.removed.as_slice()),
            },
            cache: CacheJson {
                status: rec.cache.status,
                layer: rec.cache.status.filter(|s| *s == "hit").map(|_| {
                    if rec.cache.similarity.is_some() {
                        "semantic"
                    } else {
                        "exact"
                    }
                }),
                similarity: rec.cache.similarity.map(|s| (s * 10_000.0).round() / 10_000.0),
                key: rec.cache.key.as_deref(),
                write: rec.cache.write.filter(|_| rec.cache.status != Some("hit")),
                prefix: rec.cache.prefix.as_deref(),
            },
            admission: rec.admission.as_ref(),
            degraded: Degraded(&rec.degraded),
            guardrails: &rec.guardrails,
            extensions: &rec.extensions,
            usage: rec.usage.json(Some(source)),
            cost: cost_json(&self.cost),
            timing: timing_json(&self.timing, &self.stage_ms),
            request_headers: &rec.request_headers,
            response_headers: &rec.response_headers,
            content,
            mcp: rec.mcp.as_ref(),
            a2a: rec.a2a.as_ref(),
            tool_call_count: rec.tool_call_count,
            query: rec.query.as_ref(),
            client_geo: self.geo.as_ref(),
            replay_of: rec.replay_of.as_deref(),
            experiment: rec
                .experiment
                .as_ref()
                .map(|(name, arm, _)| ExperimentJson { name, arm }),
            route_path: rec.route_path.as_deref(),
            route_rules: &rec.route_rules,
            budget_downgrade: rec.budget_downgrade.as_ref(),
        }
    }
}

impl LogRecord for Settled {
    fn write_json(&self, include_content: bool, out: &mut Vec<u8>) {
        let _ = serde_json::to_writer(out, &self.json(include_content));
    }
}

#[derive(Serialize)]
struct RecordJson<'a> {
    schema_version: u64,
    id: &'a str,
    request_id: &'a str,
    trace_id: String,
    span_id: String,
    node_id: &'a str,
    gateway_version: &'a str,
    config_generation: u64,
    started_at: String,
    ended_at: String,
    method: &'a str,
    endpoint: &'a str,
    operation: &'a str,
    dialect: &'a str,
    stream: Option<StreamJson>,
    client_ip: Option<&'a str>,
    user_agent: Option<&'a str>,
    client_retry_count: Option<u64>,
    organization: Option<&'a str>,
    team: Option<&'a str>,
    project: Option<&'a str>,
    key: Option<&'a str>,
    end_user: Option<&'a str>,
    tags: &'a [String],
    metadata: &'a Map<String, Value>,
    metadata_dropped: u64,
    session_id: Option<&'a str>,
    requested_model: Option<&'a str>,
    model: Option<&'a str>,
    route: Option<&'a str>,
    provider: Option<&'a str>,
    deployment: Option<&'a str>,
    upstream_model: Option<&'a str>,
    response_model: Option<&'a str>,
    upstream_request_id: Option<&'a str>,
    response_path: &'a str,
    status_code: u16,
    code: Option<&'a str>,
    reason: Option<&'a str>,
    stage: Option<&'a str>,
    error_message: Option<&'a str>,
    finish_reasons: &'a [String],
    stop_category: Option<()>,
    client_disconnected: bool,
    attempt_count: usize,
    attempts: Attempts<'a>,
    skipped_candidates: Skipped<'a>,
    translation_warnings: &'a [String],
    params_applied: ParamsJson<'a>,
    cache: CacheJson<'a>,
    admission: Option<&'a Value>,
    degraded: Degraded<'a>,
    guardrails: &'a [Value],
    extensions: &'a [Value],
    usage: UsageJson,
    cost: CostJson,
    timing: TimingJson<'a>,
    request_headers: &'a Map<String, Value>,
    response_headers: &'a Map<String, Value>,
    content: Option<ContentJson<'a>>,
    mcp: Option<&'a Value>,
    a2a: Option<&'a Value>,
    tool_call_count: Option<u64>,
    query: Option<&'a Map<String, Value>>,
    client_geo: Option<&'a Value>,
    replay_of: Option<&'a str>,
    experiment: Option<ExperimentJson<'a>>,
    route_path: Option<&'a str>,
    route_rules: &'a [Value],
    budget_downgrade: Option<&'a Value>,
}

#[derive(Serialize)]
struct ExperimentJson<'a> {
    name: &'a str,
    arm: &'a str,
}

#[derive(Serialize)]
struct StreamJson {
    end_reason: &'static str,
    committed: bool,
    relay_mode: &'static str,
}

#[derive(Serialize)]
struct ContentJson<'a> {
    request: Option<&'a Value>,
    response: Option<&'a Value>,
}

#[derive(Serialize)]
struct ParamsJson<'a> {
    overrides: &'a [String],
    removed: &'a [String],
}

#[derive(Serialize)]
struct CacheJson<'a> {
    status: Option<&'static str>,
    layer: Option<&'static str>,
    similarity: Option<f64>,
    key: Option<&'a str>,
    write: Option<&'static str>,
    prefix: Option<&'a str>,
}

#[derive(Serialize)]
struct TimingJson<'a> {
    total_ms: f64,
    overhead_ms: f64,
    upstream_ms: f64,
    wait_ms: f64,
    external_ms: f64,
    ttft_ms: Option<f64>,
    upstream_ttft_ms: Option<f64>,
    tpot_ms: Option<f64>,
    ttfb_overhead_ms: Option<f64>,
    relay_ms: Option<f64>,
    stage_ms: Option<&'a Map<String, Value>>,
}

fn timing_json<'a>(t: &Timing, stages: &'a Map<String, Value>) -> TimingJson<'a> {
    TimingJson {
        total_ms: t.total_ms,
        overhead_ms: t.overhead_ms,
        upstream_ms: t.upstream_ms,
        wait_ms: t.wait_ms,
        external_ms: t.external_ms,
        ttft_ms: t.ttft_ms,
        upstream_ttft_ms: t.upstream_ttft_ms,
        tpot_ms: t.tpot_ms,
        ttfb_overhead_ms: t.ttfb_overhead_ms,
        relay_ms: t.relay_ms,
        stage_ms: (!stages.is_empty()).then_some(stages),
    }
}

#[derive(Serialize)]
struct CostJson {
    currency: &'static str,
    total: Option<String>,
    input: Option<String>,
    cache_read: Option<String>,
    cache_write: Option<String>,
    output: Option<String>,
    reasoning: Option<String>,
    audio: Option<String>,
    image: Option<String>,
    video: Option<String>,
    unit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    saved: Option<String>,
    status: &'static str,
}

fn cost_json(c: &CostFact) -> CostJson {
    let amount = |n: Nusd| c.total.map(|_| n.to_usd_string());
    let k = &c.components;
    CostJson {
        currency: "USD",
        total: c.total.map(Nusd::to_usd_string),
        input: amount(k.input),
        cache_read: amount(k.cache_read),
        cache_write: amount(k.cache_write),
        output: amount(k.output),
        reasoning: amount(k.reasoning),
        audio: amount(k.audio),
        image: amount(k.image),
        video: amount(k.video),
        unit: amount(k.unit),
        saved: c.saved.map(Nusd::to_usd_string),
        status: c.status,
    }
}

/// The attempts of a record; their error bodies only with content.
struct Attempts<'a> {
    rec: &'a Record,
    include_content: bool,
}

impl Serialize for Attempts<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(
            self.rec
                .attempts
                .iter()
                .enumerate()
                .map(|(i, a)| attempt_json(self.rec, i, a, self.include_content)),
        )
    }
}

#[derive(Serialize)]
struct AttemptJson<'a> {
    index: usize,
    reason: &'static str,
    provider: &'a str,
    deployment: &'a str,
    upstream_model: &'a str,
    credential: Option<&'a str>,
    started_at: String,
    duration_ms: f64,
    ttft_ms: Option<f64>,
    status_code: Option<u16>,
    outcome: &'static str,
    failure_class: Option<&'static str>,
    retry_after_ms: Option<u64>,
    upstream_request_id: Option<&'a str>,
    error_message: Option<&'a str>,
    error_body: Option<&'a str>,
    usage: Option<UsageJson>,
    key_rotated: bool,
}

/// Whether the attempt's failure made the loop send the next attempt to the same deployment with
/// another credential.
fn key_rotated(rec: &Record, i: usize, a: &AttemptFact) -> bool {
    a.failed()
        && rec
            .attempts
            .get(i + 1)
            .is_some_and(|n| n.deployment == a.deployment && n.key.is_some() && n.key != a.key)
}

fn attempt_json<'a>(rec: &'a Record, i: usize, a: &'a AttemptFact, include_content: bool) -> AttemptJson<'a> {
    let zdr = rec.content.zdr;
    AttemptJson {
        index: i,
        reason: a.reason,
        provider: &a.provider,
        deployment: &a.deployment,
        upstream_model: &a.upstream_model,
        credential: a.credential.as_deref(),
        started_at: a.started_at.to_rfc3339_micros(),
        duration_ms: ms3(a.end.saturating_duration_since(a.start).as_secs_f64() * 1000.0),
        ttft_ms: a.ttft.map(|d| ms3(d.as_secs_f64() * 1000.0)),
        status_code: a.status,
        outcome: a.outcome(),
        failure_class: a.failed().then_some(a.class),
        retry_after_ms: a.retry_after_ms,
        upstream_request_id: a.upstream_request_id.as_deref(),
        error_message: a.error_message.as_deref().filter(|_| !zdr),
        error_body: a.error_body.as_deref().filter(|_| !zdr && include_content),
        usage: a.usage.filter(|u| !u.is_empty()).map(|u| u.json(None)),
        key_rotated: key_rotated(rec, i, a),
    }
}

struct Skipped<'a>(&'a [(String, &'static str)]);

#[derive(Serialize)]
struct SkippedJson<'a> {
    deployment: &'a str,
    credential: Option<()>,
    reason: &'static str,
}

impl Serialize for Skipped<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(self.0.iter().map(|(d, reason)| SkippedJson {
            deployment: d,
            credential: None,
            reason,
        }))
    }
}

struct Degraded<'a>(&'a [(&'static str, Option<&'static str>)]);

#[derive(Serialize)]
struct DegradedJson {
    component: &'static str,
    cause: Option<&'static str>,
}

impl Serialize for Degraded<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(self.0.iter().map(|(component, cause)| DegradedJson {
            component,
            cause: *cause,
        }))
    }
}
