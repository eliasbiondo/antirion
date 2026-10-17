//! `/admin/mcp`: a read-only MCP server over the request log and analytics. Each tool calls the
//! Admin API's own read with the caller's principal, so permissions, tenancy scopes and the audit
//! of sensitive reads are those of the API.

use antirion_runtime::Snapshot;
use axum::response::Response;
use bytes::Bytes;
use http::{Method, StatusCode};
use serde_json::{Map, Value, json};

use crate::Handled;
use crate::call::Call;
use crate::error::AdminError;
use crate::local::json_response;

/// The server's path on the admin listener.
pub(crate) const PATH: &str = "/admin/mcp";

/// The legacy protocol versions, oldest first; the last is the one offered by default.
const LEGACY: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];
const MODERN: &str = "2026-07-28";

/// Whether the server is switched on.
pub(crate) fn enabled(snap: &Snapshot) -> bool {
    snap.config.doc.pointer("/mcp/admin_server/enabled") == Some(&Value::Bool(true))
}

/// One read-only tool.
struct Tool {
    name: &'static str,
    description: &'static str,
    permission: &'static str,
    /// Its arguments: name, JSON type and description.
    args: &'static [(&'static str, &'static str, &'static str)],
}

const TOOLS: &[Tool] = &[
    Tool {
        name: "search_logs",
        description: "Lists request-log records, newest first, filtered by time range, tenancy, Model, deployment, status or text.",
        permission: "logs:read",
        args: &[
            ("from", "string", "Start of the time range (RFC 3339)."),
            ("to", "string", "End of the time range (RFC 3339)."),
            ("model", "string", "A Model name."),
            ("provider", "string", "A provider name."),
            ("deployment", "string", "A deployment name."),
            ("key", "string", "A virtual key name."),
            ("project", "string", "A project name."),
            ("team", "string", "A team name."),
            ("organization", "string", "An organization name."),
            ("status_code", "integer", "An HTTP status code."),
            ("status_class", "string", "2xx, 4xx or 5xx."),
            ("code", "string", "An error code."),
            ("end_user", "string", "An end user."),
            ("q", "string", "Text to search for."),
            ("limit", "integer", "Records per page, at most 100."),
            ("cursor", "string", "The next_cursor of a previous page."),
        ],
    },
    Tool {
        name: "get_request",
        description: "Reads one request-log record by its id, with its attempts, usage, cost and guardrail verdicts.",
        permission: "logs:read",
        args: &[("id", "string", "The record's id (x-antirion-log-id).")],
    },
    Tool {
        name: "usage_summary",
        description: "Aggregates requests, tokens, cost and latency over a time range, optionally grouped and bucketed.",
        permission: "analytics:read",
        args: &[
            ("from", "string", "Start of the time range (RFC 3339)."),
            ("to", "string", "End of the time range (RFC 3339)."),
            (
                "group_by",
                "string",
                "Comma-separated dimensions, such as model,project.",
            ),
            ("interval", "string", "Bucket size: 1m, 5m, 15m, 1h, 6h or 1d."),
            ("tz", "string", "Time zone of day buckets."),
            ("limit", "integer", "Groups per page."),
            ("model", "string", "A Model name."),
            ("project", "string", "A project name."),
            ("team", "string", "A team name."),
            ("organization", "string", "An organization name."),
        ],
    },
    Tool {
        name: "spend_summary",
        description: "Sums settled spend from the ledger over a time range, optionally grouped.",
        permission: "analytics:read",
        args: &[
            ("from", "string", "Start of the time range (RFC 3339)."),
            ("to", "string", "End of the time range (RFC 3339)."),
            (
                "group_by",
                "string",
                "Comma-separated attributes, such as model,team.",
            ),
            ("limit", "integer", "Groups per page."),
        ],
    },
];

fn holds(call: &Call, permission: &str) -> bool {
    call.principal.bindings.iter().any(|b| b.role.allows(permission))
}

fn definition(t: &Tool) -> Value {
    let props: Map<String, Value> = t
        .args
        .iter()
        .map(|(n, ty, d)| ((*n).to_owned(), json!({"type": ty, "description": d})))
        .collect();
    let required: Vec<&str> = if t.name == "get_request" {
        vec!["id"]
    } else {
        Vec::new()
    };
    json!({
        "name": t.name,
        "description": t.description,
        "inputSchema": {"type": "object", "properties": props, "required": required, "additionalProperties": false},
        "annotations": {"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false},
    })
}

fn rpc_result(id: &Value, result: Value) -> Response {
    json_response(
        StatusCode::OK,
        &json!({"jsonrpc": "2.0", "id": id, "result": result}),
    )
}

fn rpc_error(status: StatusCode, id: &Value, code: i64, message: &str) -> Response {
    json_response(
        status,
        &json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
    )
}

fn tool_text(value: &Value, is_error: bool) -> Value {
    let mut out = json!({
        "content": [{"type": "text", "text": value.to_string()}],
        "isError": is_error,
    });
    if !is_error {
        out["structuredContent"] = value.clone();
    }
    out
}

/// [`serve`], built and boxed in a frame of its own so the router's frame stays the size it was.
pub(crate) fn serve_boxed(call: &Call) -> Handled<'_> {
    Box::pin(serve(call))
}

/// Serves one JSON-RPC message.
async fn serve(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    let Ok(msg) = serde_json::from_slice::<Value>(&call.body) else {
        return Ok(rpc_error(
            StatusCode::BAD_REQUEST,
            &Value::Null,
            -32700,
            "Parse error.",
        ));
    };
    let Value::Object(msg) = msg else {
        return Ok(rpc_error(
            StatusCode::BAD_REQUEST,
            &Value::Null,
            -32600,
            "One JSON-RPC message per request; batches are not served.",
        ));
    };
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
    let Some(id) = id else {
        let mut resp = Response::new(axum::body::Body::empty());
        *resp.status_mut() = StatusCode::ACCEPTED;
        return Ok(resp);
    };
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let modern = call.header("mcp-protocol-version") == Some(MODERN);
    let mut result = match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str);
            let version = asked
                .filter(|v| LEGACY.contains(v))
                .unwrap_or(LEGACY.last().copied().unwrap_or_default());
            json!({
                "protocolVersion": version,
                "capabilities": {"tools": {}},
                "serverInfo": server_info(),
                "instructions": "Read-only access to the gateway's request log, usage and spend.",
            })
        }
        "server/discover" => json!({
            "supportedVersions": LEGACY.iter().copied().chain([MODERN]).collect::<Vec<_>>(),
            "capabilities": {"tools": {}},
            "serverInfo": server_info(),
        }),
        "ping" => json!({}),
        "tools/list" => json!({
            "tools": TOOLS.iter().filter(|t| holds(call, t.permission)).map(definition).collect::<Vec<_>>(),
        }),
        "tools/call" => call_tool(call, &params).await,
        _ => return Ok(rpc_error(StatusCode::OK, &id, -32601, "Method not found.")),
    };
    if modern && let Some(o) = result.as_object_mut() {
        o.insert("resultType".into(), Value::from("complete"));
    }
    Ok(rpc_result(&id, result))
}

fn server_info() -> Value {
    json!({"name": "antirion-admin", "version": antirion_core::headers::BUILD_VERSION})
}

/// The query parameters of a tool's arguments: strings, numbers and booleans as they are, each
/// element of an array as a repeated parameter.
fn query_of(args: &Map<String, Value>) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for (k, v) in args {
        let mut one = |v: &Value| match v {
            Value::String(s) => {
                out.push((k.clone(), s.clone()));
                Ok(())
            }
            Value::Number(n) => {
                out.push((k.clone(), n.to_string()));
                Ok(())
            }
            Value::Bool(b) => {
                out.push((k.clone(), b.to_string()));
                Ok(())
            }
            _ => Err(format!("argument {k} must be a string, a number or a boolean")),
        };
        match v {
            Value::Array(items) => items.iter().try_for_each(&mut one)?,
            Value::Null => {}
            other => one(other)?,
        }
    }
    Ok(out)
}

async fn call_tool(call: &Call, params: &Value) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
    let Some(tool) = TOOLS.iter().find(|t| t.name == name) else {
        return tool_text(&json!({"error": format!("unknown tool {name:?}")}), true);
    };
    let args = params
        .get("arguments")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(bad) = args.keys().find(|k| !tool.args.iter().any(|(n, _, _)| n == k)) {
        return tool_text(&json!({"error": format!("unknown argument {bad:?}")}), true);
    }
    let mut query = match query_of(&args) {
        Ok(q) => q,
        Err(e) => return tool_text(&json!({"error": e}), true),
    };
    if tool.name == "search_logs" {
        let limit = query
            .iter()
            .find(|(k, _)| k == "limit")
            .and_then(|(_, v)| v.parse::<u32>().ok());
        if limit.is_none_or(|l| l > 100) {
            query.retain(|(k, _)| k != "limit");
            query.push(("limit".into(), limit.map_or(20, |l| l.min(100)).to_string()));
        }
    }
    let id = query
        .iter()
        .find(|(k, _)| k == "id")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    if tool.name == "get_request" {
        query.retain(|(k, _)| k != "id");
    }
    let sub = Call {
        rt: std::sync::Arc::clone(&call.rt),
        snap: std::sync::Arc::clone(&call.snap),
        principal: call.principal.clone(),
        session: call.session.clone(),
        request_id: call.request_id.clone(),
        source_ip: call.source_ip.clone(),
        method: Method::GET,
        query,
        headers: call.headers.clone(),
        body: Bytes::new(),
        reason: None,
        path: PATH.to_owned(),
        raw_query: None,
        approved: false,
    };
    let answer = match tool.name {
        "search_logs" => crate::logs::list(&sub).await,
        "get_request" => crate::logs::get(&sub, &id).await,
        "usage_summary" => crate::analytics::usage(&sub).await,
        _ => crate::spend::summary(&sub).await,
    };
    match answer {
        Ok(resp) => {
            let status = resp.status();
            let bytes = axum::body::to_bytes(resp.into_body(), 64 << 20)
                .await
                .unwrap_or_default();
            let v = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
            tool_text(&v, !status.is_success())
        }
        Err(e) => {
            if e.status == StatusCode::FORBIDDEN {
                crate::audit_denied(
                    &call.rt,
                    call.actor(),
                    &Method::POST,
                    &format!("{PATH}#{}", tool.name),
                    e.status,
                    crate::Origin {
                        request_id: &call.request_id,
                        ip: call.source_ip.clone(),
                        reason: None,
                    },
                )
                .await;
            }
            tool_text(
                &json!({"error": {"status": e.status.as_u16(), "code": e.code, "message": e.message}}),
                true,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_become_query_parameters() {
        let args = json!({"model": "m", "limit": 5, "stream": true, "key": ["a", "b"], "q": null});
        let q = query_of(args.as_object().unwrap()).unwrap();
        assert_eq!(
            q,
            [
                ("model".to_owned(), "m".to_owned()),
                ("limit".to_owned(), "5".to_owned()),
                ("stream".to_owned(), "true".to_owned()),
                ("key".to_owned(), "a".to_owned()),
                ("key".to_owned(), "b".to_owned()),
            ]
        );
        assert!(query_of(json!({"x": {"a": 1}}).as_object().unwrap()).is_err());
    }

    #[test]
    fn every_tool_is_read_only_and_described() {
        for t in TOOLS {
            let d = definition(t);
            assert_eq!(d["annotations"]["readOnlyHint"], true);
            assert_eq!(d["inputSchema"]["type"], "object");
        }
    }
}
