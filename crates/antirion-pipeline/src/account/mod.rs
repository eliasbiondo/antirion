//! Limits, budgets and cost of one inference request: the output-cap check, admission at stage
//! `admit` (the estimate, the plan and the reservation), the holds of each attempt, and settlement
//! before the last response byte, with the ledger lines and the headers they produce.

mod deferred;
mod inbody;
mod prompt;
mod settle;
mod shadow;
pub(crate) mod units;

use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard};

use antirion_core::Stage;
use antirion_dialects::{Dialect, Endpoint};
use antirion_error::{GatewayError, Message};
use antirion_limits::applicable::{RequestAttrs, Scope};
use antirion_limits::cost::CostComponents;
use antirion_limits::degraded::Degraded;
use antirion_limits::estimate::{CandidateFacts, PromptFacts, Tokenizer, estimate};
use antirion_limits::headers;
use antirion_limits::money::{Dec, Nusd};
use antirion_limits::plan::{AdmitInput, plan, plan_holds};
use antirion_limits::price::Price;
use antirion_limits::refusal::output_cap_error;
use antirion_limits::reservation::{Reservation, Settlement};
use antirion_limits::store::ReservationId;
use antirion_limits::weights::Incurred;
use antirion_providers::DeploymentRt;
use antirion_runtime::identity::KeyRt;
use antirion_runtime::pricing::Priced;
use antirion_runtime::{Runtime, Snapshot};
use http::{HeaderName, HeaderValue, Response};
use serde_json::Value;

pub(crate) use self::deferred::{Deferred, DeferredCall, settle_deferred};
pub(crate) use self::inbody::{
    details as cost_details_json, number as cost_number, report as report_in_body,
};
pub(crate) use self::settle::{End, StreamEnd, settle};
pub(crate) use self::shadow::{ShadowCall, ShadowFacts, settle_shadow};
use crate::ctx::{Body, Ctx};
use crate::parse::Parsed;

/// The tenancy and routing facts every ledger line of the request carries, and the attributes its
/// counters key on.
#[derive(Clone, Debug)]
pub(crate) struct LineFacts {
    pub(crate) organization: String,
    pub(crate) team: String,
    pub(crate) project: String,
    pub(crate) key: String,
    pub(crate) user: Option<String>,
    pub(crate) end_user: Option<String>,
    pub(crate) tags: Vec<String>,
    pub(crate) model: String,
    pub(crate) route: Option<String>,
    pub(crate) endpoint: &'static str,
    pub(crate) stream: bool,
    pub(crate) ip: Option<IpAddr>,
    pub(crate) multiplier: Dec,
    /// The key's priority class.
    pub(crate) priority_class: Option<String>,
}

impl LineFacts {
    pub(crate) fn attrs(&self) -> RequestAttrs<'_> {
        RequestAttrs {
            organization: &self.organization,
            team: &self.team,
            project: &self.project,
            key: &self.key,
            user: self.user.as_deref(),
            end_user: self.end_user.as_deref(),
            model: Some(&self.model),
            route: self.route.as_deref(),
            endpoint: self.endpoint,
            tags: &self.tags,
            ip: self.ip,
            mcp_server: None,
            mcp_tool: None,
            a2a_agent: None,
            priority_class: self.priority_class.as_deref(),
        }
    }
}

/// An admitted request's reservation until it is settled. Dropped unsettled (a panic), it refunds
/// everything on a tracked task, so no hold outlives its request.
#[derive(Debug)]
pub(crate) struct Pending {
    rt: Arc<Runtime>,
    snap: Arc<Snapshot>,
    request_id: String,
    reservation: Option<Reservation>,
    facts: LineFacts,
    attempts: u16,
}

impl Drop for Pending {
    fn drop(&mut self) {
        let Some(r) = self.reservation.take() else {
            return;
        };
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let rt = Arc::clone(&self.rt);
        self.rt.tasks.spawn(async move {
            rt.accounts.settle(&r, &Settlement::default()).await;
        });
    }
}

#[derive(Debug, Default)]
struct State {
    pending: Option<Pending>,
    headers: Vec<(HeaderName, HeaderValue)>,
    /// The billed cost once settled; `Some(None)` when it is unknown (an unpriced call).
    cost: Option<Option<Nusd>>,
    /// The settled cost's reported groups, summing to it.
    components: CostComponents,
    /// Limits and budgets evaluated without the counter store, at admission or settlement.
    degraded: Degraded,
}

/// The accounting of one request, shared by its stages and the relay task of a committed stream.
#[derive(Clone, Debug, Default)]
pub(crate) struct Slot(Arc<Mutex<State>>);

impl Slot {
    fn lock(&self) -> MutexGuard<'_, State> {
        match self.0.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn take(&self) -> Option<Pending> {
        self.lock().pending.take()
    }

    fn put(&self, p: Pending) {
        self.lock().pending = Some(p);
    }

    /// The billed cost of the settled request; `None` before settlement or when unknown.
    pub(crate) fn cost(&self) -> Option<Nusd> {
        self.lock().cost.flatten()
    }

    fn settled(&self, cost: Option<Nusd>, components: CostComponents) {
        let mut s = self.lock();
        s.cost = Some(cost);
        s.components = components;
    }

    /// `x-antirion-cost-details` of the settled request: its non-zero cost groups, which sum to
    /// `x-antirion-cost`; `None` when the cost is unknown or zero.
    pub(crate) fn cost_details(&self) -> Option<String> {
        let s = self.lock();
        s.cost.flatten()?;
        Some(headers::cost_details(&s.components)).filter(|d| !d.is_empty())
    }

    /// The settled cost and its groups, for `usage.cost` in a response body.
    pub(crate) fn cost_breakdown(&self) -> Option<(Nusd, CostComponents)> {
        let s = self.lock();
        Some((s.cost.flatten()?, s.components))
    }

    /// Records degraded limits or budgets; the merged set, for the log record.
    fn degrade(&self, d: Degraded) -> Degraded {
        let mut s = self.lock();
        s.degraded.merge(d);
        s.degraded
    }

    /// Adds the rate-limit, reservation and budget headers of the admission, and the cost of a
    /// settled complete response (a stream's cost is a trailer), to a response.
    pub(crate) fn decorate(&self, resp: &mut Response<Body>, streaming: bool) {
        let s = self.lock();
        let h = resp.headers_mut();
        for (name, value) in &s.headers {
            h.insert(name.clone(), value.clone());
        }
        crate::mark_degraded(&s.degraded.components(), h);
        if !streaming
            && let Some(Some(cost)) = s.cost
            && let Ok(v) = HeaderValue::from_str(&headers::cost(cost))
        {
            h.insert(HeaderName::from_static(headers::COST), v);
            let details = headers::cost_details(&s.components);
            if !details.is_empty()
                && let Ok(v) = HeaderValue::from_str(&details)
            {
                h.insert(HeaderName::from_static(headers::COST_DETAILS), v);
            }
        }
    }
}

/// Whether the client accepts trailers (`TE: trailers`).
pub(crate) fn accepts_trailers(headers: &http::HeaderMap) -> bool {
    headers
        .get_all(http::header::TE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case("trailers")))
}

/// The output cap the request asks of the upstream, in any dialect.
pub(crate) fn requested_output(endpoint: &Endpoint, body: &Value) -> Option<u64> {
    prompt::requested_output(endpoint, body)
}

/// Refuses an explicit output cap or choice count out of range, before anything is reserved.
pub(crate) fn check_caps(cx: &Ctx, endpoint: &Endpoint, body: Option<&Value>) -> Result<(), GatewayError> {
    let Some(body) = body else {
        return Ok(());
    };
    let ceiling = cx.snap.limits.limits.estimation.max_output_tokens_ceiling;
    prompt::output_cap(endpoint, body, ceiling)
        .map(|_| ())
        .map_err(|e| output_cap_error(&e.field))
}

/// Marks a request that carries no tokens (an upload, a batch submission): settlement counts its
/// calls, never an estimate of their input.
pub(crate) fn no_tokens(log: &crate::reqlog::Draft) {
    log.update(|r| {
        r.usage.input.get_or_insert(0);
        r.usage.output.get_or_insert(0);
    });
}

/// What admission reads about a request.
pub(crate) struct Admission<'a> {
    pub(crate) endpoint: &'a Endpoint,
    pub(crate) parsed: &'a Parsed,
    pub(crate) key: &'a KeyRt,
    pub(crate) end_user: Option<&'a str>,
    pub(crate) model: &'a str,
    pub(crate) route: Option<&'a str>,
    pub(crate) tags: &'a [String],
    /// Every deployment the route may send the request to.
    pub(crate) candidates: &'a [&'a Arc<DeploymentRt>],
    pub(crate) stream: bool,
    /// `H = 1 + h`: the attempts the route may keep outstanding at once.
    pub(crate) hedge: u32,
}

/// `H = 1 + h` from the effective `policies.resilience`: `h` is `hedging.max_extra` (default 1)
/// when hedging is enabled, else 0.
pub(crate) fn hedge_factor(resilience: &Value) -> u32 {
    let Some(h) = resilience.get("hedging") else {
        return 1;
    };
    if h.get("enabled").and_then(Value::as_bool) != Some(true) {
        return 1;
    }
    let extra = h.get("max_extra").and_then(Value::as_u64).unwrap_or(1).min(2);
    1 + u32::try_from(extra).unwrap_or(0)
}

/// The id reservations and ledger lines carry: the request's log id (a UUIDv7).
fn reservation_id(log_id: &str) -> ReservationId {
    let hex: String = log_id.chars().filter(char::is_ascii_hexdigit).collect();
    ReservationId(u128::from_str_radix(&hex, 16).unwrap_or(0))
}

/// The effective price of a call to `d`.
pub(crate) fn price_of(snap: &Snapshot, d: &DeploymentRt) -> Option<Arc<Priced>> {
    snap.pricing
        .price(d.provider.kind.as_str(), &d.base_model, &d.name)
}

fn header(name: &'static str, value: &str) -> Option<(HeaderName, HeaderValue)> {
    Some((HeaderName::from_static(name), HeaderValue::from_str(value).ok()?))
}

fn admission_headers(
    snap: &Snapshot,
    endpoint: &Endpoint,
    r: &Reservation,
) -> Vec<(HeaderName, HeaderValue)> {
    let mut out = Vec::new();
    if let Some((policy, current)) = r.ietf_headers() {
        out.extend(header(headers::RATELIMIT_POLICY, &policy));
        out.extend(header(headers::RATELIMIT, &current));
    }
    let openai_route = matches!(
        endpoint.dialect,
        Dialect::OpenAiChat
            | Dialect::OpenAiCompletions
            | Dialect::OpenAiResponses
            | Dialect::OpenAiEmbeddings
            | Dialect::OpenAiImages
            | Dialect::OpenAiAudio
            | Dialect::OpenAiModerations
    );
    let anthropic_route = matches!(endpoint.dialect, Dialect::AnthropicMessages);
    if r.headers.dialect && anthropic_route {
        for (name, value) in headers::anthropic(&r.rate_limit_states(), r.admitted_at) {
            if let (Ok(n), Ok(v)) = (
                HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_str(&value),
            ) {
                out.push((n, v));
            }
        }
    }
    if r.headers.dialect && openai_route {
        for (name, value) in headers::openai(&r.rate_limit_states()) {
            if let (Ok(n), Ok(v)) = (
                HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_str(&value),
            ) {
                out.push((n, v));
            }
        }
    }
    if let Some(v) = r.reservation_header() {
        out.extend(header(headers::RESERVATION, &v));
    }
    if snap.limits.budgets.expose_headers
        && let Some((remaining, reset)) = r.budget_headers()
    {
        out.extend(header(headers::BUDGET_REMAINING, &remaining));
        if let Some(reset) = reset {
            out.extend(header(headers::BUDGET_RESET, &reset));
        }
    }
    out
}

/// Lists degraded limits and budgets, with their causes, in the log record.
fn record_degraded(log: &crate::reqlog::Draft, d: Degraded) {
    let causes = d.causes();
    if causes.is_empty() {
        return;
    }
    log.update(|r| {
        for (component, cause) in causes {
            if !r.degraded.iter().any(|(c, _)| *c == component) {
                r.degraded.push((component, Some(cause)));
            }
        }
    });
}

/// What admission tells the later stages.
#[derive(Debug)]
pub(crate) struct Admitted {
    /// `E_in`, the input estimate settlement falls back to.
    pub(crate) input: u64,
    /// With `output: clamp`, the output cap every upstream request carries.
    pub(crate) clamp: Option<u64>,
    /// A refusal by a budget that lets cache hits through: a cache miss is refused with it.
    pub(crate) deferred: Option<antirion_limits::bypass::DeferredRefusal>,
}

/// The response header naming the cap a clamped request was lowered to.
const MAX_OUTPUT_CLAMPED: &str = "x-antirion-max-output-clamped";

/// Caps the request body's output at `cap` (`output: clamp`); a lowered client value is reported
/// in `x-antirion-max-output-clamped`. Whether the body changed.
pub(crate) fn clamp(slot: &Slot, endpoint: &Endpoint, body: &mut Value, cap: u64) -> bool {
    let (changed, lowered) = prompt::clamp(endpoint, body, cap);
    if lowered {
        slot.lock().headers.push((
            HeaderName::from_static(MAX_OUTPUT_CLAMPED),
            HeaderValue::from(cap),
        ));
    }
    changed
}

/// Stage `admit`: the estimate, the applicable counters and their reservation, all or nothing.
pub(crate) async fn admit(cx: &Ctx, slot: &Slot, a: &Admission<'_>) -> Result<Admitted, GatewayError> {
    let started = std::time::Instant::now();
    let snap = Arc::clone(&cx.snap);
    let policies = &snap.limits;
    let settings = &policies.limits.estimation;
    let raw = match a.parsed {
        Parsed::Json { raw, .. } => raw.as_ref(),
        Parsed::Multipart { .. } => &[][..],
    };
    let body = a.parsed.doc().unwrap_or(&Value::Null);
    let max_output = prompt::output_cap(a.endpoint, body, settings.max_output_tokens_ceiling)
        .map_err(|e| output_cap_error(&e.field))?;
    let mut f = prompt::body_facts(body);
    let formulas: Vec<_> = a
        .candidates
        .iter()
        .map(|d| antirion_limits::imagetokens::ImageFormula::of_provider(d.provider.kind.as_str()))
        .collect();
    let (known, counted) = prompt::image_tokens(&f.inline_images, &formulas, settings.image_tokens_default);
    f.media.images = f.media.images.saturating_sub(counted);
    f.media.known_image_tokens = known;
    let facts = PromptFacts {
        texts: &f.texts,
        messages: f.messages,
        tools_json: f.tools.as_deref(),
        body_bytes: (raw.len() as u64).saturating_sub(f.media_bytes),
        media: f.media,
        max_output,
        reasoning_outside_cap: 0,
        choices: prompt::choices(a.endpoint, body),
        generates: prompt::generates(a.endpoint),
    };
    let candidates: Vec<CandidateFacts> = a
        .candidates
        .iter()
        .map(|d| CandidateFacts {
            tokenizer: d
                .capabilities
                .get("tokenizer")
                .0
                .as_str()
                .and_then(Tokenizer::named),
            max_output_tokens: d.capabilities.get("max_output_tokens").0.as_u64(),
        })
        .collect();
    let est = estimate(&facts, &candidates, settings);
    let asked = units::of_request(a.endpoint, a.parsed);
    cx.log.update(|r| {
        r.units.characters_in = asked.fixed.characters_in;
        r.units.audio_ms_in = asked.audio_ms.unwrap_or(0);
        r.images_requested.clone_from(&asked.images);
    });
    let prices: Vec<Option<Arc<Priced>>> = a.candidates.iter().map(|d| price_of(&snap, d)).collect();
    let price_refs: Vec<Option<&Price>> = prices.iter().map(|p| p.as_deref().map(|p| &**p)).collect();
    let key = a.key;
    let line = LineFacts {
        organization: key.organization.clone(),
        team: key.team.clone(),
        project: key.project.clone(),
        key: key.name.clone(),
        user: key.owner.clone(),
        end_user: a.end_user.map(str::to_owned),
        tags: a.tags.to_vec(),
        model: a.model.to_owned(),
        route: a.route.map(str::to_owned),
        endpoint: a.endpoint.id,
        stream: a.stream,
        ip: cx.client_ip,
        multiplier: snap
            .pricing
            .multiplier(&key.organization, &key.team, &key.project, &key.name),
        priority_class: key.priority_class.clone(),
    };
    let input = AdmitInput {
        uid: reservation_id(&cx.log_id),
        attrs: line.attrs(),
        estimate: est,
        hedge: a.hedge,
        incurs: Incurred::new(f.cache_markers, f.audio_in, f.image_in, f.video_in, false, false),
        candidates: &price_refs,
        fixed: asked.fixed,
        multiplier: line.multiplier,
        now: cx.rt.now(),
    };
    let planned = plan(policies, &input)?;
    if cx.rt.ledger.rejects_budgeted() && planned.holds.iter().any(|h| h.info.unit.is_budget()) {
        let mut e = GatewayError::with_reason(
            crate::reasons::LEDGER_UNAVAILABLE,
            Stage::Admit,
            Message::Fixed("Spend cannot be recorded right now, so budgeted requests are refused."),
        );
        e.retry_after = Some(std::time::Duration::from_secs(1));
        return Err(e);
    }
    let applied = !planned.holds.is_empty();
    let admitted = cx.rt.accounts.admit_deferring(planned).await;
    if let Err(e) = &admitted {
        count_refusal(&cx.rt.metrics, e);
    }
    if applied {
        crate::reqlog::span(
            cx,
            crate::reqlog::InternalSpan::new("antirion.admit".to_owned(), started, std::time::Instant::now()),
        );
    }
    let (reservation, deferred) = admitted?;
    record_degraded(&cx.log, slot.degrade(reservation.degraded));
    let mut st = slot.lock();
    st.headers = admission_headers(&snap, a.endpoint, &reservation);
    st.pending = Some(Pending {
        rt: Arc::clone(&cx.rt),
        snap: Arc::clone(&snap),
        request_id: cx.log_id.clone(),
        reservation: Some(reservation),
        facts: line,
        attempts: 0,
    });
    Ok(Admitted {
        input: est.input,
        clamp: est.clamp_to,
        deferred,
    })
}

/// Counts a rate-limit refusal in `antirion_rate_limited_total` by the refusing declaration's level
/// and name.
pub(crate) fn count_refusal(m: &antirion_obs::Metrics, e: &GatewayError) {
    if !matches!(
        e.code,
        antirion_error::Code::RateLimited | antirion_error::Code::RequestExceedsLimit
    ) {
        return;
    }
    let Some(limit) = e.limit.as_deref() else {
        return;
    };
    let field = |k: &str| limit.get(k).and_then(Value::as_str).unwrap_or_default();
    m.add(
        antirion_obs::Fam::RateLimited,
        &[field("level"), field("name")],
        1.0,
    );
}

/// Takes the holds of the route-tree nodes on an attempt's position (their route paths) and of its
/// deployment; `Err` with the wait after which they could fit, when they do not.
pub(crate) async fn admit_attempt(
    slot: &Slot,
    nodes: &[String],
    deployment: &str,
) -> Result<(), Option<std::time::Duration>> {
    let Some(mut p) = slot.take() else {
        return Ok(());
    };
    let result = attempt_holds(&mut p, nodes, deployment).await;
    slot.put(p);
    result
}

async fn attempt_holds(
    p: &mut Pending,
    nodes: &[String],
    deployment: &str,
) -> Result<(), Option<std::time::Duration>> {
    let Some(r) = p.reservation.as_mut() else {
        return Ok(());
    };
    let price = p
        .snap
        .providers
        .deployments
        .get(deployment)
        .and_then(|d| price_of(&p.snap, d));
    let candidates = [price.as_deref().map(|p| &**p)];
    let input = AdmitInput {
        uid: r.uid,
        attrs: p.facts.attrs(),
        estimate: r.estimate,
        hedge: r.hedge,
        incurs: Incurred::default(),
        candidates: &candidates,
        fixed: antirion_limits::cost::FixedUnits::default(),
        multiplier: p.facts.multiplier,
        now: r.admitted_at,
    };
    let scope = Scope::Attempt {
        nodes,
        deployment: Some(deployment),
    };
    let Ok((holds, _)) = plan_holds(&p.snap.limits, &input, scope) else {
        return Ok(());
    };
    if holds.is_empty() {
        return Ok(());
    }
    p.attempts = p.attempts.saturating_add(1);
    p.rt.accounts
        .admit_attempt(r, p.attempts, holds)
        .await
        .map_err(|refused| refused.wait)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hedging_multiplies_by_the_attempts_outstanding() {
        use serde_json::json;
        assert_eq!(hedge_factor(&json!({})), 1);
        assert_eq!(
            hedge_factor(&json!({"hedging": {"enabled": false, "max_extra": 2}})),
            1
        );
        assert_eq!(hedge_factor(&json!({"hedging": {"enabled": true}})), 2);
        assert_eq!(
            hedge_factor(&json!({"hedging": {"enabled": true, "max_extra": 2}})),
            3
        );
    }

    #[test]
    fn reservation_ids_come_from_the_log_id() {
        assert_eq!(
            reservation_id("01920000-0000-7000-8000-000000000001"),
            ReservationId(0x0192_0000_0000_7000_8000_0000_0000_0001)
        );
    }
}
