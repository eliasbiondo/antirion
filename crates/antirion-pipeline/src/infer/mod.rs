//! Inference requests: `parse`, `resolve`, `admit`, input guardrails, the exact cache, then
//! `route` and the attempt loop.

use std::sync::Arc;

use antirion_core::Stage;
use antirion_dialects::Endpoint;
use antirion_dialects::xlate::warn::Refusal;
use antirion_error::{Code, GatewayError, Message};
use antirion_providers::DeploymentRt;
use http::header::CONTENT_TYPE;
use http::request::Parts;
use http::{Response, StatusCode};
use hyper::body::Incoming;
use serde_json::Value;

use crate::account::{self, Admission, End};
use crate::attempt::{Done, Plan};
use crate::auth::Authenticated;
use crate::authorize::{self, Target};
use crate::cell::cell;
use crate::ctx::{Body, Ctx};
use crate::parse::{self, Parsed};
use crate::policy::{self, StreamPolicy};
use crate::reqlog::StreamFact;
use crate::{cache, caps, extend, guard, reasons};

mod facts;
pub(crate) mod model;

use facts::{accept_satisfied, alt_sse, input_tokens, max_output};
use model::{Resolution, body_end_user, requested_model, resolution, unserved};

pub(crate) async fn run(
    cx: &mut Ctx,
    parts: &Parts,
    incoming: Incoming,
    endpoint: &Endpoint,
    auth: &Authenticated,
) -> Response<Body> {
    let rt = Arc::clone(&cx.rt);
    let _in_flight = rt.in_flight_guard();
    if let Some(ip) = cx.client_ip {
        tracing::debug!(request_id = cx.request_id.as_str(), client_ip = %ip, "inference request");
    }
    let result = pipeline(cx, parts, incoming, endpoint, auth).await;
    cx.log.stage(Stage::Respond);
    let slot = cx.account.clone();
    let mut resp = match result {
        Ok(Done::Response(resp)) => {
            if cx.log.is_streaming() {
                resp
            } else {
                account::settle(&slot, &cx.log, End::Response(resp.status().as_u16())).await;
                let enabled = cx.snap.limits.pricing.report_in_body;
                account::report_in_body(enabled, endpoint, &slot, resp).await
            }
        }
        Ok(Done::Cached(resp)) => {
            account::settle(&slot, &cx.log, End::Cached).await;
            resp
        }
        Ok(Done::Cancelled) if rt.drain.is_cut() => {
            let resp = cx.error(&drain_deadline());
            account::settle(&slot, &cx.log, End::Cancelled).await;
            resp
        }
        Ok(Done::Cancelled) => {
            cx.log.update(|r| r.client_disconnected = true);
            account::settle(&slot, &cx.log, End::Cancelled).await;
            let mut resp = Response::new(crate::ctx::empty());
            *resp.status_mut() = StatusCode::from_u16(499).unwrap_or(StatusCode::BAD_REQUEST);
            resp
        }
        Err(e) => {
            let resp = cx.error(&e);
            account::settle(&slot, &cx.log, End::Failed).await;
            resp
        }
    };
    if cx.completions_as_chat {
        resp = crate::completions::respond(resp).await;
    }
    crate::fault::maybe_panic(cx, parts, Stage::Respond);
    if !cx.snap.obs.upstream_request_id_header {
        resp.headers_mut()
            .remove(antirion_core::headers::UPSTREAM_REQUEST_ID);
    }
    guard::finish(cx, &mut resp);
    for (name, value) in &cx.extra_headers {
        resp.headers_mut().insert(name.clone(), value.clone());
    }
    let streaming = cx.log.is_streaming();
    slot.decorate(&mut resp, streaming);
    cache::decorate(cx, &mut resp);
    if streaming && account::accepts_trailers(&parts.headers) {
        let names = if guard::announces_trailer(cx) {
            "x-antirion-cost, x-antirion-cost-details, x-antirion-guardrails, x-antirion-stream-end, x-antirion-overhead-ms"
        } else {
            "x-antirion-cost, x-antirion-cost-details, x-antirion-stream-end, x-antirion-overhead-ms"
        };
        resp.headers_mut()
            .insert(http::header::TRAILER, http::HeaderValue::from_static(names));
    }
    resp
}

/// The answer to a request the drain deadline cut before its response started.
fn drain_deadline() -> GatewayError {
    let mut e = GatewayError::with_reason(
        antirion_runtime::drain::reasons::DRAIN_DEADLINE,
        Stage::Respond,
        Message::Fixed("The node stopped before the request completed; retry on another node."),
    )
    .closing();
    e.retry_after = Some(std::time::Duration::from_secs(1));
    e
}

/// Takes an admission slot for the request, held until its response is complete.
async fn admission(cx: &Ctx, project: &str) -> Result<Option<()>, GatewayError> {
    use antirion_runtime::admission::{Refused, reasons as r};
    let refused = if cx.rt.admission.shedding() {
        cx.rt
            .metrics
            .add(antirion_obs::Fam::AdmissionRejected, &["memory_pressure"], 1.0);
        (
            r::MEMORY_PRESSURE,
            "The node is shedding load: its memory use is above the high watermark.",
        )
    } else {
        match cx.rt.admission.acquire(&cx.gone, project).await {
            Ok(permit) => {
                cx.log.hold_slot(permit);
                return Ok(Some(()));
            }
            Err(Refused::Gone) => return Ok(None),
            Err(Refused::QueueFull) => (r::QUEUE_FULL, "Every admission slot and queue position is taken."),
            Err(Refused::QueueTimeout) => (r::QUEUE_TIMEOUT, "No admission slot freed up in time."),
        }
    };
    let mut e = GatewayError::with_reason(refused.0, Stage::Admit, Message::Fixed(refused.1));
    e.retry_after = Some(cx.rt.admission.state().limits.queue_timeout);
    Err(e)
}

/// The body of a parsed JSON request after guardrails or extensions changed it.
fn rebuilt(v: Value) -> Option<Parsed> {
    let raw = bytes::Bytes::from(serde_json::to_vec(&v).ok()?);
    let skim = antirion_json::skim(&raw, &antirion_json::JsonLimits::default()).ok()?;
    Some(Parsed::json(raw, skim, Some(v)))
}

/// The target and resolution a budget downgrade sends the request to, or the given ones.
fn downgraded(
    cx: &mut Ctx,
    key: &antirion_runtime::identity::KeyRt,
    tags: &[String],
    end_user: Option<&str>,
    endpoint: &Endpoint,
    target: Target,
    resolved: Resolution,
) -> (Target, Resolution) {
    let attrs = antirion_limits::applicable::RequestAttrs {
        organization: &key.organization,
        team: &key.team,
        project: &key.project,
        key: &key.name,
        user: key.owner.as_deref(),
        end_user,
        model: Some(&resolved.model_name),
        route: resolved.policy_route.as_deref(),
        endpoint: endpoint.id,
        tags,
        ip: cx.client_ip,
        mcp_server: None,
        mcp_tool: None,
        a2a_agent: None,
        priority_class: None,
    };
    let Some(d) = crate::downgrade::decide(&cx.rt, &cx.snap, key, &attrs, cx.rt.now()) else {
        return (target, resolved);
    };
    let Ok(next) = resolution(&cx.snap, &d.to, &d.target) else {
        return (target, resolved);
    };
    if let Ok(v) = http::HeaderValue::from_str(&d.budget) {
        cx.extra_headers
            .push((http::HeaderName::from_static("x-antirion-budget-downgraded"), v));
    }
    let record = serde_json::json!({"budget": d.budget, "from": resolved.model_name, "to": next.model_name});
    cx.log.update(|r| r.budget_downgrade = Some(record));
    cx.rt.accounts.overlay().downgraded(d.touched);
    (d.target, next)
}

async fn pipeline(
    cx: &mut Ctx,
    parts: &Parts,
    incoming: Incoming,
    endpoint: &Endpoint,
    auth: &Authenticated,
) -> Result<Done, GatewayError> {
    let key = &auth.key;
    if cx.decide && !key.permissions.has(antirion_identity::KeyPermission::RouteDecide) {
        return Err(GatewayError::with_reason(
            antirion_identity::reasons::PERMISSION_REQUIRED,
            Stage::Authenticate,
            Message::Fixed("Asking for a routing decision needs the `route:decide` permission."),
        ));
    }
    cx.log.stage(Stage::Parse);
    let parsed = parse::run(cx, parts, incoming, endpoint).await?;
    crate::fault::maybe_panic(cx, parts, Stage::Parse);
    cx.log.stage(Stage::Resolve);
    let mut parsed = crate::files::resolve_in_body(cx, key, parsed).await?;
    if let Parsed::Json { doc, .. } = &parsed {
        crate::media::refuse_userinfo(doc.as_deref())?;
    }
    let turn = if endpoint.id == "responses"
        && endpoint.kind == antirion_dialects::Kind::Inference
        && endpoint.action.is_none()
    {
        Some(Box::pin(crate::respstate::prepare(cx, &key.project, &mut parsed)).await?)
    } else {
        None
    };
    let stateful = turn.as_ref().and_then(|t| t.stateful);
    let streams = parsed.stream() || endpoint.path_streams();
    if endpoint.upstream_path != Some("/audio/speech") && !accept_satisfied(parts, endpoint, streams) {
        return Err(GatewayError::new(
            Code::NotAcceptable,
            Stage::Parse,
            Message::Fixed("The Accept header allows no media type this endpoint can answer with."),
        ));
    }
    let json_array = endpoint.action == Some("streamGenerateContent") && !alt_sse(parts);
    let end_user = match &auth.end_user {
        Some(header) => Some(header.clone()),
        None => {
            let from_body = body_end_user(&parsed);
            cx.log.update(|r| r.end_user.clone_from(&from_body));
            from_body
        }
    };
    crate::access::require_end_user(auth, end_user.as_deref())?;
    let (requested, model_absent) = requested_model(cx, endpoint, &parsed)?;
    cx.log
        .update(|r| r.requested_model = Some(requested.chars().take(256).collect()));

    let snap = Arc::clone(&cx.snap);
    let target = authorize::resolve(&snap, key, &requested)?;
    cx.extra_headers = authorize::lifecycle(&target, key, &requested, cx.rt.now())?;
    let levels = snap.identity.access_levels(key);
    let allowed = |d: &DeploymentRt| authorize::deployment_allowed(&levels, d);
    let tags: Vec<String> = snap
        .identity
        .chain(key)
        .iter()
        .flatten()
        .flat_map(|t| t.tags.iter().cloned())
        .chain(key.tags.iter().cloned())
        .chain(auth.controls.tags.iter().cloned())
        .collect();
    let resolved = resolution(&snap, &requested, &target)?;
    let (mut target, resolved) = downgraded(cx, key, &tags, end_user.as_deref(), endpoint, target, resolved);
    let Resolution {
        mut model_name,
        policy_route,
        route: mut route_rt,
        mut alias,
        mut captures,
        known,
    } = resolved;
    let policies = policy::Levels::new(&snap, key, &model_name, policy_route.as_deref());
    let chat_endpoint;
    let endpoint = if endpoint.id == "completions"
        && crate::completions::enabled(&policies.effective("translation"))
        && !known
            .iter()
            .any(|d| allowed(d) && cell(endpoint, &parsed, d) == Some(crate::cell::Cell::Identity))
    {
        parsed = crate::completions::to_chat(&parsed)?;
        chat_endpoint = crate::completions::chat_endpoint().ok_or_else(|| {
            GatewayError::new(Code::Internal, Stage::Resolve, Message::Fixed("Internal error."))
        })?;
        cx.completions_as_chat = true;
        &chat_endpoint
    } else {
        endpoint
    };
    if streams && policies.effective("streaming").get("allow") == Some(&Value::Bool(false)) {
        return Err(GatewayError::with_reason(
            reasons::STREAMING_DISABLED,
            Stage::Resolve,
            Message::Fixed("streaming is not enabled for this model"),
        )
        .param("stream"));
    }
    let tool_loop = crate::toolloop::open(cx, parts, auth, endpoint, &policies).await;
    let client_streams = streams;
    if let Some(tl) = &tool_loop
        && let Some(mut body) = parsed.doc().cloned()
    {
        tl.offer(&mut body);
        parsed = rebuilt(body).ok_or_else(crate::toolloop::unusable)?;
    }
    let streams = streams && tool_loop.is_none();
    let serves = |d: &DeploymentRt| match cell(endpoint, &parsed, d) {
        Some(c) => stateful.is_none() || c == crate::cell::Cell::Identity,
        None => false,
    };
    let translation = policies.effective("translation");
    let header_policy = crate::xlate::header(&parts.headers)?;
    let strict = header_policy == Some(true)
        || (translation.get("on_lossy").and_then(Value::as_str) == Some("reject")
            && header_policy != Some(false));
    let fan_out = (endpoint.id == "chat_completions")
        .then(|| crate::fanout::requested(&translation, &parsed, streams, strict))
        .flatten();
    let fanned = fan_out.and_then(|_| crate::fanout::without_n(&parsed));
    let mut needs = caps::required(endpoint, &parsed);
    if fan_out.is_some() {
        needs.retain(|n| !matches!(n.flag, "n_multiple" | "max_n"));
    }
    let mut sizes = caps::Sizes {
        input: input_tokens(&parsed),
        output: max_output(endpoint, &parsed),
    };
    let servable: Vec<&Arc<DeploymentRt>> = known.iter().filter(|d| allowed(d) && serves(d)).collect();
    if !servable.is_empty() && servable.iter().all(|d| caps::output_unmet(d, sizes)) {
        sizes.output = None;
    }
    if !known.is_empty() && servable.is_empty() && known.iter().any(|d| allowed(d)) {
        if let Some(field) = stateful {
            return Err(crate::respstate::stateful_unsupported(field));
        }
        if (endpoint.id == "count_tokens" || endpoint.action == Some("input_tokens"))
            && let Parsed::Json { raw, .. } = &parsed
        {
            let responses = endpoint.dialect == antirion_dialects::Dialect::OpenAiResponses;
            return Ok(Done::Response(crate::local::count_estimate(cx, raw, responses)));
        }
        return Err(unserved(endpoint, &requested));
    }
    let feasibility = crate::xlate::resolve(
        &crate::xlate::Candidates {
            endpoint,
            parsed: fanned.as_ref().unwrap_or(&parsed),
            servable: &servable,
            needs: &needs,
            sizes,
            doc: &snap.config.doc,
            rt: &cx.rt,
        },
        || policies.effective("translation"),
        crate::xlate::header(&parts.headers)?,
        key,
    )?;
    let mut route_name = route_rt.tree.name.clone();
    cx.log.update(|r| {
        r.model = Some(model_name.clone());
        r.route = Some(route_name.clone());
    });
    crate::trace::scope(cx, &policies);
    crate::observe::content(cx, &policies, auth.controls.log_content)?;
    crate::observe::request(cx, &parts.headers, &parsed);

    crate::access::check(
        cx,
        &crate::access::Subject {
            parts,
            auth,
            end_user: end_user.as_deref(),
            requested: &requested,
            model: &model_name,
            tags: &tags,
        },
    )
    .await?;
    let via = match (&target, model_absent) {
        (_, true) => "default",
        (Target::Direct(_), _) => "provider",
        (Target::Model(r), _) => r.via,
    };
    if cx.decide {
        cx.log.stage(Stage::Admit);
        let counted = Parsed::json(
            bytes::Bytes::from_static(b"{}"),
            antirion_json::skim(b"{}", &antirion_json::JsonLimits::default()).map_err(|_| {
                GatewayError::new(Code::Internal, Stage::Admit, Message::Fixed("Internal error."))
            })?,
            Some(Value::Object(serde_json::Map::new())),
        );
        account::admit(
            cx,
            &cx.account,
            &Admission {
                endpoint,
                parsed: &counted,
                key,
                end_user: end_user.as_deref(),
                model: &model_name,
                route: policy_route.as_deref(),
                tags: &tags,
                candidates: &[],
                stream: false,
                hedge: 1,
            },
        )
        .await?;
        cx.log.stage(Stage::Route);
        let request = Request {
            parts,
            endpoint,
            parsed: &parsed,
            auth,
            end_user: end_user.as_deref(),
            requested: &requested,
            model_name: &model_name,
            tags: &tags,
            needs: &needs,
            sizes,
            native_only: stateful.is_some(),
            verdicts: feasibility.verdicts.clone(),
            resilience: policies.effective("resilience"),
        };
        let client_keys = crate::byok::ClientKeys::of(&parts.headers, key);
        let direct = match &target {
            Target::Direct(d) => Some(Arc::clone(d)),
            Target::Model(_) => None,
        };
        let body = Box::pin(crate::decision::decide(
            cx,
            crate::decision::Carried {
                turn: turn.as_ref(),
                xlate: feasibility.xlate.as_deref(),
                client_keys: client_keys.as_ref(),
            },
            &request,
            crate::decision::Subject {
                route: &route_rt,
                captures: captures.as_ref(),
                direct,
                via,
            },
        ))
        .await?;
        return Ok(Done::Response(crate::local::json_response(cx, &body)));
    }
    cx.log.stage(Stage::Admit);
    if Box::pin(admission(cx, &key.project)).await?.is_none() {
        return Ok(Done::Cancelled);
    }
    let admitted = account::admit(
        cx,
        &cx.account,
        &Admission {
            endpoint,
            parsed: &parsed,
            key,
            end_user: end_user.as_deref(),
            model: &model_name,
            route: policy_route.as_deref(),
            tags: &tags,
            candidates: &servable,
            stream: streams,
            hedge: account::hedge_factor(&policies.effective("resilience")),
        },
    )
    .await?;
    let e_in = admitted.input;
    let deferred = admitted.deferred;

    let effective = |member: &str| policies.effective(member);
    let guard_levels = policies.member("guardrails");
    let mut request_guard = guard::RequestGuard::new(
        cx,
        &guard::GuardInputs {
            levels: &guard_levels,
            merged: &effective("guardrails"),
            endpoint,
            model: &model_name,
            key,
            end_user: end_user.as_deref(),
            body: parsed.doc(),
        },
    );
    crate::select::apply(
        &parts.headers,
        &mut request_guard,
        &snap.guardrails,
        &effective("guardrails"),
    )?;
    if streams && request_guard.rejects_stream() {
        return Err(guard::stream_rejected());
    }
    let plugin_levels = policies.member("plugins");
    let hooks = extend::hooks_for(
        cx,
        &plugin_levels,
        &extend::HookInputs {
            parts,
            endpoint,
            auth,
            model: &requested,
            route: Some(route_name.as_str()),
            stream: streams,
            end_user: end_user.as_deref(),
        },
    );
    let edits_body = hooks.is_some() || !request_guard.attached.is_empty() || admitted.clamp.is_some();
    let mut body_value: Option<Value> = parsed.doc().filter(|_| edits_body).cloned();
    let key_body = parsed.shared_doc();
    let mut extra_headers = Vec::new();
    let mut changed = false;
    if let Some(h) = &hooks {
        let (end, c) = Box::pin(extend::request(cx, h, &mut body_value, &mut extra_headers)).await?;
        changed |= c;
        if let extend::RequestEnd::Respond(reply) = end {
            let wants_usage = body_value
                .as_ref()
                .and_then(|b| b.pointer("/stream_options/include_usage"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            return Ok(Done::Response(extend::respond(
                cx,
                endpoint,
                &reply,
                &requested,
                streams,
                wants_usage,
            )));
        }
        if streams {
            extend::skip_response(cx, h);
        }
    }
    cx.log.stage(Stage::GuardInput);
    let input = Box::pin(guard::input(cx, &request_guard, body_value.as_mut())).await?;
    changed |= input.changed;
    let alongside = input.alongside;
    request_guard.tokens = input.tokens;
    if parsed.member("template").is_some()
        && let Some(body) = body_value.as_ref().or(parsed.doc())
    {
        crate::parse::revalidate(cx, endpoint, body)?;
    }
    if let (Some(cap), Some(body)) = (admitted.clamp, body_value.as_mut()) {
        changed |= account::clamp(&cx.account, endpoint, body, cap);
    }
    let diverted_known;
    let (known, servable): (&[Arc<DeploymentRt>], Vec<&Arc<DeploymentRt>>) = match input.divert {
        Some(to) => {
            let resolved = snap.routing.resolve(&to).ok_or_else(|| unserved(endpoint, &to))?;
            let next = resolution(&snap, &to, &Target::Model(Box::new(resolved.clone())))?;
            target = Target::Model(Box::new(resolved));
            model_name = next.model_name;
            route_rt = next.route;
            alias = next.alias;
            captures = next.captures;
            route_name = route_rt.tree.name.clone();
            diverted_known = next.known;
            let usable: Vec<&Arc<DeploymentRt>> = diverted_known
                .iter()
                .filter(|d| allowed(d) && serves(d))
                .collect();
            if usable.is_empty() {
                return Err(unserved(endpoint, &to));
            }
            cx.log.update(|r| {
                r.model = Some(model_name.clone());
                r.route = Some(route_name.clone());
            });
            changed = body_value.is_some();
            (&diverted_known, usable)
        }
        None => (&known, servable),
    };
    let feasibility = match body_value.filter(|_| changed).and_then(rebuilt) {
        Some(p) => {
            parsed = p;
            crate::xlate::resolve(
                &crate::xlate::Candidates {
                    endpoint,
                    parsed: &parsed,
                    servable: &servable,
                    needs: &needs,
                    sizes,
                    doc: &snap.config.doc,
                    rt: &cx.rt,
                },
                || policies.effective("translation"),
                crate::xlate::header(&parts.headers)?,
                key,
            )?
        }
        None => feasibility,
    };

    let client_wants_usage = parsed
        .member("stream_options")
        .and_then(|v| v.get("include_usage").and_then(Value::as_bool))
        .unwrap_or(false);
    cx.log.stage(Stage::CacheLookup);
    let cache_policy = effective("cache");
    let cache_levels = policies.named("cache");
    let prompt_cache = cache::prompt::switch(&parts.headers, &cache_policy)?;
    cache::prompt::note_prefix(cx, &cache_policy, parsed.doc());
    let lookup = cache::lookup(
        cx,
        &cache::Inputs {
            parts,
            endpoint,
            body: key_body.as_deref(),
            key,
            end_user: end_user.as_deref(),
            model: &model_name,
            policy_route: policy_route.as_deref(),
            route: &route_name,
            reachable: known,
            policy: &cache_policy,
            guardrails: &request_guard.fingerprint(&snap.config.doc),
            tokenised: request_guard.tokens.is_some(),
            stream: streams,
            wants_usage: client_wants_usage,
            levels: &cache_levels,
            tags: &tags,
            metadata: auth.controls.metadata.as_ref(),
        },
    )
    .await?;
    let (cache_slot, items) = match lookup {
        cache::Lookup::Hit(resp) => {
            if !alongside.is_empty() {
                Box::pin(crate::alongside::checks(cx, &request_guard, &alongside)).await?;
            }
            return Ok(Done::Cached(resp));
        }
        cache::Lookup::Miss(slot) => (slot, None),
        cache::Lookup::Items(slot, found) => (None, Some((slot, found))),
    };
    let feasibility = match items
        .as_ref()
        .filter(|(_, found)| found.partial())
        .and_then(|(_, found)| rebuilt(found.reduced()))
    {
        Some(p) => {
            parsed = p;
            crate::xlate::resolve(
                &crate::xlate::Candidates {
                    endpoint,
                    parsed: &parsed,
                    servable: &servable,
                    needs: &needs,
                    sizes,
                    doc: &snap.config.doc,
                    rt: &cx.rt,
                },
                || policies.effective("translation"),
                crate::xlate::header(&parts.headers)?,
                key,
            )?
        }
        None => feasibility,
    };
    if let Some(d) = deferred {
        return Err(cx.rt.accounts.refuse_deferred(d));
    }
    let stream_policy = StreamPolicy::new(&snap, &effective("streaming"));
    let plan = Plan {
        endpoint,
        parsed: &parsed,
        requested: requested.clone(),
        model_absent,
        stream: streams,
        client_wants_usage,
        stream_policy,
        client_content_type: parts.headers.get(CONTENT_TYPE).cloned(),
        client_headers: &parts.headers,
        rules: crate::egress::rules(cx, key, policy_route.as_deref(), &model_name),
        input_estimate: e_in,
        cache: cache_slot,
        prompt_cache,
        openai_cache_key: cache::prompt::openai_key(&cache_policy, parsed.doc(), &key.project, &route_name),
        turn,
        xlate: feasibility.xlate,
        guard: (!request_guard.attached.is_empty()).then(|| Arc::new(request_guard)),
        hooks: hooks.map(Arc::new),
        extra_headers,
        post_commit: crate::postcommit::PostCommit::default(),
        client_keys: crate::byok::ClientKeys::of(&parts.headers, key),
        json_array,
        fan_out,
    };
    if plan.stream {
        cx.log.update(|r| {
            r.stream = Some(StreamFact {
                end_reason: "upstream_error",
                committed: false,
                relay_mode: "passthrough",
            });
        });
    }
    let request = Request {
        parts,
        endpoint,
        parsed: &parsed,
        auth,
        end_user: end_user.as_deref(),
        requested: &requested,
        model_name: &model_name,
        tags: &tags,
        needs: &needs,
        sizes,
        native_only: stateful.is_some(),
        verdicts: feasibility.verdicts,
        resilience: effective("resilience"),
    };
    let direct = match &target {
        Target::Direct(d) => Some(Arc::clone(d)),
        Target::Model(_) => None,
    };
    crate::fault::maybe_panic(cx, parts, Stage::Route);
    cx.log.stage(Stage::Route);
    if snap.routing.settings.dry_run_all {
        let body = Box::pin(crate::decision::decide(
            cx,
            crate::decision::Carried {
                turn: plan.turn.as_ref(),
                xlate: plan.xlate.as_deref(),
                client_keys: plan.client_keys.as_ref(),
            },
            &request,
            crate::decision::Subject {
                route: &route_rt,
                captures: captures.as_ref(),
                direct: direct.clone(),
                via,
            },
        ))
        .await?;
        let mut resp = crate::local::json_response(cx, &body);
        resp.headers_mut().insert(
            http::HeaderName::from_static("x-antirion-dry-run"),
            http::HeaderValue::from_static("true"),
        );
        return Ok(Done::Response(resp));
    }
    let call = crate::routing::run(
        cx,
        &plan,
        &request,
        &route_rt,
        alias.clone(),
        captures.as_ref(),
        direct.clone(),
    );
    let mut first = match plan.guard.as_deref().filter(|_| !alongside.is_empty()) {
        Some(g) => Box::pin(crate::alongside::run(cx, g, &alongside, call)).await,
        None => Box::pin(call).await,
    };
    let mut reasks = 0u32;
    while first.is_err() {
        match cx.guard.take_reask() {
            Some(n) if reasks < n => reasks += 1,
            _ => break,
        }
        cx.guard.forget_output();
        cx.log.update(|r| {
            let (usage, raw) = (r.usage, r.usage_raw.clone());
            if let Some(a) = r.attempts.last_mut() {
                a.usage = Some(usage);
                a.usage_raw = raw;
            }
        });
        let again = crate::routing::run(
            cx,
            &plan,
            &request,
            &route_rt,
            alias.clone(),
            captures.as_ref(),
            direct.clone(),
        );
        first = Box::pin(again).await;
    }
    let first = match (first?, &items) {
        (Done::Response(resp), Some((slot, found))) => {
            Done::Response(Box::pin(cache::items::finish(cx, slot.as_ref(), found, resp)).await)
        }
        (done, _) => done,
    };
    let Some(tl) = tool_loop else {
        return Ok(first);
    };
    let (guard, hooks, extra_headers) = (plan.guard.clone(), plan.hooks.clone(), plan.extra_headers.clone());
    let mut body = parsed.doc().cloned().unwrap_or(Value::Null);
    let mut done = first;
    let mut calls_made = 1u64;
    loop {
        let (head, raw, answer) = match crate::toolloop::read(done).await {
            Ok(read) => read,
            Err(other) => return Ok(*other),
        };
        let calls = match tl.decide(&answer) {
            crate::toolloop::Decision::Execute(calls) if calls_made < tl.max_iterations => calls,
            decision => {
                let capped = matches!(decision, crate::toolloop::Decision::Execute(_));
                let resp = tl.finish(head, &answer, raw, client_streams, client_wants_usage, capped);
                return Ok(Done::Response(resp));
            }
        };
        cx.log.update(|r| {
            let (usage, raw) = (r.usage, r.usage_raw.clone());
            if let Some(a) = r.attempts.last_mut() {
                a.usage = Some(usage);
                a.usage_raw = raw;
            }
        });
        let results = tl.execute(&calls).await;
        crate::mcp::report_definition_changes(cx);
        tl.append(&mut body, &answer, &calls, &results);
        let next = rebuilt(body.clone()).ok_or_else(crate::toolloop::unusable)?;
        let feasibility = crate::xlate::resolve(
            &crate::xlate::Candidates {
                endpoint,
                parsed: &next,
                servable: &servable,
                needs: &needs,
                sizes,
                doc: &snap.config.doc,
                rt: &cx.rt,
            },
            || policies.effective("translation"),
            crate::xlate::header(&parts.headers)?,
            key,
        )?;
        let plan = Plan {
            endpoint,
            parsed: &next,
            requested: requested.clone(),
            model_absent,
            stream: false,
            client_wants_usage,
            stream_policy: StreamPolicy::new(&snap, &effective("streaming")),
            client_content_type: parts.headers.get(CONTENT_TYPE).cloned(),
            client_headers: &parts.headers,
            rules: crate::egress::rules(cx, key, policy_route.as_deref(), &model_name),
            input_estimate: e_in,
            cache: None,
            prompt_cache: None,
            openai_cache_key: None,
            turn: None,
            xlate: feasibility.xlate,
            guard: guard.clone(),
            hooks: hooks.clone(),
            extra_headers: extra_headers.clone(),
            post_commit: crate::postcommit::PostCommit::default(),
            client_keys: crate::byok::ClientKeys::of(&parts.headers, key),
            json_array: false,
            fan_out: None,
        };
        let request = Request {
            parts,
            endpoint,
            parsed: &next,
            auth,
            end_user: end_user.as_deref(),
            requested: &requested,
            model_name: &model_name,
            tags: &tags,
            needs: &needs,
            sizes,
            native_only: false,
            verdicts: feasibility.verdicts,
            resilience: effective("resilience"),
        };
        let call = crate::routing::run(
            cx,
            &plan,
            &request,
            &route_rt,
            alias.clone(),
            captures.as_ref(),
            direct.clone(),
        );
        done = Box::pin(call).await?;
        calls_made += 1;
    }
}

/// The facts of a request the routing stage reads.
pub(crate) struct Request<'a> {
    pub(crate) parts: &'a Parts,
    pub(crate) endpoint: &'a Endpoint,
    pub(crate) parsed: &'a Parsed,
    pub(crate) auth: &'a Authenticated,
    pub(crate) end_user: Option<&'a str>,
    pub(crate) requested: &'a str,
    pub(crate) model_name: &'a str,
    pub(crate) tags: &'a [String],
    pub(crate) needs: &'a [caps::Need],
    pub(crate) sizes: caps::Sizes,
    /// A stateful Responses field is present: only a deployment that serves Responses natively
    /// can serve the request.
    pub(crate) native_only: bool,
    /// Each servable candidate's translation verdict from `resolve` (`None` when feasible).
    pub(crate) verdicts: std::collections::HashMap<String, Option<Refusal>>,
    /// The merged `policies.resilience` above the route's nodes.
    pub(crate) resilience: Value,
}
