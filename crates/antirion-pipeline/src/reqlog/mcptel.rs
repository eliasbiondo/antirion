//! The telemetry of an MCP request, after the OpenTelemetry MCP conventions: the SERVER span named
//! `"{mcp.method.name} {target}"` with the client-facing tool or prompt, one CLIENT span per upstream
//! request with the upstream names, and the MCP metric families.

use std::time::Instant;

use antirion_mcp::{LogFacts, UpstreamCall};
use antirion_obs::trace::{SpanData, SpanKind, Status, Value as Attr};
use antirion_obs::{Fam, Metrics};
use serde_json::Value;

use crate::trace::TraceCx;

/// One upstream request as a span.
#[derive(Clone, Debug)]
struct Client {
    name: String,
    attrs: Vec<(&'static str, Attr)>,
    start: Instant,
    end: Instant,
    error: bool,
    server: String,
    method: String,
}

/// What an MCP or A2A request adds to its spans and metrics.
#[derive(Clone, Debug, Default)]
pub(crate) struct McpTelemetry {
    name: Option<String>,
    attrs: Vec<(&'static str, Attr)>,
    clients: Vec<Client>,
}

fn id_text(id: &Value) -> Option<String> {
    match id {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn span_name(method: &str, target: Option<&str>) -> String {
    match target.filter(|_| matches!(method, "tools/call" | "prompts/get")) {
        Some(t) => format!("{method} {t}"),
        None => method.to_owned(),
    }
}

/// The attributes of a method addressing `item`.
fn item_attrs(method: &str, item: Option<&str>, out: &mut Vec<(&'static str, Attr)>) {
    let Some(item) = item else { return };
    match method {
        "tools/call" => {
            out.push(("gen_ai.tool.name", item.into()));
            out.push(("gen_ai.operation.name", "execute_tool".into()));
        }
        "prompts/get" => out.push(("gen_ai.prompt.name", item.into())),
        m if m.starts_with("resources/") => out.push(("mcp.resource.uri", item.into())),
        _ => {}
    }
}

fn client(call: &UpstreamCall, request_id: Option<&str>) -> Client {
    let mut attrs = vec![
        ("mcp.method.name", Attr::from(call.method.as_str())),
        ("antirion.mcp.server", call.server.as_str().into()),
    ];
    if let Some(id) = request_id {
        attrs.push(("jsonrpc.request.id", id.into()));
    }
    item_attrs(&call.method, call.item.as_deref(), &mut attrs);
    if let Some(code) = call.error_code {
        attrs.push(("error.type", code.to_string().into()));
        attrs.push(("rpc.response.status_code", code.to_string().into()));
    } else if call.failed {
        attrs.push(("error.type", "upstream_failed".into()));
    }
    Client {
        name: span_name(&call.method, call.item.as_deref()),
        attrs,
        start: call.start,
        end: call.end,
        error: call.failed || call.error_code.is_some(),
        server: call.server.clone(),
        method: call.method.clone(),
    }
}

impl McpTelemetry {
    /// The telemetry of an MCP gateway request.
    pub(crate) fn mcp(f: &LogFacts) -> McpTelemetry {
        let Some(method) = f.method.as_deref() else {
            return McpTelemetry::default();
        };
        let request_id = f.request_id.as_ref().and_then(id_text);
        let mut attrs = vec![("mcp.method.name", Attr::from(method))];
        if let Some(v) = &f.protocol_version {
            attrs.push(("mcp.protocol.version", v.as_str().into()));
        }
        if let Some(id) = &request_id {
            attrs.push(("jsonrpc.request.id", id.as_str().into()));
        }
        item_attrs(method, f.client_item.as_deref(), &mut attrs);
        if let Some(code) = f.jsonrpc_error_code {
            attrs.push(("rpc.response.status_code", code.to_string().into()));
            if f.error.is_none() {
                attrs.push(("error.type", code.to_string().into()));
            }
        }
        if let Some(n) = f.listed.filter(|_| method.ends_with("/list")) {
            attrs.push(("antirion.mcp.items", i64::try_from(n).unwrap_or(i64::MAX).into()));
        }
        if f.legacy
            && let Some(sid) = &f.session_id
        {
            attrs.push(("mcp.session.id", sid.as_str().into()));
        }
        McpTelemetry {
            name: Some(span_name(method, f.client_item.as_deref())),
            attrs,
            clients: f
                .upstream
                .iter()
                .map(|c| client(c, request_id.as_deref()))
                .collect(),
        }
    }

    /// The telemetry of an A2A request.
    pub(crate) fn a2a(f: &LogFacts) -> McpTelemetry {
        let mut attrs = Vec::new();
        if let Some(m) = &f.method {
            attrs.push(("a2a.method", Attr::from(m.as_str())));
        }
        if let Some(t) = &f.task_id {
            attrs.push(("a2a.task_id", t.as_str().into()));
        }
        if let Some(id) = f.request_id.as_ref().and_then(id_text) {
            attrs.push(("jsonrpc.request.id", id.into()));
        }
        McpTelemetry {
            name: None,
            attrs,
            clients: Vec::new(),
        }
    }

    /// Names and decorates the request's SERVER span.
    pub(crate) fn decorate(&self, root: &mut SpanData) {
        if let Some(n) = &self.name {
            root.name.clone_from(n);
        }
        for (k, v) in &self.attrs {
            root.set(k, v.clone());
        }
    }

    /// One CLIENT span per upstream request.
    pub(crate) fn client_spans(&self, t: &TraceCx) -> Vec<SpanData> {
        self.clients
            .iter()
            .map(|c| {
                let mut s = t.child(c.name.clone(), SpanKind::Client, c.start, c.end);
                for (k, v) in &c.attrs {
                    s.set(k, v.clone());
                }
                if c.error {
                    s.status = Status::Error(String::new());
                }
                s
            })
            .collect()
    }

    /// `antirion_mcp_requests_total` and `antirion_mcp_request_duration_seconds` per upstream request.
    pub(crate) fn metrics(&self, m: &Metrics) {
        for c in &self.clients {
            let outcome = if c.error { "error" } else { "ok" };
            m.add(Fam::McpRequests, &[&c.server, &c.method, outcome], 1.0);
            m.observe(
                Fam::McpRequestDuration,
                &[&c.server, &c.method],
                c.end.saturating_duration_since(c.start).as_secs_f64(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn names_follow_the_target() {
        assert_eq!(span_name("tools/call", Some("a__add")), "tools/call a__add");
        assert_eq!(span_name("tools/list", None), "tools/list");
        assert_eq!(span_name("resources/read", Some("file:///x")), "resources/read");
        assert_eq!(id_text(&json!(7)).as_deref(), Some("7"));
        assert_eq!(id_text(&Value::Null), None);
    }

    #[test]
    fn legacy_sessions_and_errors_are_attributes() {
        let f = LogFacts {
            method: Some("tools/call".into()),
            client_item: Some("a__add".into()),
            session_id: Some("tok".into()),
            legacy: true,
            jsonrpc_error_code: Some(-32602),
            ..LogFacts::default()
        };
        let t = McpTelemetry::mcp(&f);
        let keys: Vec<&str> = t.attrs.iter().map(|(k, _)| *k).collect();
        assert!(keys.contains(&"mcp.session.id") && keys.contains(&"gen_ai.tool.name"));
        assert!(keys.contains(&"rpc.response.status_code") && keys.contains(&"error.type"));
        let modern = McpTelemetry::mcp(&LogFacts { legacy: false, ..f });
        assert!(!modern.attrs.iter().any(|(k, _)| *k == "mcp.session.id"));
    }
}
