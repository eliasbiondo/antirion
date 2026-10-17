//! Requests answered from every selected server: list methods, with the partial-failure policy, and
//! the modern decorations of a result (`resultType`, gateway `serverInfo`, caching hints).

use std::sync::Arc;
use std::time::Duration;

use antirion_core::Stage;
use antirion_error::Message;
use serde_json::{Map, Value, json};

use crate::compile::{ServerInfo, ServerRt};
use crate::http::Req;
use crate::lists::{self, Kind, Mode};
use crate::reasons;
use crate::respond::Extra;
use crate::rpc::Fail;
use crate::select::Selection;
use crate::state::Hints;
use crate::upstream::UpError;

pub(crate) const UNAVAILABLE_META: &str = "antirion/unavailable_servers";
pub(crate) const SERVER_INFO_META: &str = "io.modelcontextprotocol/serverInfo";

/// Every selected server failed, or none is configured.
pub(crate) fn none_available() -> Fail {
    let mut f = Fail::gateway(
        reasons::MCP_NO_SERVER_AVAILABLE,
        Stage::Attempt,
        Message::Fixed("No MCP server of this gateway is available."),
    );
    f.err.retry_after = Some(Duration::from_secs(1));
    f
}

/// A selected server failed on a gateway with `on_server_failure: fail`.
pub(crate) fn server_failed(name: &str) -> Fail {
    Fail::gateway(
        reasons::MCP_SERVER_FAILED,
        Stage::Attempt,
        Message::built(format!("MCP server {name} failed.")),
    )
    .provider(name)
}

/// Applies the failure policy to a fan-out: `Ok(names of failed servers)` when the result may be
/// served, else the error to answer with.
pub(crate) fn policy(req: &Req, tried: usize, failed: &[(String, UpError)]) -> Result<Vec<String>, Fail> {
    if let Some((name, e)) = failed.iter().find(|(_, e)| *e == UpError::SessionGone) {
        return Err(e.fail(name));
    }
    if failed.len() == tried && tried > 0 {
        return Err(none_available());
    }
    if let Some((name, _)) = failed.first().filter(|_| req.gw.fail_closed) {
        return Err(server_failed(name));
    }
    let mut names: Vec<String> = failed.iter().map(|(n, _)| n.clone()).collect();
    names.sort();
    Ok(names)
}

/// Marks a result served without some servers: `_meta` and the response headers.
pub(crate) fn mark_partial(result: &mut Value, failed: &[String], extra: &mut Extra) {
    if failed.is_empty() {
        return;
    }
    if let Some(obj) = result.as_object_mut() {
        let meta = obj.entry("_meta").or_insert_with(|| Value::Object(Map::new()));
        if let Some(m) = meta.as_object_mut() {
            m.insert(UNAVAILABLE_META.into(), json!(failed));
        }
    }
    extra.set("x-antirion-mcp-unavailable", &failed.join(","));
    extra.set("x-antirion-degraded", "mcp_servers");
}

/// The modern decorations: `resultType` when missing, the gateway's `serverInfo` on multiplexed
/// gateways, and for cacheable results the merged caching hints.
pub(crate) fn modern_result(req: &Req, sel: &Selection, result: &mut Value, hints: Option<&[Hints]>) {
    let multiplexed = req.gw.servers.len() > 1;
    let Some(obj) = result.as_object_mut() else {
        return;
    };
    obj.entry("resultType").or_insert_with(|| Value::from("complete"));
    if multiplexed && let ServerInfo::Gateway(info) = &req.gw.server_info {
        let meta = obj.entry("_meta").or_insert_with(|| Value::Object(Map::new()));
        if let Some(m) = meta.as_object_mut() {
            m.insert(SERVER_INFO_META.into(), info.clone());
        }
    }
    if let Some(hints) = hints {
        let cap = i64::try_from(req.gw.client_cache_max_ttl.as_millis()).unwrap_or(i64::MAX);
        let ttl = hints.iter().map(|h| h.ttl_ms).min().unwrap_or(0).min(cap).max(0);
        let public = !hints.is_empty() && hints.iter().all(|h| h.public) && !sel.private(req);
        obj.insert("ttlMs".into(), Value::from(ttl));
        obj.insert(
            "cacheScope".into(),
            Value::from(if public { "public" } else { "private" }),
        );
    }
}

/// A list method answered from every selected server.
pub(crate) async fn list(
    req: &Req,
    sel: &Selection,
    mode: &Mode,
    servers: &[Arc<ServerRt>],
    kind: Kind,
    params: Option<&Value>,
) -> Result<(Value, Extra), Fail> {
    let from = lists::offset(req, kind.method(), params)?;
    let modern = matches!(mode, Mode::Modern { .. });
    let read = lists::entries(req, mode, servers, kind, modern).await;
    let mut ok = Vec::new();
    let mut failed = Vec::new();
    for (server, r) in read {
        match r {
            Ok(e) => ok.push((server, e)),
            Err(e) => failed.push((server.name.clone(), e)),
        }
    }
    let failed = policy(req, servers.len(), &failed)?;
    let merged = lists::merge(req, sel, kind, &ok);
    let hints = merged.hints.clone();
    let (items, next) = lists::page(req, kind.method(), merged, from);
    req.env.log.update(|f| f.listed = Some(items.len()));
    let mut result = json!({ kind.key(): items });
    if let (Some(next), Some(obj)) = (next, result.as_object_mut()) {
        obj.insert("nextCursor".into(), Value::from(next));
    }
    let mut extra = Extra::default();
    mark_partial(&mut result, &failed, &mut extra);
    if modern {
        modern_result(req, sel, &mut result, Some(&hints));
    }
    Ok((result, extra))
}

/// The union of capability objects: an object is present when any server has it; boolean flags are
/// true when any server set them.
pub(crate) fn union_capabilities<'a>(all: impl Iterator<Item = &'a Value>) -> Value {
    let mut out = Map::new();
    for caps in all {
        let Some(obj) = caps.as_object() else {
            continue;
        };
        for (k, v) in obj {
            let slot = out.entry(k.clone()).or_insert_with(|| Value::Object(Map::new()));
            if let (Some(dst), Some(src)) = (slot.as_object_mut(), v.as_object()) {
                for (fk, fv) in src {
                    match (dst.get(fk), fv) {
                        (Some(Value::Bool(true)), _) => {}
                        (_, Value::Bool(b)) => {
                            dst.insert(fk.clone(), Value::Bool(*b));
                        }
                        (None, other) => {
                            dst.insert(fk.clone(), other.clone());
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_union() {
        let a = json!({"tools": {"listChanged": false}, "logging": {}});
        let b = json!({"tools": {"listChanged": true}, "prompts": {}, "resources": {"subscribe": true}});
        let u = union_capabilities([&a, &b].into_iter());
        assert_eq!(u["tools"]["listChanged"], true);
        assert!(u.get("logging").is_some() && u.get("prompts").is_some());
        assert_eq!(u["resources"]["subscribe"], true);
    }
}
