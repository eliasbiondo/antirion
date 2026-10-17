//! What the inference stages tell the request's record, metrics and trace once the Model is known:
//! whether content is recorded, the session value, the request parameters of the attempt spans, the
//! cache lookup and each routing decision.

use antirion_core::Stage;
use antirion_error::{GatewayError, Message};
use antirion_identity::reasons;
use antirion_obs::Fam;
use antirion_obs::trace::Value as Attr;
use antirion_router::{CompiledRoute, NodeKind};
use http::HeaderMap;
use serde_json::Value;

use crate::ctx::Ctx;
use crate::parse::Parsed;
use crate::policy;

/// Decides content logging and zero data retention for the request, refusing
/// `x-antirion-log-content: true` when zero data retention applies at a Model or route level.
pub(crate) fn content(
    cx: &Ctx,
    policies: &policy::Levels<'_>,
    header: Option<bool>,
) -> Result<(), GatewayError> {
    let o = policies.effective("observability");
    let zdr = o.get("zero_data_retention") == Some(&Value::Bool(true));
    if zdr && header == Some(true) {
        return Err(GatewayError::with_reason(
            reasons::OVERRIDE_NOT_ALLOWED,
            Stage::Resolve,
            Message::Fixed("Content logging cannot be turned on by this request."),
        )
        .param("x-antirion-log-content"));
    }
    let policy_on = o.pointer("/content/enabled") == Some(&Value::Bool(true));
    let enabled = !zdr && header.unwrap_or(policy_on);
    let redact_pii = (o.pointer("/content/redact_pii") == Some(&Value::Bool(true))).then(|| {
        match o.pointer("/content/on_pii").and_then(Value::as_str) {
            Some("hash") => crate::reqlog::PiiAction::Hash,
            Some("drop_content") => crate::reqlog::PiiAction::DropContent,
            _ => crate::reqlog::PiiAction::Mask,
        }
    });
    cx.log.update(|r| {
        r.content.enabled = enabled;
        r.content.zdr = zdr;
        r.content.redact_pii = redact_pii;
    });
    Ok(())
}

const PARAMS: &[(&str, &str)] = &[
    ("temperature", "gen_ai.request.temperature"),
    ("top_p", "gen_ai.request.top_p"),
    ("max_tokens", "gen_ai.request.max_tokens"),
    ("max_completion_tokens", "gen_ai.request.max_tokens"),
    ("max_output_tokens", "gen_ai.request.max_tokens"),
    ("seed", "gen_ai.request.seed"),
    ("frequency_penalty", "gen_ai.request.frequency_penalty"),
    ("presence_penalty", "gen_ai.request.presence_penalty"),
];

fn attr(v: &Value) -> Option<Attr> {
    match v {
        Value::Number(n) => n.as_i64().map(Attr::Int).or_else(|| n.as_f64().map(Attr::F64)),
        Value::String(s) => Some(Attr::StrArray(vec![s.clone()])),
        Value::Array(items) => Some(Attr::StrArray(
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
        )),
        _ => None,
    }
}

/// The request facts read from the body: the session value (after the header), the `gen_ai.request.*`
/// parameters and, when content logging applies, the body itself.
pub(crate) fn request(cx: &Ctx, headers: &HeaderMap, parsed: &Parsed) {
    let (raw, body) = match parsed {
        Parsed::Json { raw, .. } => (Some(raw), parsed.doc()),
        Parsed::Multipart { .. } => (None, None),
    };
    let mut params: Vec<(&'static str, Attr)> = Vec::new();
    if let Some(b) = &body {
        for (field, name) in PARAMS {
            if let Some(v) = b.get(*field).filter(|v| v.is_number()).and_then(attr)
                && !params.iter().any(|(n, _)| n == name)
            {
                params.push((name, v));
            }
        }
        if let Some(v) = b.get("stop").or_else(|| b.get("stop_sequences")).and_then(attr) {
            params.push(("gen_ai.request.stop_sequences", v));
        }
    }
    let later = crate::meta::later_session(headers, body);
    let tier = body
        .and_then(|b| b.get("service_tier"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    cx.log.update(|r| {
        r.request_params = params;
        if tier.is_some() {
            r.service_tier = tier;
        }
        if r.session_id.is_none() {
            r.session_id = later;
        }
    });
    if let Some(raw) = raw {
        cx.log.request_content(raw, body);
    }
}

/// Records the cache status of the request and the key it was looked up or stored under.
pub(crate) fn cache_lookup(cx: &Ctx, status: &'static str, key: Option<&str>) {
    cx.log.update(|r| {
        r.cache.status = Some(status);
        r.cache.key = key.map(str::to_owned);
        r.cached = status == "hit";
    });
}

fn strategy(route: &CompiledRoute) -> &'static str {
    match route.node(0).map(|n| &n.kind) {
        Some(NodeKind::Deployment(_) | NodeKind::ProviderModel { .. }) => "single",
        Some(k) => k.type_name(),
        None => "single",
    }
}

/// Counts a deployment picked by `model`'s route.
pub(crate) fn decision(cx: &Ctx, model: &str, route: &CompiledRoute, deployment: &str) {
    cx.rt
        .metrics
        .add(Fam::RoutingDecisions, &[model, deployment, strategy(route)], 1.0);
}
