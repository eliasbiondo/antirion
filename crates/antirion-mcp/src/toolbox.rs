//! MCP tools offered to model requests (the server-side tool loop): the caller's granted tools of
//! the listed servers, named `<prefix>__<tool>` as on a multiplexed gateway and turned into function
//! names models accept, and their execution as `tools/call` under the caller's grants, limits and
//! guardrails. Each use opens and closes its upstream session, so nothing outlives the request.

use std::sync::Arc;
use std::time::Duration;

use http::HeaderMap;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::caller::Caller;
use crate::compile::{GatewayRt, ServerInfo, SessionKey};
use crate::http::{Env, Req};
use crate::lists::{self, Kind, Mode};
use crate::names;
use crate::select::{ItemKind, Selection};
use crate::sse::Parser;
use crate::token::Keyring;
use crate::upstream::LATEST_LEGACY;

/// The longest function name model APIs accept.
const MAX_NAME: usize = 64;

/// One MCP tool offered to the model.
#[derive(Clone, Debug, PartialEq)]
pub struct OfferedTool {
    /// The function name the model sees.
    pub function: String,
    /// `<prefix>__<tool>`, what `auto_execute` globs match.
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
}

/// The name a model accepts for `name`: characters outside `[A-Za-z0-9_-]` become `_`, and a name
/// longer than 64 characters is cut to 55, followed by `_` and 8 hex digits of its SHA-256.
#[must_use]
pub fn function_name(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if clean.chars().count() <= MAX_NAME && !clean.is_empty() {
        return clean;
    }
    let digest: String = Sha256::digest(name.as_bytes())
        .iter()
        .take(4)
        .map(|b| format!("{b:02x}"))
        .collect();
    let head: String = clean.chars().take(55).collect();
    format!("{head}_{digest}")
}

/// The MCP tools of one model request.
#[derive(Debug)]
pub struct Toolbox {
    req: Req,
    sel: Selection,
    tools: Vec<OfferedTool>,
}

/// A JSON-RPC answer read from a response body, JSON or SSE.
async fn answer_of(resp: http::Response<crate::Body>) -> Option<Value> {
    let streamed = resp
        .headers()
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.starts_with("text/event-stream"));
    let bytes = resp.into_body().collect().await.ok()?.to_bytes();
    if !streamed {
        return serde_json::from_slice(&bytes).ok();
    }
    let mut parser = Parser::default();
    parser.push(&bytes);
    parser.push(b"\n\n");
    let mut last = None;
    while let Some(ev) = parser.next_event() {
        if let Ok(v) = serde_json::from_str::<Value>(&ev.data)
            && (v.get("result").is_some() || v.get("error").is_some())
        {
            last = Some(v);
        }
    }
    last
}

impl Toolbox {
    /// The caller's granted tools of `servers` (unknown, disabled and ungranted servers are left
    /// out); `None` when none is left.
    pub async fn open(env: Env, caller: Caller, servers: &[String]) -> Option<Toolbox> {
        let mut keys = env.state.keys(&env.snap.session_keys);
        if keys.is_empty() {
            let mut secret = [0u8; 32];
            getrandom::fill(&mut secret).ok()?;
            keys.push(SessionKey { id: 0, secret });
        }
        let gw = Arc::new(GatewayRt {
            name: "mcp_tools".to_owned(),
            servers: servers.to_vec(),
            enabled: true,
            prefixed: true,
            stateless: true,
            fail_closed: false,
            server_info: ServerInfo::Upstream,
            client_cache_max_ttl: Duration::ZERO,
            allowed_origins: Vec::new(),
            max_body: u64::MAX,
            oauth: None,
            restricted: true,
            modern_bridge: false,
            tool_search: false,
        });
        let req = Req {
            env,
            gw,
            caller,
            headers: HeaderMap::new(),
            keys: Keyring::new(keys)?,
        };
        let sel = Selection::new(&req).ok()?;
        if sel.servers.is_empty() {
            return None;
        }
        let mode = Mode::Stateless {
            pv: LATEST_LEGACY.to_owned(),
        };
        let mut tools = Vec::new();
        for (server, entry) in lists::entries(&req, &mode, &sel.servers, Kind::Tools, false).await {
            let Ok(entry) = entry else { continue };
            for t in entry.items.iter() {
                let Some(upstream) = t.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let name = names::name_out(&server.prefix, upstream);
                if !sel.visible(&req.env.state, &server, ItemKind::Tool, upstream, &name, Some(t)) {
                    continue;
                }
                tools.push(OfferedTool {
                    function: function_name(&name),
                    name,
                    description: t.get("description").and_then(Value::as_str).map(str::to_owned),
                    input_schema: t
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type": "object"})),
                });
            }
        }
        (!tools.is_empty()).then_some(Toolbox { req, sel, tools })
    }

    #[must_use]
    pub fn tools(&self) -> &[OfferedTool] {
        &self.tools
    }

    /// The offered tool a function name stands for.
    #[must_use]
    pub fn by_function(&self, function: &str) -> Option<&OfferedTool> {
        self.tools.iter().find(|t| t.function == function)
    }

    /// Runs a tool and returns the text the model gets back: the result's text items, or the error.
    pub async fn call(&self, tool: &OfferedTool, arguments: Value, id: u64) -> (String, bool) {
        let params = json!({"name": tool.name, "arguments": arguments});
        let mode = Mode::Stateless {
            pv: LATEST_LEGACY.to_owned(),
        };
        let rid = Value::from(format!("antirion-tool-{id}"));
        let resp = crate::route::forward(
            &self.req,
            &self.sel,
            &mode,
            &rid,
            "tools/call",
            Some(&params),
            None,
        )
        .await;
        let Some(msg) = answer_of(resp).await else {
            return ("The tool did not answer.".to_owned(), true);
        };
        if let Some(e) = msg.get("error") {
            let text = e
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("The tool call failed.")
                .to_owned();
            return (text, true);
        }
        let result = msg.get("result").cloned().unwrap_or(Value::Null);
        let failed = result.get("isError").and_then(Value::as_bool).unwrap_or(false);
        let texts: Vec<String> = result
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|c| c.get("text").and_then(Value::as_str).map(str::to_owned))
            .collect();
        let text = if texts.is_empty() {
            result
                .get("structuredContent")
                .map(Value::to_string)
                .unwrap_or_default()
        } else {
            texts.join("\n")
        };
        (text, failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_names_are_sanitised_and_bounded() {
        assert_eq!(function_name("github__get_issue"), "github__get_issue");
        assert_eq!(function_name("a__weird name.x"), "a__weird_name_x");
        let long = format!("srv__{}", "x".repeat(80));
        let f = function_name(&long);
        assert_eq!(f.chars().count(), 64);
        assert!(f.starts_with("srv__xxx"));
        assert_eq!(&f[55..56], "_");
        assert_ne!(
            function_name(&format!("{long}y")),
            f,
            "the digest tells long names apart"
        );
    }
}
