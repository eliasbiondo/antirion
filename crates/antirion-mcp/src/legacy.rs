//! The legacy era (2024-11-05 to 2025-11-25): `initialize` negotiation across servers, sessions
//! carried in encrypted tokens, the server-to-client GET stream, `DELETE`, and the stateless mode.

use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;

use antirion_core::Stage;
use antirion_error::Message;
use http::{HeaderName, HeaderValue, Response, StatusCode};
use serde_json::{Map, Value, json};

use crate::compile::{ServerInfo, ServerRt};
use crate::connect::{self, gateway_client_info};
use crate::fanout::{self, mark_partial, union_capabilities};
use crate::http::{Req, method_not_allowed};
use crate::link::{Link, NAMED};
use crate::lists::{Kind, Mode};
use crate::reasons;
use crate::relay::{decode_id, shape_unsolicited};
use crate::respond::{self, Extra};
use crate::rpc::{Fail, Msg};
use crate::select::Selection;
use crate::token::{SessionClaims, Upstream};
use crate::toolsearch::{self, Intercepted};
use crate::upstream::{self, DEFAULT_LEGACY, LATEST_LEGACY};
use crate::{Body, route};

const ROUTED: [&str; 3] = [
    "resources/subscribe",
    "resources/unsubscribe",
    "completion/complete",
];

fn result_response(id: &Value, result: Value, extra: Extra) -> Response<Body> {
    let mut resp = respond::json(
        200,
        &json!({"jsonrpc": "2.0", "id": id, "result": result}),
        "upstream",
    );
    extra.merge_into(&mut resp);
    resp
}

fn mismatch(id: &Value) -> Fail {
    Fail::gateway(
        reasons::MCP_PROTOCOL_VERSION_MISMATCH,
        Stage::Parse,
        Message::Fixed("MCP-Protocol-Version does not match the negotiated protocol version."),
    )
    .id(id)
}

/// A POSTed legacy message.
pub(crate) async fn post(req: Req, msg: Msg, raw: Value, version: Option<String>) -> Response<Body> {
    if let Msg::Request { id, method, params } = &msg
        && method == "initialize"
    {
        return match initialize(&req, id, params.as_ref(), version.as_deref()).await {
            Ok(r) => r,
            Err(f) => req.error(&f.id(id)),
        };
    }
    if req.gw.stateless {
        return stateless(&req, msg, version).await;
    }
    let id = msg.id().cloned().unwrap_or(Value::Null);
    let (token, claims) = match session(&req) {
        Ok(s) => s,
        Err(f) => return req.error(&f.id(&id)),
    };
    if version.as_deref().is_some_and(|v| v != claims.pv) {
        return req.error(&mismatch(&id));
    }
    req.env
        .log
        .update(|f| f.protocol_version = Some(claims.pv.clone()));
    let sel = match Selection::new(&req) {
        Ok(s) => s,
        Err(f) => return req.error(&f.id(&id)),
    };
    let links = session_links(&req, &sel, &claims);
    let mode = Mode::Session { links };
    match msg {
        Msg::Request { id, method, params } => {
            request(&req, &sel, &mode, &id, &method, params.as_ref(), Some(&token)).await
        }
        Msg::Notification { method, params } => {
            notification(&req, &mode, &token, &method, params).await;
            respond::accepted()
        }
        Msg::Response { .. } => {
            if let Mode::Session { links } = &mode {
                deliver_response(&req, links, raw).await;
            }
            respond::accepted()
        }
    }
}

/// The session a request carries: decrypted, bound to this gateway and principal, unexpired and not
/// deleted.
fn session(req: &Req) -> Result<(String, SessionClaims), Fail> {
    let Some(token) = req.header("mcp-session-id") else {
        return Err(Fail::gateway(
            reasons::MCP_SESSION_REQUIRED,
            Stage::Parse,
            Message::Fixed("session header is required for non-initialize requests"),
        ));
    };
    let now = req.env.now.0 / 1_000_000;
    let claims = req
        .keys
        .open_session(&req.gw.name, token)
        .filter(|c| c.sub == req.caller.principal && c.exp > now && !req.env.state.is_revoked(token));
    match claims {
        Some(c) => {
            req.env.log.update(|f| f.session_id = Some(token.to_owned()));
            Ok((token.to_owned(), c))
        }
        None => {
            req.env.state.count(&req.gw.name, false);
            Err(Fail::gateway(
                reasons::MCP_SESSION_NOT_FOUND,
                Stage::Parse,
                Message::Fixed("The MCP session is unknown or has ended; initialize again."),
            ))
        }
    }
}

/// Links of the session's servers that are still selected and whose configuration has not changed.
fn session_links(req: &Req, sel: &Selection, claims: &SessionClaims) -> Vec<Link> {
    sel.servers
        .iter()
        .filter_map(|s| {
            let up = claims
                .up
                .iter()
                .find(|u| u.s == s.name && u.generation == s.generation)?;
            Some(connect::session_link(req, s, up, claims))
        })
        .collect()
}

async fn request(
    req: &Req,
    sel: &Selection,
    mode: &Mode,
    id: &Value,
    method: &str,
    params: Option<&Value>,
    token: Option<&str>,
) -> Response<Body> {
    if method == "ping" {
        return result_response(id, json!({}), Extra::default());
    }
    let servers: Vec<Arc<ServerRt>> = match mode {
        Mode::Session { links } => links.iter().map(|l| Arc::clone(&l.server)).collect(),
        _ => sel.servers.clone(),
    };
    if let Some(found) = toolsearch::intercept(req, sel, mode, &servers, method, params).await {
        return match found {
            Ok(Intercepted::Result(v)) => result_response(id, v, Extra::default()),
            Ok(Intercepted::Forward(p)) => {
                route::forward(req, sel, mode, id, "tools/call", Some(&p), token).await
            }
            Err(f) => req.error(&f.id(id)),
        };
    }
    if let Some(kind) = Kind::of(method) {
        if params.is_some_and(|p| !p.is_object()) {
            return req.error(&Fail::invalid_params("params must be an object.").id(id));
        }
        return match fanout::list(req, sel, mode, &servers, kind, params).await {
            Ok((result, extra)) => result_response(id, result, extra),
            Err(f) => req.error(&f.id(id)),
        };
    }
    if NAMED.contains(&method) || ROUTED.contains(&method) {
        return route::forward(req, sel, mode, id, method, params, token).await;
    }
    if method == "logging/setLevel"
        && let Mode::Session { links } = mode
    {
        let sends = links.iter().map(|l| l.ask(&req.env, method, params.cloned()));
        futures_util::future::join_all(sends).await;
        return result_response(id, json!({}), Extra::default());
    }
    req.error(&Fail::unknown_method(method, 200).id(id))
}

async fn notification(req: &Req, mode: &Mode, token: &str, method: &str, params: Option<Value>) {
    let Mode::Session { links } = mode else {
        return;
    };
    if method == "notifications/initialized" {
        return;
    }
    let mut msg = json!({"jsonrpc": "2.0", "method": method});
    if let (Some(p), Some(obj)) = (params.clone(), msg.as_object_mut()) {
        obj.insert("params".into(), p);
    }
    if method == "notifications/cancelled" {
        let target = params
            .as_ref()
            .and_then(|p| p.get("requestId"))
            .and_then(|rid| req.env.state.executing(token, rid));
        if let Some(link) = target.and_then(|name| links.iter().find(|l| l.server.name == name)) {
            let _ = link.post(&req.env, &msg).await;
        }
        return;
    }
    let posts = links.iter().map(|l| l.post(&req.env, &msg));
    futures_util::future::join_all(posts).await;
}

/// Delivers a client's answer to a server-to-client request to the server its `ag1` id names.
async fn deliver_response(req: &Req, links: &[Link], raw: Value) {
    let Some((server, original)) = raw.get("id").and_then(decode_id) else {
        tracing::warn!("dropped a client response whose id names no server");
        return;
    };
    let Some(link) = links.iter().find(|l| l.server.name == server) else {
        tracing::warn!("dropped a client response for a server outside the session");
        return;
    };
    let mut out = raw.clone();
    if let Some(obj) = out.as_object_mut() {
        obj.insert("id".into(), original);
    }
    let _ = link.post(&req.env, &out).await;
}

/// `initialize`: negotiates the version with every selected server and issues the session token.
async fn initialize(
    req: &Req,
    id: &Value,
    params: Option<&Value>,
    header: Option<&str>,
) -> Result<Response<Body>, Fail> {
    let asked = params
        .and_then(|p| p.get("protocolVersion"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if header.is_some_and(|h| h != asked) {
        return Err(mismatch(id));
    }
    let offer = if upstream::is_legacy(&asked) {
        asked
    } else {
        LATEST_LEGACY.to_owned()
    };
    let sel = Selection::new(req)?;
    let stateful = !req.gw.stateless;
    let (caps, info) = if stateful {
        (
            params
                .and_then(|p| p.get("capabilities"))
                .cloned()
                .unwrap_or_else(|| json!({})),
            params
                .and_then(|p| p.get("clientInfo"))
                .cloned()
                .unwrap_or_else(|| json!({})),
        )
    } else {
        (json!({}), gateway_client_info())
    };
    let opens = sel
        .servers
        .iter()
        .map(|s| connect::open(req, s, &offer, &caps, &info, stateful));
    let outcomes = futures_util::future::join_all(opens).await;
    let mut ok: Vec<(Link, Value)> = Vec::new();
    let mut failed = Vec::new();
    for (server, r) in sel.servers.iter().zip(outcomes) {
        match r {
            Ok(pair) => ok.push(pair),
            Err(e) => failed.push((server.name.clone(), e)),
        }
    }
    let configured = req.gw.servers.iter().any(|s| req.env.snap.server(s).is_some());
    let verdict = if configured {
        fanout::policy(req, sel.servers.len(), &failed)
    } else {
        Err(fanout::none_available())
    };
    let failed = match verdict {
        Ok(f) => f,
        Err(f) => {
            for (link, _) in &ok {
                link.close(&req.env).await;
            }
            return Err(f);
        }
    };
    let version = ok
        .iter()
        .map(|(l, _)| l.pv.clone())
        .chain(std::iter::once(offer))
        .min()
        .unwrap_or_else(|| LATEST_LEGACY.to_owned());
    req.env.log.update(|f| f.protocol_version = Some(version.clone()));
    let mut result = initialize_result(req, &version, &ok);
    let mut extra = Extra::default();
    mark_partial(&mut result, &failed, &mut extra);
    if stateful {
        let claims = SessionClaims {
            v: 1,
            gw: req.gw.name.clone(),
            sub: req.caller.principal.clone(),
            iat: req.env.now.0 / 1_000_000,
            exp: req.env.now.saturating_add(req.env.snap.max_lifetime).0 / 1_000_000,
            pv: version,
            caps,
            ci: info,
            up: ok
                .iter()
                .map(|(l, _)| Upstream {
                    s: l.server.name.clone(),
                    sid: l.sid.clone(),
                    pv: l.pv.clone(),
                    generation: l.server.generation,
                })
                .collect(),
        };
        let token = req.keys.seal_session(&claims).ok_or_else(|| {
            Fail::gateway(
                reasons::SESSION_STORE_UNAVAILABLE,
                Stage::Respond,
                Message::Fixed("The MCP session could not be sealed."),
            )
        })?;
        req.env.state.count(&req.gw.name, true);
        req.env.log.update(|f| f.session_id = Some(token.clone()));
        if let Ok(v) = HeaderValue::from_str(&token) {
            extra.headers.insert(HeaderName::from_static("mcp-session-id"), v);
        }
    } else {
        for (link, _) in &ok {
            link.close(&req.env).await;
        }
    }
    Ok(result_response(id, result, extra))
}

fn initialize_result(req: &Req, version: &str, ok: &[(Link, Value)]) -> Value {
    let single = req.gw.servers.len() == 1;
    let server_info = match (&req.gw.server_info, single, ok.first()) {
        (ServerInfo::Upstream, true, Some((_, r))) => r.get("serverInfo").cloned().unwrap_or(Value::Null),
        (ServerInfo::Gateway(info), _, _) => info.clone(),
        (ServerInfo::Upstream, _, _) => json!({"name": "antirion", "version": env!("CARGO_PKG_VERSION")}),
    };
    let caps = union_capabilities(ok.iter().filter_map(|(_, r)| r.get("capabilities")));
    let mut result = Map::new();
    result.insert("protocolVersion".into(), Value::from(version));
    result.insert("capabilities".into(), caps);
    result.insert("serverInfo".into(), server_info);
    let instructions: Vec<String> = ok
        .iter()
        .filter_map(|(l, r)| {
            let text = r.get("instructions")?.as_str().filter(|t| !t.is_empty())?;
            Some(if single {
                text.to_owned()
            } else {
                format!("[{}] {text}", l.server.prefix)
            })
        })
        .collect();
    if !instructions.is_empty() {
        result.insert("instructions".into(), Value::from(instructions.join("\n\n")));
    }
    Value::Object(result)
}

/// A request to a stateless gateway: each use initializes the servers it needs and deletes their
/// sessions afterwards.
async fn stateless(req: &Req, msg: Msg, version: Option<String>) -> Response<Body> {
    let Msg::Request { id, method, params } = msg else {
        return respond::accepted();
    };
    let sel = match Selection::new(req) {
        Ok(s) => s,
        Err(f) => return req.error(&f.id(&id)),
    };
    let mode = Mode::Stateless {
        pv: version.unwrap_or_else(|| DEFAULT_LEGACY.to_owned()),
    };
    request(req, &sel, &mode, &id, &method, params.as_ref(), None).await
}

/// The client-facing id of an event a server sent with id `upstream`.
pub(crate) fn resumable_id(server: &str, upstream: &str) -> String {
    format!("ag1:{server}:{}", URL_SAFE_NO_PAD.encode(upstream.as_bytes()))
}

/// The server and upstream event id a client-facing event id names.
pub(crate) fn resume_point(id: &str) -> Option<(String, String)> {
    let rest = id.strip_prefix("ag1:")?;
    let (server, encoded) = rest.split_once(':')?;
    let upstream = String::from_utf8(URL_SAFE_NO_PAD.decode(encoded).ok()?).ok()?;
    (!server.is_empty() && !upstream.is_empty()).then(|| (server.to_owned(), upstream))
}

/// The warning that a server's events sent while the client was away are not replayed.
fn not_replayed(server: &str) -> Value {
    json!({"jsonrpc": "2.0", "method": "notifications/message", "params": {
        "level": "warning",
        "logger": "antirion",
        "data": {"server": server,
                 "message": format!("events of MCP server {server} sent before this stream opened were not replayed")},
    }})
}

/// `GET`: the server-to-client stream, merged from every session server that offers one. Events a
/// server sent with an id carry `ag1:<server>:<base64url id>`; a client reconnecting with such a
/// `Last-Event-ID`, on any node, has that server's stream resumed after it, and is told which
/// servers' missed events are not replayed.
pub(crate) async fn get(req: Req) -> Response<Body> {
    if req.gw.stateless {
        return req.error(&method_not_allowed());
    }
    let (_, claims) = match session(&req) {
        Ok(s) => s,
        Err(f) => return req.error(&f),
    };
    let sel = match Selection::new(&req) {
        Ok(s) => s,
        Err(f) => return req.error(&f),
    };
    let links = session_links(&req, &sel, &claims);
    let reconnect = req.header("last-event-id").map(str::to_owned);
    let resume = reconnect.as_deref().and_then(resume_point);
    let env = &req.env;
    let opens = links.iter().map(|l| {
        let from = resume
            .as_ref()
            .filter(|(s, _)| *s == l.server.name)
            .map(|(_, id)| id.clone());
        async move {
            let src = l.listen(env, from.as_deref()).await;
            (l.clone(), src, from.is_some())
        }
    });
    let opened = futures_util::future::join_all(opens).await;
    let mut warnings: Vec<Bytes> = Vec::new();
    let mut streams = Vec::new();
    for (link, src, resumed) in opened {
        if reconnect.is_some() && !resumed {
            warnings.push(respond::event(&not_replayed(&link.server.name)));
        }
        if let Some(src) = src {
            streams.push((link, src));
        }
    }
    if streams.is_empty() {
        return req.error(&method_not_allowed());
    }
    let prefixed = req.gw.prefixed;
    let env = req.env.clone();
    let merged = futures_util::stream::select_all(streams.into_iter().map(|(link, src)| {
        let env = env.clone();
        Box::pin(futures_util::stream::unfold(
            (src, link, env),
            move |(mut src, link, env)| async move {
                loop {
                    match src.next_event(std::time::Duration::from_secs(3600)).await {
                        Ok((msg, id)) => {
                            let prefix = prefixed.then(|| link.server.prefix.clone());
                            let shaped = shape_unsolicited(&env, &link.server, prefix.as_deref(), msg);
                            let id = id.map(|i| resumable_id(&link.server.name, &i));
                            return Some((respond::event_with_id(&shaped, id.as_deref()), (src, link, env)));
                        }
                        Err(crate::sse::ReadError::Idle) => {}
                        Err(crate::sse::ReadError::Ended) => return None,
                    }
                }
            },
        ))
    }));
    respond::sse(futures_util::StreamExt::chain(
        futures_util::stream::iter(warnings),
        merged,
    ))
}

/// `DELETE`: ends the upstream sessions that exist and revokes the token.
pub(crate) async fn delete(req: Req) -> Response<Body> {
    if req.gw.stateless {
        return req.error(&method_not_allowed());
    }
    let (token, claims) = match session(&req) {
        Ok(s) => s,
        Err(f) => return req.error(&f),
    };
    let links: Vec<Link> = claims
        .up
        .iter()
        .filter(|u| u.sid.is_some())
        .filter_map(|u| {
            let server = req.env.snap.servers.get(&u.s)?;
            (server.generation == u.generation).then(|| connect::session_link(&req, server, u, &claims))
        })
        .collect();
    futures_util::future::join_all(links.iter().map(|l| l.close(&req.env))).await;
    req.env.state.revoke(&token, claims.exp, req.env.now);
    respond::empty(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resumable_ids_round_trip() {
        let id = resumable_id("a", "g7-3");
        assert_eq!(id, "ag1:a:ZzctMw");
        assert_eq!(resume_point(&id), Some(("a".to_owned(), "g7-3".to_owned())));
        assert_eq!(resume_point("ag1:a:"), None);
        assert_eq!(resume_point("ag1:a:!!"), None);
        assert_eq!(resume_point("42"), None);
    }
}
