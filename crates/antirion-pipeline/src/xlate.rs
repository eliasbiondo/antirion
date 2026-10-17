//! Translated cells: the client request decoded once, the translation policy, the feasibility of each
//! candidate deployment, and each attempt's translated body.

use std::collections::HashMap;
use std::sync::Arc;

use antirion_core::Stage;
pub(crate) use antirion_dialects::xlate::decode::Invalid;
use antirion_dialects::xlate::decode::decode;
use antirion_dialects::xlate::encode::{Encoded, Restore, encode, fetch_needs};
use antirion_dialects::xlate::ir::{Client, Family, Request};
use antirion_dialects::xlate::target::{
    Budgets, Control, NameRule, Policy, Target, Upstream as XUp, default_names,
};
use antirion_dialects::xlate::warn::{Refusal, RefusalKind, Warnings};
use antirion_dialects::xlate::{replay, resp};
use antirion_error::reason::hygiene;
use antirion_error::{Code, GatewayError, Message};
use antirion_identity::KeyPermission;
use antirion_providers::{DeploymentRt, Source};
use antirion_resilience::{Class, Failure};
use antirion_runtime::Runtime;
use antirion_runtime::identity::KeyRt;
use antirion_runtime::replay::ReplaySettings;
use http::HeaderMap;
use serde_json::Value;

use crate::apis;
use crate::caps::{Need, Sizes};
use crate::cell::{Cell, Upstream, cell};
use crate::ctx::Ctx;
use crate::media::Fetches;
use crate::outcome::{Attempt, upstream_or_new_id, wall_clock_secs};
use crate::parse::Parsed;
use crate::reasons;
use crate::relay::ClientDialect;

/// `x-antirion-translation`.
pub(crate) const TRANSLATION: &str = "x-antirion-translation";

/// The decoded request of a translated cell, shared by every attempt.
#[derive(Debug)]
pub(crate) struct Xlate {
    pub(crate) ir: Request,
    pub(crate) policy: Policy,
    /// The client's body (Responses echo fields).
    pub(crate) body: Value,
    /// Remote media fetched for this request.
    pub(crate) fetched: tokio::sync::Mutex<Fetches>,
    /// The project and settings of the reasoning replay store, when it is on.
    pub(crate) replay: Option<(String, ReplaySettings)>,
    /// The replay store put reasoning back into the request.
    pub(crate) restored: bool,
}

/// What one attempt's translation produced, for its response.
#[derive(Clone, Debug)]
pub(crate) struct AttemptXlate {
    pub(crate) warnings: Warnings,
    pub(crate) restore: Restore,
    pub(crate) upstream: XUp,
}

pub(crate) fn client_of(c: ClientDialect) -> Option<Client> {
    match c {
        ClientDialect::Chat => Some(Client::Chat),
        ClientDialect::Responses => Some(Client::Responses),
        ClientDialect::Anthropic => Some(Client::Anthropic),
        ClientDialect::Gemini => Some(Client::Gemini),
        ClientDialect::Bedrock => Some(Client::Converse),
    }
}

pub(crate) fn upstream_of(u: Upstream) -> XUp {
    match u {
        Upstream::Chat => XUp::Chat,
        Upstream::Responses => XUp::Responses,
        Upstream::Anthropic => XUp::Anthropic,
        Upstream::Gemini => XUp::Gemini,
        Upstream::Converse => XUp::Converse,
        Upstream::Cohere => XUp::Cohere,
    }
}

/// Puts back the reasoning the replay store kept for the request's tool calls.
pub(crate) fn restore(rt: &Runtime, project: &str, ir: &mut Request) -> bool {
    if ir.calls().next().is_none() {
        return false;
    }
    let now = rt.now();
    replay::restore(ir, |id| rt.state.replay.get(project, id, now))
}

/// The 400 for a part the gateway cannot use.
pub(crate) fn invalid(i: Invalid) -> GatewayError {
    GatewayError::with_reason(reasons::MEDIA_INVALID, Stage::Parse, Message::built(i.message)).param(i.param)
}

/// Decodes the client body for translated cells; a native path that streams sets the stream flag
/// the body cannot carry.
pub(crate) fn decode_body(
    client: Client,
    parsed: &Parsed,
    endpoint: &antirion_dialects::Endpoint,
) -> Result<(Request, Value), GatewayError> {
    let body: Value = parsed.doc().cloned().unwrap_or(Value::Null);
    let mut ir = decode(client, &body).map_err(invalid)?;
    ir.stream |= endpoint.path_streams();
    Ok((ir, body))
}

/// The request's translation header: `Some(true)` for `strict`, `Some(false)` for `warn`.
pub(crate) fn header(headers: &HeaderMap) -> Result<Option<bool>, GatewayError> {
    let Some(v) = headers.get(TRANSLATION) else {
        return Ok(None);
    };
    match v.to_str().map(str::trim) {
        Ok("strict") => Ok(Some(true)),
        Ok("warn") => Ok(Some(false)),
        _ => Err(GatewayError::with_reason(
            hygiene::INVALID_HEADER,
            Stage::Parse,
            Message::Fixed("x-antirion-translation must be strict or warn."),
        )
        .param(TRANSLATION)),
    }
}

/// The effective translation policy: the merged `translation` policy, made stricter by `strict` and
/// relaxed by `warn` only for a key with `override:relax`.
pub(crate) fn policy(effective: &Value, header: Option<bool>, key: &KeyRt) -> Result<Policy, GatewayError> {
    let configured_reject = effective.get("on_lossy").and_then(Value::as_str) == Some("reject");
    let reject = match header {
        Some(true) => true,
        Some(false) if configured_reject => {
            if !key.permissions.has(KeyPermission::OverrideRelax) {
                return Err(GatewayError::with_reason(
                    antirion_identity::reasons::PERMISSION_REQUIRED,
                    Stage::Parse,
                    Message::Fixed("Relaxing the translation policy needs the `override:relax` permission."),
                )
                .param(TRANSLATION));
            }
            false
        }
        _ => configured_reject,
    };
    let mut budgets = Budgets::default();
    if let Some(b) = effective.get("reasoning_budgets").and_then(Value::as_object) {
        for (i, name) in ["minimal", "low", "medium", "high", "xhigh", "max"]
            .iter()
            .enumerate()
        {
            if let (Some(v), Some(slot)) = (b.get(*name).and_then(Value::as_u64), budgets.0.get_mut(i)) {
                *slot = v;
            }
        }
    }
    Ok(Policy {
        reject,
        forward_unknown: effective.get("forward_unknown_fields").and_then(Value::as_bool) == Some(true),
        budgets,
    })
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

/// What the translation knows about a deployment, for the upstream dialect it is translated to.
pub(crate) fn target(doc: &Value, d: &DeploymentRt, dialect: XUp) -> Target {
    let caps = &d.capabilities;
    let get = |n: &str| caps.get(n);
    let num = |n: &str| get(n).0.as_u64();
    let mut t = Target::defaults(dialect);
    t.provider.clone_from(&d.provider.name);
    t.model.clone_from(&d.model);
    let lower = d.model.to_ascii_lowercase();
    t.claude = lower.contains("claude");
    t.chat_max_field = if d.provider.kind.compatible() {
        "max_tokens"
    } else {
        "max_completion_tokens"
    };
    t.family = get("reasoning_family")
        .0
        .as_str()
        .map_or(Family::None, Family::parse);
    let (pattern, source) = get("tool_name_pattern");
    t.names = match (pattern.as_str(), source) {
        (Some(p), Source::Deployment | Source::Provider) => NameRule {
            pattern: p.to_owned(),
            max_len: num("tool_name_max_len")
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(64),
        },
        _ => default_names(dialect),
    };
    t.max_breakpoints = num("max_cache_breakpoints")
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(4);
    t.accepts_cache_control = caps.flag("accepts_cache_control");
    t.native_structured = caps.flag("native_structured_output");
    let (range, source) = get("temperature_range");
    t.temperature_range = match (range.as_array(), source) {
        (Some(r), Source::Deployment | Source::Provider) => match (
            r.first().and_then(Value::as_f64),
            r.get(1).and_then(Value::as_f64),
        ) {
            (Some(lo), Some(hi)) => Some((lo, hi)),
            _ => None,
        },
        _ if dialect == XUp::Anthropic
            || dialect == XUp::Cohere
            || (dialect == XUp::Converse && t.claude) =>
        {
            Some((0.0, 1.0))
        }
        _ => Some((0.0, 2.0)),
    };
    t.exclusive_temperature_top_p = caps.flag("exclusive_temperature_top_p");
    t.sampling_params = caps.flag("sampling_params");
    t.max_stop = num("max_stop_sequences").and_then(|n| usize::try_from(n).ok());
    t.control = get("reasoning_control")
        .0
        .as_str()
        .map_or(Control::None, Control::parse);
    t.efforts = strings(&get("reasoning_efforts").0);
    t.min_budget = num("reasoning_min_budget");
    t.max_budget = num("reasoning_max_budget");
    t.can_disable = caps.flag("reasoning_can_disable");
    t.budget_ok = caps.flag("reasoning_budget_ok");
    t.thought_signatures = match get("thought_signatures") {
        (v, Source::Deployment | Source::Provider) => v.as_bool().unwrap_or(false),
        _ => lower.contains("gemini-3"),
    };
    t.image_formats = strings(&get("image_formats").0);
    t.image_url = caps.flag("image_url_input");
    t.document_formats = strings(&get("document_formats").0);
    t.document_url = caps.flag("document_url_input");
    t.audio_formats = strings(&get("audio_formats").0);
    t.max_output_tokens = num("max_output_tokens").filter(|n| *n > 0);
    t.default_max_tokens = d.default_max_tokens.max(1);
    t.developer_role = caps.flag("developer_role");
    t.assistant_prefill = caps.flag("assistant_prefill");
    t.media_fetch = crate::media::enabled(doc);
    t.stream_usage = caps.flag("stream_usage");
    t
}

/// Whether a deployment can take the request in `cell`: `Ok` for identity cells and feasible
/// translations, the refusal otherwise.
pub(crate) fn feasible(doc: &Value, x: &Xlate, d: &DeploymentRt, cell: Cell) -> Result<(), Refusal> {
    let Cell::Translate { upstream, .. } = cell else {
        return Ok(());
    };
    let t = target(doc, d, upstream_of(upstream));
    encode(&x.ir, &t, &x.policy, None).map(|_| ())
}

/// The error of a refusal at `resolve`.
pub(crate) fn refusal_error(r: &Refusal) -> GatewayError {
    let reason = match r.kind {
        RefusalKind::Strict => reasons::TRANSLATION_STRICT,
        RefusalKind::Unsupported => hygiene::TRANSLATION_UNSUPPORTED,
        RefusalKind::Unpaired => {
            return GatewayError::with_reason(
                reasons::TOOL_RESULT_UNPAIRED,
                Stage::Parse,
                Message::built(r.message.clone()),
            )
            .param(r.param.clone());
        }
    };
    GatewayError::with_reason(reason, Stage::Resolve, Message::built(r.message.clone()))
        .param(r.param.clone())
        .openai_code("unsupported_parameter")
}

/// One attempt's translated body: remote media fetched (once per request), then `ir` (the request's
/// decoded body, or that of the body route params edited) encoded.
pub(crate) async fn body(
    cx: &Ctx,
    x: &Xlate,
    ir: &Request,
    d: &DeploymentRt,
    up: Upstream,
) -> Result<(Encoded, XUp), GatewayError> {
    let dialect = upstream_of(up);
    let t = target(&cx.snap.config.doc, d, dialect);
    let needs = fetch_needs(ir, &t);
    let mut fetched = x.fetched.lock().await;
    if !needs.is_empty() && t.media_fetch {
        crate::media::fetch_needs(cx, &needs, &mut fetched).await?;
    }
    let encoded = encode(ir, &t, &x.policy, Some(&fetched.map)).map_err(|r| {
        let mut e = refusal_error(&r);
        e.stage = Stage::Attempt;
        e
    })?;
    Ok((encoded, dialect))
}

/// Why a candidate deployment was left out at `resolve`.
#[derive(Debug)]
pub(crate) enum Excluded {
    Caps(Vec<crate::caps::Need>),
    Size(&'static str),
    Translation(Refusal),
}

impl Excluded {
    fn reason(&self) -> String {
        match self {
            Excluded::Caps(missing) => missing.first().map_or("operations", |n| n.flag).to_owned(),
            Excluded::Size(flag) => (*flag).to_owned(),
            Excluded::Translation(r) => r.reason().to_owned(),
        }
    }
}

/// The candidate's exclusion, if any: a missing capability first, then a size it cannot take, then
/// the translation classes.
pub(crate) fn exclusion(
    doc: &Value,
    x: Option<&Xlate>,
    d: &DeploymentRt,
    cell: Option<Cell>,
    needs: &[Need],
    sizes: Sizes,
) -> Option<Excluded> {
    let missing = crate::caps::missing(d, needs);
    if !missing.is_empty() {
        return Some(Excluded::Caps(missing));
    }
    if let Some(flag) = crate::caps::size_unmet(d, sizes) {
        return Some(Excluded::Size(flag));
    }
    match (x, cell) {
        (Some(x), Some(c)) => feasible(doc, x, d, c).err().map(Excluded::Translation),
        _ => None,
    }
}

/// The 400 when every candidate is excluded: the first candidate's reason and field, and a message
/// listing each distinct reason.
pub(crate) fn none_left(first: &DeploymentRt, all: &[Excluded]) -> GatewayError {
    if let Some(r) = all.iter().find_map(|x| match x {
        Excluded::Translation(r) if r.kind == RefusalKind::Unpaired => Some(r),
        _ => None,
    }) {
        return refusal_error(r);
    }
    let mut e = match all.first() {
        Some(Excluded::Caps(missing)) => crate::caps::unsupported_on(first, missing),
        Some(Excluded::Size(flag)) => crate::caps::unsupported_flag(flag),
        Some(Excluded::Translation(r)) => refusal_error(r),
        None => crate::caps::unsupported(&[]),
    };
    let first_reason = all.first().map(Excluded::reason).unwrap_or_default();
    let mut others: Vec<String> = Vec::new();
    for x in all.iter().skip(1) {
        let r = x.reason();
        if r != first_reason && !others.contains(&r) {
            others.push(r);
        }
    }
    if !others.is_empty() {
        let base = e.message.as_str().trim_end_matches('.').to_owned();
        e.message = Message::built(format!(
            "{base} ({first_reason}); other candidates: {}.",
            others.join(", ")
        ));
    } else if let Some(Excluded::Translation(_)) = all.first() {
        let base = e.message.as_str().trim_end_matches('.').to_owned();
        e.message = Message::built(format!("{base} ({first_reason})."));
    }
    e
}

/// The candidates of a request at `resolve`.
pub(crate) struct Candidates<'a> {
    pub(crate) endpoint: &'a antirion_dialects::Endpoint,
    pub(crate) parsed: &'a Parsed,
    pub(crate) servable: &'a [&'a Arc<DeploymentRt>],
    pub(crate) needs: &'a [Need],
    pub(crate) sizes: Sizes,
    pub(crate) doc: &'a Value,
    pub(crate) rt: &'a Runtime,
}

/// What `resolve` learned about translation: the decoded request (when some candidate is a
/// translated cell) and each candidate's translation verdict (`None` when feasible), for the
/// routing filters.
pub(crate) struct Feasibility {
    pub(crate) xlate: Option<Arc<Xlate>>,
    pub(crate) verdicts: HashMap<String, Option<Refusal>>,
}

/// Decodes the request when a candidate translates it, then decides every candidate's eligibility
/// (capabilities, sizes, then translation classes); the 400 when none remains.
pub(crate) fn resolve(
    c: &Candidates<'_>,
    effective: impl FnOnce() -> Value,
    header: Option<bool>,
    key: &KeyRt,
) -> Result<Feasibility, GatewayError> {
    let translated = c
        .servable
        .iter()
        .find_map(|d| match cell(c.endpoint, c.parsed, d) {
            Some(Cell::Translate { client, .. }) => client_of(client),
            _ => None,
        });
    let xlate = match (translated, c.parsed) {
        (Some(client), parsed @ Parsed::Json { .. }) => {
            let (mut ir, body) = decode_body(client, parsed, c.endpoint)?;
            let policy = policy(&effective(), header, key)?;
            let replay = ReplaySettings::of(c.doc).map(|s| (key.project.clone(), s));
            let restored = replay
                .as_ref()
                .is_some_and(|(project, _)| restore(c.rt, project, &mut ir));
            Some(Arc::new(Xlate {
                ir,
                policy,
                body,
                fetched: tokio::sync::Mutex::new(Fetches::default()),
                replay,
                restored,
            }))
        }
        _ => None,
    };
    let excluded: Vec<(String, Option<Excluded>)> = c
        .servable
        .iter()
        .map(|d| {
            let cell = cell(c.endpoint, c.parsed, d);
            (
                d.name.clone(),
                exclusion(c.doc, xlate.as_deref(), d, cell, c.needs, c.sizes),
            )
        })
        .collect();
    if let Some(first) = c.servable.first()
        && excluded.iter().all(|(_, e)| e.is_some())
    {
        let all: Vec<Excluded> = excluded.into_iter().filter_map(|(_, e)| e).collect();
        return Err(none_left(first, &all));
    }
    let verdicts = excluded
        .into_iter()
        .filter_map(|(n, e)| match e {
            Some(Excluded::Translation(r)) => Some((n, Some(r))),
            None => Some((n, None)),
            Some(_) => None,
        })
        .collect();
    Ok(Feasibility { xlate, verdicts })
}

/// A client body: `Ok(None)` for the upstream's own, `Err(())` when unusable; with the translation's
/// entries in a translated cell.
pub(crate) type Translated = (Result<Option<Value>, ()>, Option<Warnings>);

/// The client body of a complete upstream response; in a translated cell the translated body and
/// the translation's entries. A malformed function call is a retryable attempt failure.
pub(crate) fn response(
    a: &Attempt<'_>,
    api: apis::Api,
    v: &Value,
    shown: &str,
    up_headers: &HeaderMap,
    status: u16,
) -> Result<Translated, Failure> {
    let (Cell::Translate { client, .. }, Some(x), Some(px)) = (a.cell, &a.xlate, &a.plan.xlate) else {
        return Ok((Ok(apis::client_body(a.cell, api, v)), None));
    };
    if matches!(
        api,
        apis::Api::GeminiCount | apis::Api::AnthropicCount | apis::Api::ResponsesCount
    ) {
        return Ok((Ok(apis::client_body(a.cell, api, v)), None));
    }
    let Some(client) = crate::xlate::client_of(client) else {
        return Ok((Err(()), None));
    };
    let mut w = x.warnings.clone();
    let fallback = upstream_or_new_id(up_headers);
    let cx = resp::Cx {
        upstream: x.upstream,
        client,
        restore: &x.restore,
        provider: &a.deployment.provider.name,
        model: shown,
        created: wall_clock_secs(),
        fallback_id: &fallback,
        request: &px.body,
    };
    match resp::decode(v, &cx, &mut w) {
        Ok(decoded) => {
            if let Some((project, settings)) = &px.replay {
                let entries = replay::entries(&decoded, cx.provider);
                if !entries.is_empty() {
                    a.cx.rt
                        .state
                        .replay
                        .keep(project, &entries, a.cx.rt.now(), *settings);
                }
            }
            Ok((Ok(Some(resp::encode(&decoded, &cx, &mut w))), Some(w)))
        }
        Err(resp::Bad::Malformed) => Err(a.fail(
            Class::ServerError,
            Code::UpstreamBadResponse,
            Some(reasons::MALFORMED_FUNCTION_CALL),
            Some(status),
            Message::Fixed("The upstream returned a malformed function call."),
        )),
        Err(resp::Bad::Unusable) => Ok((Err(()), Some(w))),
    }
}
