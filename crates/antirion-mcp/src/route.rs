//! Requests that address one item and so one server: `tools/call`, `prompts/get`, `resources/read`,
//! subscriptions and completion. The owner is found from the prefix (or, unprefixed, from the lists),
//! the item must exist and be visible, and only the owner is contacted.

use std::sync::Arc;

use antirion_core::Stage;
use antirion_error::Message;
use http::Response;
use serde_json::{Value, json};

use crate::compile::{ServerInfo, ServerRt};
use crate::hooks::{CallFacts, Counted};
use crate::http::Env;
use crate::http::Req;
use crate::lists::{self, Kind, Mode};
use crate::names;
use crate::reasons;
use crate::relay::{ModernShape, Shaper};
use crate::rpc::Fail;
use crate::select::{ItemKind, Selection};
use crate::upstream::{Answer, UpError};
use crate::{Body, relay, respond};

fn unknown_tool(name: &str) -> Fail {
    Fail::protocol(
        -32602,
        200,
        reasons::UNKNOWN_TOOL,
        Stage::Resolve,
        Message::built(format!("Unknown tool: {name}")),
    )
}

fn unknown_prompt(name: &str) -> Fail {
    Fail::protocol(
        -32602,
        200,
        reasons::UNKNOWN_PROMPT,
        Stage::Resolve,
        Message::built(format!("Unknown prompt: {name}")),
    )
}

pub(crate) fn unknown_resource(uri: &str, modern: bool) -> Fail {
    Fail::protocol(
        if modern { -32602 } else { -32002 },
        200,
        reasons::UNKNOWN_RESOURCE,
        Stage::Resolve,
        Message::Fixed("Resource not found"),
    )
    .data("uri", Value::from(uri))
}

/// The servers a request may route to: the selection, limited to the session's servers.
fn scope(sel: &Selection, mode: &Mode) -> Vec<Arc<ServerRt>> {
    match mode.servers() {
        Some(names) => sel
            .servers
            .iter()
            .filter(|s| names.contains(&s.name))
            .cloned()
            .collect(),
        None => sel.servers.clone(),
    }
}

/// The routed target: the owning server, the upstream item and the rewritten params.
struct Target {
    server: Arc<ServerRt>,
    item: String,
    params: Value,
}

/// Whether `list` holds `item` (for resources, also through a template's literal prefix).
fn holds(kind: Kind, list: &[Value], item: &str) -> Option<Value> {
    let key = kind.item_key();
    if let Some(found) = list
        .iter()
        .find(|i| i.get(key).and_then(Value::as_str) == Some(item))
    {
        return Some(found.clone());
    }
    None
}

async fn has(
    req: &Req,
    mode: &Mode,
    server: &Arc<ServerRt>,
    kind: Kind,
    item: &str,
    fresh: bool,
) -> Result<Option<Value>, UpError> {
    let e = lists::entry(req, mode, server, kind, fresh).await?;
    if let Some(found) = holds(kind, &e.items, item) {
        return Ok(Some(found));
    }
    if kind == Kind::Resources {
        let t = lists::entry(req, mode, server, Kind::Templates, fresh).await?;
        let by_template = t.items.iter().any(|tpl| {
            tpl.get("uriTemplate")
                .and_then(Value::as_str)
                .and_then(|u| u.split('{').next())
                .is_some_and(|literal| !literal.is_empty() && item.starts_with(literal))
        });
        if by_template {
            return Ok(Some(json!({"uri": item})));
        }
    }
    Ok(None)
}

/// Checks that `item` exists on `server`, refreshing the cached list once before giving up.
async fn exists(
    req: &Req,
    mode: &Mode,
    server: &Arc<ServerRt>,
    kind: Kind,
    item: &str,
) -> Result<Option<Value>, Fail> {
    for fresh in [false, true] {
        match has(req, mode, server, kind, item, fresh).await {
            Ok(Some(def)) => return Ok(Some(def)),
            Ok(None) => {}
            Err(e) => return Err(e.fail(&server.name)),
        }
    }
    Ok(None)
}

/// Finds the owner of an unprefixed item: exactly one server in scope must offer it.
async fn owner(
    req: &Req,
    mode: &Mode,
    servers: &[Arc<ServerRt>],
    kind: Kind,
    item: &str,
) -> Result<Option<Arc<ServerRt>>, Fail> {
    for fresh in [false, true] {
        let mut owners = Vec::new();
        for s in servers {
            match has(req, mode, s, kind, item, fresh).await {
                Ok(Some(_)) => owners.push(Arc::clone(s)),
                Ok(None) => {}
                Err(e) => return Err(e.fail(&s.name)),
            }
        }
        match owners.len() {
            0 => {}
            1 => return Ok(owners.pop()),
            _ => return Ok(None),
        }
    }
    Ok(None)
}

async fn resolve_named(
    req: &Req,
    sel: &Selection,
    mode: &Mode,
    kind: Kind,
    client: &str,
) -> Result<Option<(Arc<ServerRt>, String, Option<Value>)>, Fail> {
    let item_kind = if kind == Kind::Tools {
        ItemKind::Tool
    } else {
        ItemKind::Prompt
    };
    let servers = scope(sel, mode);
    let (server, upstream) = if req.gw.prefixed {
        let Some((prefix, upstream)) = names::split_name(client) else {
            return Ok(None);
        };
        let Some(server) = servers.iter().find(|s| s.prefix == prefix).cloned() else {
            return Ok(None);
        };
        (server, upstream.to_owned())
    } else {
        match owner(req, mode, &servers, kind, client).await? {
            Some(s) => (s, client.to_owned()),
            None => return Ok(None),
        }
    };
    let state = &req.env.state;
    if !sel.visible(state, &server, item_kind, &upstream, client, None) {
        return Ok(None);
    }
    let def = exists(req, mode, &server, kind, &upstream).await?;
    match def {
        Some(d) if sel.visible(state, &server, item_kind, &upstream, client, Some(&d)) => {
            Ok(Some((server, upstream, Some(d))))
        }
        _ => Ok(None),
    }
}

async fn resolve_uri(
    req: &Req,
    sel: &Selection,
    mode: &Mode,
    client: &str,
    check: bool,
) -> Result<Option<(Arc<ServerRt>, String)>, Fail> {
    let servers = scope(sel, mode);
    let (server, upstream) = if req.gw.prefixed {
        let Some((prefix, upstream)) = names::split_uri(client) else {
            return Ok(None);
        };
        let Some(server) = servers.iter().find(|s| s.prefix == prefix).cloned() else {
            return Ok(None);
        };
        (server, upstream)
    } else if check {
        match owner(req, mode, &servers, Kind::Resources, client).await? {
            Some(s) => (s, client.to_owned()),
            None => return Ok(None),
        }
    } else {
        match servers.first() {
            Some(s) if servers.len() == 1 => (Arc::clone(s), client.to_owned()),
            _ => return Ok(None),
        }
    };
    if !sel.visible(
        &req.env.state,
        &server,
        ItemKind::Resource,
        &upstream,
        client,
        None,
    ) {
        return Ok(None);
    }
    if check
        && exists(req, mode, &server, Kind::Resources, &upstream)
            .await?
            .is_none()
    {
        return Ok(None);
    }
    Ok(Some((server, upstream)))
}

fn set(params: &mut Value, pointer: &str, value: &str) {
    if let Some(slot) = params.pointer_mut(pointer) {
        *slot = Value::from(value);
    }
}

fn string_param<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str)
}

async fn resolve(
    req: &Req,
    sel: &Selection,
    mode: &Mode,
    method: &str,
    params: &Value,
) -> Result<Target, Fail> {
    let modern = matches!(mode, Mode::Modern { .. });
    let mut params = params.clone();
    match method {
        "tools/call" | "prompts/get" => {
            let name = string_param(&params, "name")
                .ok_or_else(|| Fail::invalid_params("params.name must be a string."))?
                .to_owned();
            let (kind, unknown): (Kind, fn(&str) -> Fail) = if method == "tools/call" {
                (Kind::Tools, unknown_tool)
            } else {
                (Kind::Prompts, unknown_prompt)
            };
            let Some((server, upstream, _)) = resolve_named(req, sel, mode, kind, &name).await? else {
                return Err(unknown(&name));
            };
            set(&mut params, "/name", &upstream);
            Ok(Target {
                server,
                item: upstream,
                params,
            })
        }
        "resources/read" | "resources/subscribe" | "resources/unsubscribe" => {
            let uri = string_param(&params, "uri")
                .ok_or_else(|| Fail::invalid_params("params.uri must be a string."))?
                .to_owned();
            let check = method == "resources/read";
            let Some((server, upstream)) = resolve_uri(req, sel, mode, &uri, check).await? else {
                return Err(unknown_resource(&uri, modern));
            };
            set(&mut params, "/uri", &upstream);
            Ok(Target {
                server,
                item: upstream,
                params,
            })
        }
        "completion/complete" => {
            let reference = params
                .get("ref")
                .filter(|r| r.is_object())
                .ok_or_else(|| Fail::invalid_params("params.ref must be an object."))?
                .clone();
            if let Some(name) = string_param(&reference, "name") {
                let Some((server, upstream, _)) = resolve_named(req, sel, mode, Kind::Prompts, name).await?
                else {
                    return Err(unknown_prompt(name));
                };
                set(&mut params, "/ref/name", &upstream);
                return Ok(Target {
                    server,
                    item: upstream,
                    params,
                });
            }
            let uri = string_param(&reference, "uri")
                .ok_or_else(|| Fail::invalid_params("params.ref needs a name or a uri."))?;
            let Some((server, upstream)) = resolve_uri(req, sel, mode, uri, false).await? else {
                return Err(unknown_resource(uri, modern));
            };
            set(&mut params, "/ref/uri", &upstream);
            Ok(Target {
                server,
                item: upstream,
                params,
            })
        }
        _ => Err(Fail::unknown_method(method, if modern { 404 } else { 200 })),
    }
}

fn modern_shape(req: &Req, sel: &Selection, method: &str) -> ModernShape {
    let server_info = match (&req.gw.server_info, req.gw.servers.len() > 1) {
        (ServerInfo::Gateway(info), true) => Some(info.clone()),
        _ => None,
    };
    let cache = (method == "resources/read").then(|| {
        (
            i64::try_from(req.gw.client_cache_max_ttl.as_millis()).unwrap_or(i64::MAX),
            sel.private(req),
        )
    });
    ModernShape { server_info, cache }
}

/// Routes one request to the server that owns its item and relays the answer.
pub(crate) async fn forward(
    req: &Req,
    sel: &Selection,
    mode: &Mode,
    id: &Value,
    method: &str,
    params: Option<&Value>,
    session: Option<&str>,
) -> Response<Body> {
    match forward_inner(req, sel, mode, id, method, params, session).await {
        Ok(r) => r,
        Err(f) => req.error(&f.id(id)),
    }
}

async fn forward_inner(
    req: &Req,
    sel: &Selection,
    mode: &Mode,
    id: &Value,
    method: &str,
    params: Option<&Value>,
    session: Option<&str>,
) -> Result<Response<Body>, Fail> {
    let params = match params {
        Some(p) if p.is_object() => p.clone(),
        None => json!({}),
        Some(_) => return Err(Fail::invalid_params("params must be an object.")),
    };
    let client_item =
        string_param(&params, if method == "resources/read" { "uri" } else { "name" }).map(str::to_owned);
    let mut target = resolve(req, sel, mode, method, &params).await?;
    let env = &req.env;
    env.log.update(|f| {
        f.server = Some(target.server.name.clone());
        f.item = Some(target.item.clone());
        f.client_item = client_item;
    });
    let check = ResultCheck {
        gateway: req.gw.name.clone(),
        server: target.server.name.clone(),
        method: method.to_owned(),
        item: target.item.clone(),
        principal: req.caller.principal.clone(),
    };
    let charge = if crate::link::NAMED.contains(&method) {
        env.hooks
            .admit(Counted::Mcp {
                server: &target.server.name,
                method,
                item: &target.item,
            })
            .await
            .map_err(Fail::from)?
    } else {
        None
    };
    let replaced = {
        let mut facts = check.facts();
        facts.arguments = target.params.get("arguments");
        env.hooks.before(&facts).await.map_err(Fail::from)?
    };
    if let (Some(args), Some(obj)) = (replaced, target.params.as_object_mut()) {
        obj.insert("arguments".into(), args);
    }
    let (mut link, opened) = mode
        .link(req, &target.server)
        .await
        .map_err(|e| e.fail(&target.server.name))?;
    if matches!(mode, Mode::Modern { .. })
        && let Some(params_headers) = modern_params(req)
    {
        for (k, v) in &params_headers {
            link.headers.insert(k.clone(), v.clone());
        }
    }
    let inflight = session.map(|s| env.state.begin(s, id, &target.server.name));
    let shaper = Shaper {
        env: env.clone(),
        server: Arc::clone(&target.server),
        prefix: req.gw.prefixed.then(|| target.server.prefix.clone()),
        method: method.to_owned(),
        client_id: id.clone(),
        modern: matches!(mode, Mode::Modern { .. }).then(|| modern_shape(req, sel, method)),
    };
    if matches!(mode, Mode::Modern { .. }) && opened {
        let bridged = bridged(req, &link, id, method, &mut target).await;
        link.close(env).await;
        let msg = bridged?;
        if let Some(c) = charge {
            c.answered(msg.get("error").is_none()).await;
        }
        let msg = check.apply(env, shaper.shape(msg)).await?;
        env.log.upstream_answer(&msg);
        return Ok(respond::json(200, &msg, "upstream"));
    }
    let started = std::time::Instant::now();
    let sent = link
        .send(
            env,
            id,
            method,
            Some(target.params.clone()),
            target.server.request_timeout,
        )
        .await;
    env.log.upstream(crate::log::UpstreamCall {
        server: target.server.name.clone(),
        method: method.to_owned(),
        item: Some(target.item.clone()),
        start: started,
        end: std::time::Instant::now(),
        error_code: match &sent {
            Ok(Answer::Json(m)) => m.pointer("/error/code").and_then(Value::as_i64),
            _ => None,
        },
        failed: sent.is_err(),
    });
    match sent {
        Ok(Answer::Stream(src)) => {
            if let Some(c) = charge {
                c.answered(true).await;
            }
            let owned = opened.then_some(link);
            let check = env.hooks.checks_results(method).then_some(check);
            relay::stream(src, shaper, owned, inflight, check).await
        }
        other => {
            if opened {
                link.close(env).await;
            }
            match other {
                Ok(Answer::Json(msg)) => {
                    if let Some(c) = charge {
                        c.answered(msg.get("error").is_none()).await;
                    }
                    let msg = check.apply(env, shaper.shape(msg)).await?;
                    env.log.upstream_answer(&msg);
                    Ok(respond::json(200, &msg, "upstream"))
                }
                Ok(Answer::Empty) => Err(UpError::NotMcp.fail(&target.server.name)),
                Err(e) => Err(e.fail(&target.server.name)),
                Ok(Answer::Stream(_)) => Err(UpError::NotMcp.fail(&target.server.name)),
            }
        }
    }
}

/// A modern request sent through a session opened for it with a legacy-only server: the server's
/// final answer, or an input-required result for the input it asked the client for.
async fn bridged(
    req: &Req,
    link: &crate::link::Link,
    id: &Value,
    method: &str,
    target: &mut Target,
) -> Result<Value, Fail> {
    let server = target.server.name.clone();
    let responses = crate::bridge::take_inputs(req, &server, &target.item, &mut target.params)?;
    let outcome = crate::bridge::call(
        &req.env,
        link,
        id,
        method,
        target.params.clone(),
        &responses,
        target.server.request_timeout,
    )
    .await
    .map_err(|e| e.fail(&server))?;
    Ok(match outcome {
        crate::bridge::Bridged::Final(msg) => msg,
        crate::bridge::Bridged::Input { key, request } => {
            let result =
                crate::bridge::input_required(req, &server, &target.item, &key, request, responses.len() + 1);
            json!({"jsonrpc": "2.0", "id": id, "result": result})
        }
    })
}

/// The facts the result hooks of one routed call see.
#[derive(Clone, Debug)]
pub(crate) struct ResultCheck {
    pub(crate) gateway: String,
    pub(crate) server: String,
    pub(crate) method: String,
    pub(crate) item: String,
    pub(crate) principal: String,
}

impl ResultCheck {
    pub(crate) fn facts(&self) -> CallFacts<'_> {
        CallFacts {
            gateway: &self.gateway,
            server: &self.server,
            method: &self.method,
            item: &self.item,
            arguments: None,
            principal: &self.principal,
        }
    }

    /// Runs the result hooks over a final answer carrying a result: the result may be replaced, or
    /// the answer refused.
    pub(crate) async fn apply(&self, env: &Env, mut msg: Value) -> Result<Value, Fail> {
        let Some(result) = msg.get("result") else {
            return Ok(msg);
        };
        if let Some(new) = env.hooks.after(&self.facts(), result).await.map_err(Fail::from)?
            && let Some(slot) = msg.get_mut("result")
        {
            *slot = new;
        }
        Ok(msg)
    }
}

/// The client's `Mcp-Param-*` headers, forwarded unchanged to the owning server only.
fn modern_params(req: &Req) -> Option<http::HeaderMap> {
    let mut out = http::HeaderMap::new();
    for (name, value) in &req.headers {
        if name.as_str().starts_with("mcp-param-") {
            out.insert(name.clone(), value.clone());
        }
    }
    (!out.is_empty()).then_some(out)
}
