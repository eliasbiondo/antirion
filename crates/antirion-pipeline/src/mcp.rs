//! MCP gateways and A2A agents: the head checks made before authentication, authentication (with the
//! OAuth challenge of protected gateways), the body, the hand-off to the MCP crate and the request-log
//! record.

use std::sync::Arc;

use antirion_core::{Peer, Stage};
use antirion_dialects::{Endpoint, Kind};
use antirion_error::{Code, GatewayError, Message};
use antirion_mcp::{CallLog, Caller, Env, Hooks, McpPath};
use bytes::Bytes;
use http::header::WWW_AUTHENTICATE;
use http::request::Parts;
use http::{Method, Response};
use hyper::body::Incoming;
use serde_json::Value;

use crate::auth::Authenticated;
use crate::ctx::{Body, Ctx};
use crate::reqlog;

pub(crate) fn caller(cx: &Ctx, authed: &Authenticated) -> Caller {
    let key = &authed.key;
    let tags = cx
        .snap
        .identity
        .chain(key)
        .iter()
        .flatten()
        .flat_map(|t| t.tags.iter().cloned())
        .chain(key.tags.iter().cloned())
        .chain(authed.controls.tags.iter().cloned())
        .collect();
    Caller {
        principal: key.uid.to_hex(),
        key_id: key.uid.to_hex(),
        key_name: key.name.clone(),
        organization: key.organization.clone(),
        team: key.team.clone(),
        project: key.project.clone(),
        tags,
        end_user: authed.end_user.clone(),
        jwt: authed.claims.clone(),
    }
}

/// The refusal of a JWT whose `aud` names none of an OAuth-protected gateway's audiences. It is an
/// authentication failure: the body is never read, so the JSON-RPC error answers a null id.
fn wrong_audience(env: &Env, path: &McpPath, authed: &Authenticated) -> Option<GatewayError> {
    let g = path.gateway()?;
    let presented = authed.token_audiences.as_ref()?;
    let accepted = antirion_mcp::audiences(&env.snap, g, &env.public_base)?;
    if presented.iter().any(|a| accepted.contains(a)) {
        return None;
    }
    Some(crate::auth::unauthenticated(
        antirion_identity::reasons::INVALID_TOKEN,
        "The token's audience is not this MCP resource.",
    ))
}

/// Counts and announces the tool definitions that changed upstream while this request read lists.
pub(crate) fn report_definition_changes(cx: &Ctx) {
    for c in cx.rt.state.mcp.take_definition_changes() {
        cx.rt.metrics.add(
            antirion_obs::Fam::McpToolDefinitionChanges,
            &[&c.server, c.change],
            1.0,
        );
        cx.rt.send_alert(
            &cx.snap,
            None,
            "mcp.tool_definition_changed",
            serde_json::json!({
                "server": c.server,
                "tool": c.tool,
                "change": c.change,
                "old_fingerprint": c.old_fingerprint,
                "new_fingerprint": c.new_fingerprint,
            }),
        );
    }
}

/// The `sessionId` query parameter of an HTTP+SSE message.
fn session_param(query: Option<&str>) -> Option<String> {
    query?
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == "sessionId")
        .map(|(_, v)| v.to_owned())
        .filter(|v| !v.is_empty())
}

/// The answer of an authorization-server helper: unknown paths are 404, failures their error.
fn helper_answer(cx: &Ctx, helped: antirion_mcp::Helped) -> Response<Body> {
    match helped {
        antirion_mcp::Helped::Answer(mut r) => {
            cx.finish(&mut r, false);
            r
        }
        antirion_mcp::Helped::NotFound => cx.error(&crate::accept::not_found(
            antirion_error::reason::hygiene::UNKNOWN_ENDPOINT,
        )),
        antirion_mcp::Helped::Failed(e) => cx.error(&e),
    }
}

fn finish(cx: &Ctx, mut resp: Response<Body>) -> Response<Body> {
    cx.finish(&mut resp, false);
    reqlog::finish(cx, resp)
}

/// Copies what the MCP crate learned into the request-log record.
fn record(cx: &Ctx, path: &McpPath, log: &CallLog) {
    let facts = log.facts();
    cx.log.update(|r| {
        match path {
            McpPath::Gateway(_) | McpPath::SseStream(_) | McpPath::SseMessage(..) => {
                r.mcp = Some(facts.mcp_json());
                r.mcp_telemetry = Some(crate::reqlog::mcptel::McpTelemetry::mcp(&facts));
            }
            McpPath::Agent(_) | McpPath::AgentCard(_) => {
                r.a2a = Some(facts.a2a_json());
                r.mcp_telemetry = Some(crate::reqlog::mcptel::McpTelemetry::a2a(&facts));
            }
        }
        if let Some(e) = &facts.error {
            r.fail(e);
        }
    });
}

pub(crate) async fn serve(
    cx: &mut Ctx,
    parts: &Parts,
    incoming: Incoming,
    endpoint: &Endpoint,
) -> Response<Body> {
    let Some(egress) = cx.rt.mcp_egress(&cx.snap, &cx.request_id) else {
        return cx.error(&GatewayError::new(
            Code::Internal,
            Stage::Accept,
            Message::Fixed("The egress TLS profile is not available."),
        ));
    };
    let security = &cx.snap.security;
    let trusted_peer = parts
        .extensions
        .get::<Peer>()
        .is_some_and(|p| security.client_ip.trusts_peer(p.addr.ip()));
    let public_url = cx
        .rt
        .statics
        .doc
        .pointer("/server/public_url")
        .and_then(Value::as_str);
    let log = Arc::new(CallLog::default());
    let mut env = Env {
        public_base: antirion_mcp::public_base(public_url, &parts.headers, cx.secure, trusted_peer),
        snap: Arc::clone(&cx.snap.mcp),
        state: Arc::clone(&cx.rt.state.mcp),
        egress,
        now: cx.rt.now(),
        request_id: cx.request_id.clone(),
        tasks: cx.rt.tasks.clone(),
        shutdown: cx.rt.shutdown.clone(),
        hooks: Hooks::default(),
        log: Arc::clone(&log),
    };
    let name = endpoint
        .path_param
        .clone()
        .unwrap_or_else(|| "default".to_owned());
    let path = match (endpoint.kind, endpoint.id) {
        (Kind::McpMetadata, "mcp_authorization_server") => {
            let helped = antirion_mcp::authorization_server(&env, &name).await;
            return helper_answer(cx, helped);
        }
        (Kind::McpMetadata, "mcp_register") => {
            let body = match crate::parse::read(cx, parts, incoming, 64 << 10).await {
                Ok(b) => b,
                Err(e) => return cx.error(&e),
            };
            let helped = antirion_mcp::register(&env, &name, body).await;
            return helper_answer(cx, helped);
        }
        (Kind::McpMetadata, _) => {
            return match antirion_mcp::metadata(&env.snap, &name, &env.public_base) {
                Some(mut r) => {
                    cx.finish(&mut r, false);
                    r
                }
                None => cx.error(&crate::accept::not_found(
                    antirion_error::reason::hygiene::UNKNOWN_ENDPOINT,
                )),
            };
        }
        (Kind::A2a, "a2a_card") => McpPath::AgentCard(name),
        (Kind::A2a, _) => McpPath::Agent(name),
        (_, "mcp_sse") => McpPath::SseStream(name),
        (_, "mcp_message") => McpPath::SseMessage(name, session_param(parts.uri.query())),
        _ => McpPath::Gateway(name),
    };
    if let Some(r) = antirion_mcp::precheck(&env, &path, &parts.headers) {
        return finish(cx, r);
    }
    let challenge = |e: &mut GatewayError| {
        if let Some(g) = path.gateway()
            && let Some(v) = antirion_mcp::challenge(&env.snap, g, parts.uri.path(), &env.public_base)
            && let Some(h) = e.extra_headers.as_mut()
        {
            h.insert(WWW_AUTHENTICATE, v);
        }
    };
    let caller = if antirion_mcp::a2a_requires_credential(&env.snap, &path) {
        let auth_start = std::time::Instant::now();
        let authed = crate::auth::run(cx, parts, endpoint)
            .await
            .and_then(|a| wrong_audience(&env, &path, &a).map_or(Ok(a), Err))
            .and_then(|a| crate::access::require_end_user(&a, a.end_user.as_deref()).map(|()| a));
        match authed {
            Ok(authed) => {
                let span = (auth_start, std::time::Instant::now());
                let snap = Arc::clone(&cx.snap);
                crate::trace::scope(cx, &crate::policy::Levels::new(&snap, &authed.key, "", None));
                cx.log
                    .begin(reqlog::Record::start(cx, parts, endpoint, &authed, span));
                let caller = caller(cx, &authed);
                if authed.claims.is_some() {
                    env.egress.subject = parts
                        .headers
                        .get(http::header::AUTHORIZATION)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
                        .map(|t| Arc::from(t.trim()));
                }
                if let Some(g) = crate::mcpguard::McpGuard::new(cx, &authed.key, authed.end_user.as_deref()) {
                    env.hooks.guardrails = Some(Arc::new(g));
                }
                env.hooks.limits = Some(Arc::new(crate::mcplimits::McpLimits {
                    rt: Arc::clone(&cx.rt),
                    snap: Arc::clone(&cx.snap),
                    key: Arc::clone(&authed.key),
                    end_user: authed.end_user.clone(),
                    tags: caller.tags.clone(),
                    ip: cx.client_ip,
                    log_id: cx.log_id.clone(),
                    family: endpoint.family,
                }));
                Some(caller)
            }
            Err(mut e) => {
                challenge(&mut e);
                reqlog::begin_unauthenticated(cx, parts, endpoint, auth_start);
                return reqlog::finish(cx, cx.error(&e));
            }
        }
    } else {
        None
    };
    let limit = antirion_mcp::body_limit(&env.snap, &path, cx.snap.security.max_body);
    if let Some(r) = antirion_mcp::before_body(&env, &path, &parts.method, &parts.headers) {
        record(cx, &path, &log);
        return finish(cx, r);
    }
    let body = if parts.method == Method::POST {
        match crate::parse::read(cx, parts, incoming, limit).await {
            Ok(b) => b,
            Err(e) => return reqlog::finish(cx, cx.error(&e)),
        }
    } else {
        Bytes::new()
    };

    let token = matches!(path, McpPath::Gateway(_))
        .then(|| parts.headers.get("mcp-session-id"))
        .flatten()
        .and_then(|v| v.to_str().ok());
    let unchecked = match token {
        Some(t) => !cx.rt.check_shared_revocation(t).await,
        None => false,
    };
    let mut resp = antirion_mcp::serve(
        env,
        path.clone(),
        parts.method.clone(),
        parts.headers.clone(),
        body,
        caller,
    )
    .await;
    if let Some(t) = token
        && parts.method == Method::DELETE
        && resp.status() == http::StatusCode::NO_CONTENT
    {
        cx.rt.share_revocation(t).await;
    }
    if unchecked {
        crate::mark_degraded(&["session_store"], resp.headers_mut());
    }
    report_definition_changes(cx);
    record(cx, &path, &log);
    finish(cx, resp)
}
