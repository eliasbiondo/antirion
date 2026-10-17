//! Whether a server speaks the modern era: learned with `server/discover` the first time a modern
//! request needs it and remembered for the server's `discovery_ttl`.

use std::sync::Arc;

use antirion_core::Stage;
use antirion_error::Message;
use serde_json::Value;

use crate::compile::{Protocol, ServerRt};
use crate::connect::modern_link;
use crate::http::Req;
use crate::reasons;
use crate::rpc::Fail;
use crate::select::Selection;
use crate::state::Discovered;
use crate::upstream::MODERN;

const MODERN_ERRORS: [i64; 3] = [-32020, -32021, -32022];

/// Sends `server/discover` now and records what it showed. The result is returned when the server
/// answered with one that includes the modern version.
pub(crate) async fn discover(req: &Req, server: &Arc<ServerRt>, meta: &Value) -> (bool, Option<Value>) {
    let link = modern_link(req, server, meta);
    let answer = link.ask(&req.env, "server/discover", None).await;
    let (modern, result, error) = match &answer {
        Ok(msg) => {
            let result = msg.get("result").filter(|r| {
                r.get("supportedVersions")
                    .and_then(Value::as_array)
                    .is_some_and(|v| v.iter().any(|x| x.as_str() == Some(MODERN)))
            });
            let modern_error = msg
                .get("error")
                .and_then(|e| e.get("code"))
                .and_then(Value::as_i64)
                .is_some_and(|c| MODERN_ERRORS.contains(&c));
            (
                result.is_some() || modern_error,
                result.cloned(),
                (result.is_none() && !modern_error).then(|| "server/discover was not answered".to_owned()),
            )
        }
        Err(e) => (false, None, Some(format!("{e:?}"))),
    };
    let now = req.env.now;
    req.env
        .state
        .discovery
        .lock()
        .insert(server.generation, Discovered { modern, at: now });
    {
        let mut facts = req.env.state.facts.lock();
        let f = facts.entry(server.name.clone()).or_default();
        f.last_discovery_at = Some(now);
        f.last_error = error;
        f.modern = Some(modern);
    }
    (modern, result)
}

/// Whether `server` is modern-capable, from its setting, the cache, or a discovery.
pub(crate) async fn is_modern(req: &Req, server: &Arc<ServerRt>, meta: &Value) -> bool {
    match server.protocol {
        Protocol::Legacy => return false,
        Protocol::Modern => return true,
        Protocol::Auto => {}
    }
    let cached = req
        .env
        .state
        .discovery
        .lock()
        .get(&server.generation)
        .filter(|d| req.env.now.since(d.at) < server.discovery_ttl && req.env.now >= d.at)
        .map(|d| d.modern);
    match cached {
        Some(m) => m,
        None => discover(req, server, meta).await.0,
    }
}

/// The answer for a modern request on a gateway whose selected servers are not all modern-capable:
/// a 400 that is not a recognised modern error, so a dual-era client falls back to `initialize`.
pub(crate) fn legacy_required() -> Fail {
    Fail::gateway(
        reasons::MCP_LEGACY_REQUIRED,
        Stage::Route,
        Message::Fixed("this MCP gateway requires the legacy protocol: send initialize"),
    )
}

/// Requires every selected server to be modern-capable.
pub(crate) async fn ensure_modern(req: &Req, sel: &Selection, meta: &Value) -> Result<(), Fail> {
    let checks = sel.servers.iter().map(|s| is_modern(req, s, meta));
    let all = futures_util::future::join_all(checks).await;
    if all.into_iter().all(|m| m) {
        Ok(())
    } else {
        Err(legacy_required())
    }
}
