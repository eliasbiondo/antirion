//! What a settled request contributes to the metrics and, when sampled, to its trace.

use std::time::Instant;

use antirion_obs::timing::Timing;
use antirion_obs::trace::{SpanData, SpanKind, Status, Value};
use antirion_obs::{Fam, LabelSource, Metrics};

use super::settle::CostFact;
use super::{AttemptFact, HeadFacts, Record, Usage};
use crate::ctx::Ctx;

fn status_class(status: u16) -> &'static str {
    match status {
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        _ => "5xx",
    }
}

fn bool_label(b: bool) -> &'static str {
    if b { "true" } else { "false" }
}

/// The OTel GenAI operation name of an endpoint.
pub(crate) fn genai_operation(operation: &str) -> &str {
    match operation {
        "chat_completions" | "messages" | "responses" | "converse" | "converse_stream" => "chat",
        "generate_content" | "stream_generate_content" => "generate_content",
        "completions" => "text_completion",
        "embeddings" => "embeddings",
        other => other,
    }
}

/// The OTel GenAI provider name of a provider type.
pub(crate) fn genai_provider(provider_type: &str) -> &str {
    match provider_type {
        "azure_openai" => "azure.ai.openai",
        "bedrock" => "aws.bedrock",
        "vertex" => "gcp.vertex_ai",
        "gemini" => "gcp.gemini",
        "mistral" => "mistral_ai",
        "xai" => "x_ai",
        "ibm_watsonx" => "ibm.watsonx.ai",
        other => other,
    }
}

/// The usage an attempt produced: its own report, or the request's for the attempt that served it.
fn attempt_usage(rec: &Record, i: usize, a: &AttemptFact) -> Option<Usage> {
    if a.failed() {
        return a.usage;
    }
    let serving = rec.serving_index() == Some(i);
    a.usage
        .or_else(|| serving.then_some(rec.usage))
        .filter(|u| !u.is_empty())
}

/// The metrics of a request without a record (refused before or at `authenticate`).
pub(crate) fn unlogged(
    cx: &Ctx,
    status: u16,
    path: &str,
    error: Option<(&'static str, Option<&'static str>)>,
    overhead_ms: f64,
) {
    let m = &cx.rt.metrics;
    let (operation, dialect) = cx.labels;
    let code = error.map_or("ok", |e| e.0);
    let total = cx.started.elapsed().as_secs_f64();
    m.add(
        Fam::Requests,
        &[
            operation,
            dialect,
            "_unresolved",
            "false",
            &status.to_string(),
            code,
            path,
        ],
        1.0,
    );
    m.observe(
        Fam::RequestDuration,
        &[operation, "_unresolved", "false", status_class(status)],
        total,
    );
    m.observe(Fam::Overhead, &[operation, "false"], overhead_ms / 1000.0);
    if code == "unauthenticated" {
        m.add(
            Fam::AuthFailures,
            &["data", error.and_then(|e| e.1).unwrap_or("")],
            1.0,
        );
    }
}

/// What the configured labels read of a record: its tenancy and its metadata.
fn label_source(rec: &Record) -> LabelSource<'_> {
    let tenant = rec
        .tenant
        .as_ref()
        .map_or(["", "", ""], |[o, t, p, _]| [o.as_str(), t.as_str(), p.as_str()]);
    LabelSource {
        tenant,
        metadata: Some(&rec.metadata),
    }
}

/// The metrics of a settled request.
pub(super) fn metrics(m: &Metrics, rec: &Record, head: &HeadFacts, timing: &Timing, cost: &CostFact) {
    let op = rec.operation;
    let model = rec.model.as_deref().unwrap_or("_unresolved");
    let stream = bool_label(rec.stream.is_some());
    let code = rec.error.as_ref().map_or("ok", |e| e.code);
    let status = head.status.to_string();
    let src = label_source(rec);
    let src = Some(&src);
    m.add_for(
        Fam::Requests,
        &[op, rec.dialect, model, stream, &status, code, &head.response_path],
        src,
        1.0,
    );
    m.observe_for(
        Fam::RequestDuration,
        &[op, model, stream, status_class(head.status)],
        src,
        timing.total_ms / 1000.0,
    );
    m.observe(Fam::Overhead, &[op, stream], timing.overhead_ms / 1000.0);
    let serving = rec.serving_attempt().filter(|a| !a.failed());
    if let Some(a) = serving {
        let labels = [op, model, a.provider.as_str(), a.deployment.as_str()];
        if let Some(t) = timing.ttft_ms {
            m.observe(Fam::Ttft, &labels, t / 1000.0);
        }
        if let Some(t) = timing.tpot_ms {
            m.observe(Fam::Tpot, &labels, t / 1000.0);
        }
        if let Some(t) = timing.upstream_ttft_ms {
            m.observe(Fam::UpstreamTtft, &[&a.provider, &a.deployment], t / 1000.0);
        }
    }
    let mut previous: Option<&AttemptFact> = None;
    for (i, a) in rec.attempts.iter().enumerate() {
        let outcome = a.outcome();
        let status = a.status.map_or_else(|| "0".to_owned(), |s| s.to_string());
        m.add(
            Fam::UpstreamAttempts,
            &[&a.provider, &a.deployment, &a.upstream_model, outcome, &status],
            1.0,
        );
        m.observe(
            Fam::UpstreamAttemptDuration,
            &[&a.provider, &a.deployment, outcome],
            a.end.saturating_duration_since(a.start).as_secs_f64(),
        );
        if let Some(prev) = previous {
            match a.reason {
                "retry" => m.add(Fam::Retries, &[&a.provider, &a.deployment, prev.class], 1.0),
                "fallback" => m.add(Fam::Fallbacks, &[model, &prev.deployment, &a.deployment], 1.0),
                _ => {}
            }
        }
        previous = Some(a);
        if a.failed() || rec.cached {
            continue;
        }
        let usage = attempt_usage(rec, i, a);
        if usage.is_none_or(|u| u.estimated) {
            m.add(Fam::UsageMissing, &[&a.provider, &a.deployment], 1.0);
        }
        for (token_type, n) in usage.map(|u| u.token_types()).unwrap_or_default() {
            m.add_for(
                Fam::Tokens,
                &[model, &a.provider, &a.deployment, &a.upstream_model, token_type],
                src,
                n as f64,
            );
        }
    }
    if let Some(a) = serving.filter(|_| !rec.cached && !rec.usage.is_empty()) {
        let n = rec.usage.normalised();
        m.observe(Fam::TokenUsage, &[model, &a.provider, "input"], n.input as f64);
        m.observe(Fam::TokenUsage, &[model, &a.provider, "output"], n.output as f64);
    }
    if let Some(a) = rec.serving_attempt().filter(|_| !rec.cached) {
        match cost.total {
            Some(total) => m.add_for(
                Fam::Cost,
                &[model, &a.provider, &a.deployment],
                src,
                total.units() as f64 / 1e9,
            ),
            None => m.add(Fam::Unpriced, &[model, &a.deployment], 1.0),
        }
    }
    if let Some(t) = &rec.mcp_telemetry {
        t.metrics(m);
    }
    if let Some(result) = rec
        .cache
        .status
        .filter(|s| matches!(*s, "hit" | "miss" | "error"))
    {
        m.add(Fam::CacheLookups, &["exact", result, model], 1.0);
    }
}

fn usage_attrs(s: &mut SpanData, u: &Usage) {
    let n = u.normalised();
    s.set("gen_ai.usage.input_tokens", n.input);
    s.set("gen_ai.usage.output_tokens", n.output);
    if n.input_cache_read > 0 {
        s.set("gen_ai.usage.cache_read.input_tokens", n.input_cache_read);
    }
    if n.input_cache_write > 0 {
        let name = if antirion_obs::latest_semconv() {
            "gen_ai.usage.cache_write.input_tokens"
        } else {
            "gen_ai.usage.cache_creation.input_tokens"
        };
        s.set(name, n.input_cache_write);
    }
    if n.output_reasoning > 0 {
        s.set("gen_ai.usage.reasoning.output_tokens", n.output_reasoning);
    }
}

fn attempt_span(rec: &Record, i: usize, a: &AttemptFact) -> SpanData {
    let op = genai_operation(rec.operation);
    let mut s = rec.trace.child(
        format!("{op} {}", a.upstream_model),
        SpanKind::Client,
        a.start,
        a.end,
    );
    s.span_id = a.span;
    s.set("gen_ai.operation.name", op);
    s.set("gen_ai.provider.name", genai_provider(&a.provider_type));
    s.set("gen_ai.request.model", a.upstream_model.as_str());
    for (k, v) in &rec.request_params {
        s.set(k, v.clone());
    }
    if let Some((host, port)) = &a.server {
        s.set("server.address", host.as_str());
        s.set("server.port", i64::from(*port));
    }
    if let Some(st) = a.status {
        s.set("http.response.status_code", i64::from(st));
    }
    s.set("antirion.deployment", a.deployment.as_str());
    s.set("antirion.attempt.index", i as i64);
    s.set("antirion.attempt.reason", a.reason);
    if let Some(t) = a.ttft {
        s.set(
            "antirion.ttft_ms",
            antirion_obs::timing::ms3(t.as_secs_f64() * 1000.0),
        );
    }
    if a.failed() {
        s.set("error.type", a.class);
        let message = if rec.content.zdr {
            String::new()
        } else {
            a.error_message.clone().unwrap_or_default()
        };
        s.status = Status::Error(message);
        return s;
    }
    if let Some(m) = &rec.response_model {
        s.set("gen_ai.response.model", m.as_str());
    }
    if let Some(id) = &rec.response_id {
        s.set("gen_ai.response.id", id.as_str());
    }
    s.set(
        "gen_ai.response.finish_reasons",
        Value::StrArray(rec.finish_reasons.clone()),
    );
    if let Some(u) = attempt_usage(rec, i, a) {
        usage_attrs(&mut s, &u);
    }
    if rec.content.enabled && rec.serving_index() == Some(i) {
        if let Some(m) = &rec.content.input_messages {
            s.set("gen_ai.input.messages", m.as_str());
        }
        if let Some(m) = &rec.content.output_messages {
            s.set("gen_ai.output.messages", m.as_str());
        }
    }
    s
}

/// The spans of a sampled request: the SERVER root, `authenticate`, the INTERNAL spans of the stages
/// that did work and one CLIENT span per attempt.
pub(super) fn spans(
    rec: &Record,
    head: &HeadFacts,
    timing: &Timing,
    cost: &CostFact,
    t_end: Instant,
) -> Vec<SpanData> {
    let t = &rec.trace;
    let mut root = SpanData::new(
        t.trace_id,
        t.parent,
        format!("{} {}", rec.method, rec.endpoint),
        SpanKind::Server,
    );
    root.span_id = t.root;
    root.start_unix_nanos = t.unix_ns(rec.started);
    root.end_unix_nanos = t.unix_ns(t_end);
    root.set("http.request.method", rec.method.as_str());
    root.set("http.route", rec.endpoint.as_str());
    root.set("http.response.status_code", i64::from(head.status));
    root.set("antirion.request_id", rec.request_id.as_str());
    if rec.logged {
        root.set("antirion.log_id", rec.id.as_str());
    }
    if let Some(m) = &rec.model {
        root.set("antirion.model", m.as_str());
    }
    root.set("antirion.response_path", head.response_path.as_str());
    root.set("antirion.attempts", rec.attempts.len() as i64);
    root.set("antirion.overhead_ms", timing.overhead_ms);
    if let Some([org, team, project, key]) = &rec.tenant {
        root.set("antirion.organization", org.as_str());
        root.set("antirion.team", team.as_str());
        root.set("antirion.project", project.as_str());
        root.set("antirion.key", key.as_str());
    }
    match cost.total {
        Some(c) => root.set("antirion.cost_usd", c.units() as f64 / 1e9),
        None => root.set("antirion.cost_usd", "unpriced"),
    }
    if !rec.usage.is_empty() {
        usage_attrs(&mut root, &rec.usage);
    }
    if let Some(sid) = &rec.session_id {
        root.set("session.id", sid.as_str());
    }
    if rec.snap.obs.traces.include_end_user
        && let Some(eu) = &rec.end_user
    {
        root.set("antirion.end_user", eu.as_str());
    }
    for (k, v) in &rec.metadata {
        if let Some(v) = v.as_str() {
            root.set(&format!("antirion.meta.{k}"), v);
        }
    }
    for (attr, v) in &rec.header_attributes {
        root.set(attr, v.as_str());
    }
    if let Some(e) = &rec.error {
        if head.status >= 500 {
            let message = if rec.content.zdr && e.upstream {
                String::new()
            } else {
                e.message.clone()
            };
            root.status = Status::Error(message);
        }
        root.set("error.type", e.code);
    }
    if let Some(m) = &rec.mcp_telemetry {
        m.decorate(&mut root);
    }
    let mut out = vec![root];
    if let Some(m) = &rec.mcp_telemetry {
        out.extend(m.client_spans(t));
    }
    if let Some((a, b)) = rec.auth_span {
        out.push(t.child("antirion.authenticate".into(), SpanKind::Internal, a, b));
    }
    out.extend(rec.internal_spans.iter().map(|s| {
        let mut span = t.child(s.name.clone(), SpanKind::Internal, s.start, s.end);
        for (k, v) in &s.attrs {
            span.set(k, *v);
        }
        span
    }));
    out.extend(
        rec.attempts
            .iter()
            .enumerate()
            .map(|(i, a)| attempt_span(rec, i, a)),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genai_names() {
        assert_eq!(genai_operation("chat_completions"), "chat");
        assert_eq!(genai_operation("completions"), "text_completion");
        assert_eq!(genai_operation("images"), "images");
        assert_eq!(genai_provider("bedrock"), "aws.bedrock");
        assert_eq!(genai_provider("openai"), "openai");
        assert_eq!(status_class(429), "4xx");
    }
}
