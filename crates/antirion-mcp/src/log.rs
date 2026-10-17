//! The facts of one MCP or A2A request that its request-log record carries, gathered while it runs.

use std::time::Instant;

use antirion_error::GatewayError;
use parking_lot::Mutex;
use serde_json::{Value, json};

/// What one request learned for its log record.
#[derive(Clone, Debug, Default)]
pub struct LogFacts {
    /// The JSON-RPC method the client called.
    pub method: Option<String>,
    /// The upstream server that owned the item.
    pub server: Option<String>,
    /// The upstream name of the tool, prompt or resource.
    pub item: Option<String>,
    /// The client-facing session id (legacy era only).
    pub session_id: Option<String>,
    pub task_id: Option<String>,
    /// The JSON-RPC error code the client received.
    pub jsonrpc_error_code: Option<i64>,
    /// The gateway error the client received, when the gateway raised one.
    pub error: Option<GatewayError>,
    /// The JSON-RPC id of the client's request.
    pub request_id: Option<Value>,
    /// The protocol version of the exchange with the client.
    pub protocol_version: Option<String>,
    /// The tool or prompt name, or resource URI, as the client sent it.
    pub client_item: Option<String>,
    /// How many items a list answer carried.
    pub listed: Option<usize>,
    /// The request was in the legacy era.
    pub legacy: bool,
    /// The requests made to upstream servers for this one.
    pub upstream: Vec<UpstreamCall>,
}

/// One request to an upstream server.
#[derive(Clone, Debug)]
pub struct UpstreamCall {
    pub server: String,
    pub method: String,
    /// The upstream tool or prompt name, or resource URI.
    pub item: Option<String>,
    pub start: Instant,
    pub end: Instant,
    /// The JSON-RPC error code the server answered with.
    pub error_code: Option<i64>,
    /// The exchange failed (no usable answer).
    pub failed: bool,
}

impl LogFacts {
    /// The record's `mcp` member.
    #[must_use]
    pub fn mcp_json(&self) -> Value {
        let tool = self
            .method
            .as_deref()
            .is_some_and(|m| m == "tools/call")
            .then(|| self.item.clone())
            .flatten();
        json!({
            "server": self.server,
            "method": self.method,
            "tool": tool,
            "session_id": self.session_id,
            "jsonrpc_error_code": self.jsonrpc_error_code,
        })
    }

    /// The record's `a2a` member.
    #[must_use]
    pub fn a2a_json(&self) -> Value {
        json!({
            "method": self.method,
            "task_id": self.task_id,
            "jsonrpc_error_code": self.jsonrpc_error_code,
        })
    }
}

/// The facts of one request, shared by the handlers that serve it.
#[derive(Debug, Default)]
pub struct CallLog(Mutex<LogFacts>);

impl CallLog {
    pub(crate) fn update(&self, f: impl FnOnce(&mut LogFacts)) {
        f(&mut self.0.lock());
    }

    /// Records the JSON-RPC error of an upstream answer relayed to the client.
    pub(crate) fn upstream_answer(&self, msg: &Value) {
        if let Some(code) = msg
            .get("error")
            .and_then(|e| e.get("code"))
            .and_then(Value::as_i64)
        {
            self.update(|f| f.jsonrpc_error_code = Some(code));
        }
    }

    /// Records one request to an upstream server.
    pub(crate) fn upstream(&self, call: UpstreamCall) {
        self.update(|f| f.upstream.push(call));
    }

    #[must_use]
    pub fn facts(&self) -> LogFacts {
        self.0.lock().clone()
    }
}
