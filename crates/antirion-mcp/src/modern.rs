//! The modern era (2026-07-28): stateless requests validated in a fixed order, `server/discover`
//! across servers, and the fallback that sends dual-era clients to `initialize` on mixed fleets.

use std::collections::BTreeSet;
use std::sync::Arc;

use antirion_core::Stage;
use antirion_error::Message;
use http::Response;
use serde_json::{Map, Value, json};

use crate::compile::{ServerInfo, ServerRt};
use crate::connect::modern_link;
use crate::discovery::{self, ensure_modern, legacy_required};
use crate::fanout::{self, modern_result, union_capabilities};
use crate::http::Req;
use crate::link::{NAMED, decode_header_value, item_of};
use crate::lists::{Kind, Mode};
use crate::reasons;
use crate::respond::{self, Extra};
use crate::rpc::{Fail, Msg};
use crate::select::Selection;
use crate::state::Hints;
use crate::upstream::{self, MODERN};
use crate::{Body, route};

pub(crate) const META_PV: &str = "io.modelcontextprotocol/protocolVersion";
pub(crate) const META_CAPS: &str = "io.modelcontextprotocol/clientCapabilities";

/// Methods the modern era removed; sent to it, they are unknown.
const LEGACY_ONLY: [&str; 8] = [
    "initialize",
    "notifications/initialized",
    "ping",
    "logging/setLevel",
    "resources/subscribe",
    "resources/unsubscribe",
    "roots/list",
    "notifications/roots/list_changed",
];

const KNOWN: [&str; 9] = [
    "server/discover",
    "tools/list",
    "tools/call",
    "prompts/list",
    "prompts/get",
    "resources/list",
    "resources/templates/list",
    "resources/read",
    "completion/complete",
];

fn invalid(code: i32, reason: antirion_error::Reason, message: &'static str) -> Fail {
    Fail::protocol(code, 400, reason, Stage::Parse, Message::Fixed(message))
}

/// Checks a message in a fixed order (the JSON-RPC shape, the `Mcp-Method` header, a known method,
/// `_meta` with its protocol version and client capabilities, the `Mcp-Name` header); the first
/// failure wins.
fn validate(req: &Req, msg: &Msg) -> Result<Value, Fail> {
    let Some(method) = msg.method() else {
        return Err(Fail::gateway(
            reasons::INVALID_JSONRPC,
            Stage::Parse,
            Message::Fixed("A modern request must be a JSON-RPC request or notification."),
        ));
    };
    if req.header("mcp-method") != Some(method) {
        return Err(invalid(
            -32020,
            reasons::MCP_HEADER_MISMATCH,
            "Mcp-Method header does not match the request method.",
        ));
    }
    if LEGACY_ONLY.contains(&method) {
        return Err(Fail::unknown_method(method, 404));
    }
    let meta = match msg.params() {
        None => Value::Object(Map::new()),
        Some(Value::Object(p)) => match p.get("_meta") {
            None => Value::Object(Map::new()),
            Some(m @ Value::Object(_)) => m.clone(),
            Some(_) => {
                return Err(invalid(
                    -32602,
                    reasons::MCP_META_INVALID,
                    "params._meta must be an object.",
                ));
            }
        },
        Some(_) => {
            return Err(invalid(
                -32602,
                reasons::MCP_META_INVALID,
                "params must be an object.",
            ));
        }
    };
    let Some(pv) = meta.get(META_PV) else {
        return Err(invalid(
            -32602,
            reasons::MCP_META_INVALID,
            "params._meta must carry the protocol version.",
        ));
    };
    if pv.as_str() != Some(MODERN) {
        return Err(invalid(
            -32020,
            reasons::MCP_PROTOCOL_VERSION_MISMATCH,
            "The _meta protocol version does not match MCP-Protocol-Version.",
        ));
    }
    if matches!(msg, Msg::Request { .. }) && !meta.get(META_CAPS).is_some_and(Value::is_object) {
        return Err(invalid(
            -32602,
            reasons::MCP_CLIENT_CAPABILITIES_MISSING,
            "params._meta must carry the client capabilities.",
        ));
    }
    if NAMED.contains(&method) {
        let item = item_of(method, msg.params());
        let sent = req.header("mcp-name").and_then(decode_header_value);
        if sent.is_none() || sent != item {
            return Err(invalid(
                -32020,
                reasons::MCP_HEADER_MISMATCH,
                "Mcp-Name header does not match the request.",
            ));
        }
    }
    Ok(meta)
}

/// A POSTed modern message.
pub(crate) async fn post(req: Req, msg: Msg) -> Response<Body> {
    let id = msg.id().cloned().unwrap_or(Value::Null);
    let meta = match validate(&req, &msg) {
        Ok(m) => m,
        Err(f) => return req.error(&f.id(&id)),
    };
    let Msg::Request { id, method, params } = msg else {
        notify(&req, &msg, &meta).await;
        return respond::accepted();
    };
    if !KNOWN.contains(&method.as_str()) {
        return req.error(&Fail::unknown_method(&method, 404).id(&id));
    }
    match request(&req, &id, &method, params.as_ref(), meta).await {
        Ok(r) => r,
        Err(f) => req.error(&f.id(&id)),
    }
}

async fn notify(req: &Req, msg: &Msg, meta: &Value) {
    let Ok(sel) = Selection::new(req) else {
        return;
    };
    let mut out = json!({"jsonrpc": "2.0", "method": msg.method().unwrap_or_default()});
    if let (Some(p), Some(obj)) = (msg.params().cloned(), out.as_object_mut()) {
        obj.insert("params".into(), p);
    }
    let posts = sel.servers.iter().map(|s| {
        let link = modern_link(req, s, meta);
        let out = out.clone();
        async move { link.post(&req.env, &out).await }
    });
    futures_util::future::join_all(posts).await;
}

async fn request(
    req: &Req,
    id: &Value,
    method: &str,
    params: Option<&Value>,
    meta: Value,
) -> Result<Response<Body>, Fail> {
    let sel = Selection::new(req)?;
    if method == "server/discover" {
        return discover(req, &sel, id, &meta).await;
    }
    if !req.gw.modern_bridge {
        ensure_modern(req, &sel, &meta).await?;
    }
    let mode = Mode::Modern { meta };
    if let Some(found) = crate::toolsearch::intercept(req, &sel, &mode, &sel.servers, method, params).await {
        return match found? {
            crate::toolsearch::Intercepted::Result(mut v) => {
                fanout::modern_result(req, &sel, &mut v, None);
                Ok(answer(id, v, Extra::default()))
            }
            crate::toolsearch::Intercepted::Forward(p) => {
                Ok(route::forward(req, &sel, &mode, id, "tools/call", Some(&p), None).await)
            }
        };
    }
    if let Some(kind) = Kind::of(method) {
        let (result, extra) = fanout::list(req, &sel, &mode, &sel.servers, kind, params).await?;
        return Ok(answer(id, result, extra));
    }
    Ok(route::forward(req, &sel, &mode, id, method, params, None).await)
}

fn answer(id: &Value, result: Value, extra: Extra) -> Response<Body> {
    let mut resp = respond::json(
        200,
        &json!({"jsonrpc": "2.0", "id": id, "result": result}),
        "upstream",
    );
    extra.merge_into(&mut resp);
    resp
}

/// What a legacy-only server looks like to a modern client through the bridge: the capabilities and
/// server info of an `initialize` made for the purpose, and the modern version.
async fn bridged_discovery(req: &Req, server: &Arc<ServerRt>, meta: &Value) -> Option<Value> {
    let (link, init) = crate::bridge::open_for_modern(req, server, meta).await.ok()?;
    link.close(&req.env).await;
    let mut result = json!({
        "supportedVersions": [MODERN],
        "capabilities": init.get("capabilities").cloned().unwrap_or_else(|| json!({})),
    });
    if let (Some(info), Some(obj)) = (init.get("serverInfo"), result.as_object_mut()) {
        obj.insert("serverInfo".into(), info.clone());
    }
    Some(result)
}

/// `server/discover`: forwarded on a single-server gateway, merged on a multiplexed one; any server
/// that is not modern-capable sends the client back to the legacy era.
async fn discover(req: &Req, sel: &Selection, id: &Value, meta: &Value) -> Result<Response<Body>, Fail> {
    let configured = req.gw.servers.iter().any(|s| req.env.snap.server(s).is_some());
    if !configured {
        return Err(fanout::none_available());
    }
    let asks = sel.servers.iter().map(|s| async move {
        match discovery::discover(req, s, meta).await {
            (true, Some(r)) => Some(r),
            _ if req.gw.modern_bridge => bridged_discovery(req, s, meta).await,
            _ => None,
        }
    });
    let answers = futures_util::future::join_all(asks).await;
    let mut results = Vec::new();
    for result in answers {
        match result {
            Some(r) => results.push(r),
            None => return Err(legacy_required()),
        }
    }
    let hints: Vec<Hints> = results.iter().map(Hints::of).collect();
    let mut result = if req.gw.servers.len() == 1 {
        results.pop().unwrap_or_else(|| json!({}))
    } else {
        let mut common: BTreeSet<String> = upstream::supported().into_iter().map(str::to_owned).collect();
        for r in &results {
            let theirs: BTreeSet<String> = r
                .get("supportedVersions")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
            common = common.intersection(&theirs).cloned().collect();
        }
        json!({
            "supportedVersions": common.into_iter().collect::<Vec<_>>(),
            "capabilities": union_capabilities(results.iter().filter_map(|r| r.get("capabilities"))),
        })
    };
    if let (ServerInfo::Gateway(info), Some(obj)) = (&req.gw.server_info, result.as_object_mut()) {
        obj.insert("serverInfo".into(), info.clone());
    }
    if let Some(obj) = result.as_object_mut() {
        obj.remove("ttlMs");
        obj.remove("cacheScope");
    }
    modern_result(req, sel, &mut result, Some(&hints));
    Ok(answer(id, result, Extra::default()))
}
