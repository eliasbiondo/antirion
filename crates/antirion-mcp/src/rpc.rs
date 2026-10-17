//! JSON-RPC 2.0 messages and the error answers of MCP and A2A endpoints: gateway conditions take the
//! code table's status and JSON-RPC code; protocol errors keep the protocol's own status.

use antirion_core::{Envelope, RequestId, Stage};
use antirion_error::{GatewayError, Message, Reason, UpstreamFacts};
use serde_json::{Map, Value, json};

use crate::reasons;

/// One JSON-RPC message from a client or an upstream.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Msg {
    Request {
        id: Value,
        method: String,
        params: Option<Value>,
    },
    Notification {
        method: String,
        params: Option<Value>,
    },
    Response {
        id: Value,
    },
}

impl Msg {
    pub(crate) fn method(&self) -> Option<&str> {
        match self {
            Msg::Request { method, .. } | Msg::Notification { method, .. } => Some(method),
            Msg::Response { .. } => None,
        }
    }

    pub(crate) fn id(&self) -> Option<&Value> {
        match self {
            Msg::Request { id, .. } | Msg::Response { id } => Some(id),
            Msg::Notification { .. } => None,
        }
    }

    pub(crate) fn params(&self) -> Option<&Value> {
        match self {
            Msg::Request { params, .. } | Msg::Notification { params, .. } => params.as_ref(),
            Msg::Response { .. } => None,
        }
    }
}

fn valid_id(id: &Value) -> bool {
    matches!(id, Value::String(_) | Value::Number(_) | Value::Null)
}

/// Classifies a JSON value; on failure, the id to echo (when the value carried a usable one).
pub(crate) fn classify(v: &Value) -> Result<Msg, Value> {
    let Some(obj) = v.as_object() else {
        return Err(Value::Null);
    };
    let id = obj.get("id").filter(|i| valid_id(i)).cloned();
    let echo = id.clone().unwrap_or(Value::Null);
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(echo);
    }
    let params = obj.get("params").cloned();
    match (obj.get("method"), id) {
        (Some(Value::String(m)), Some(id)) => Ok(Msg::Request {
            id,
            method: m.clone(),
            params,
        }),
        (Some(Value::String(m)), None) if !obj.contains_key("id") => Ok(Msg::Notification {
            method: m.clone(),
            params,
        }),
        (Some(_), _) => Err(echo),
        (None, Some(id)) if obj.contains_key("result") || obj.contains_key("error") => {
            Ok(Msg::Response { id })
        }
        (None, _) => Err(echo),
    }
}

/// A failed exchange, ready to render as a JSON-RPC error. Boxed, so results stay small.
#[derive(Clone, Debug)]
pub(crate) struct Fail(Box<FailBody>);

/// What a [`Fail`] holds.
#[derive(Clone, Debug)]
pub(crate) struct FailBody {
    pub(crate) err: GatewayError,
    /// The JSON-RPC code when it is not the code table's (protocol errors).
    pub(crate) rpc_code: Option<i32>,
    /// The HTTP status when it is not the code table's (protocol errors).
    pub(crate) status: Option<u16>,
    /// Protocol-defined members of `error.data`.
    pub(crate) data: Map<String, Value>,
    pub(crate) id: Value,
}

impl Fail {
    /// A gateway condition with a registered reason.
    pub(crate) fn gateway(reason: Reason, stage: Stage, message: Message) -> Fail {
        Fail::from(GatewayError::with_reason(reason, stage, message))
    }

    /// A protocol error: the protocol's JSON-RPC code and HTTP status.
    pub(crate) fn protocol(code: i32, status: u16, reason: Reason, stage: Stage, message: Message) -> Fail {
        let mut f = Fail::gateway(reason, stage, message);
        f.rpc_code = Some(code);
        f.status = Some(status);
        f
    }

    pub(crate) fn id(mut self, id: &Value) -> Fail {
        self.id = id.clone();
        self
    }

    pub(crate) fn data(mut self, key: &str, value: Value) -> Fail {
        self.data.insert(key.to_owned(), value);
        self
    }

    pub(crate) fn provider(mut self, name: &str) -> Fail {
        self.err.attempts = self.err.attempts.max(1);
        let facts = self.err.upstream.get_or_insert_with(|| {
            Box::new(UpstreamFacts {
                provider: String::new(),
                status: None,
                failure_class: "upstream_error",
                client_status: None,
                error: None,
            })
        });
        facts.provider = name.to_owned();
        self
    }

    pub(crate) fn upstream_status(mut self, status: u16) -> Fail {
        if let Some(u) = self.err.upstream.as_mut() {
            u.status = Some(status);
        }
        self
    }

    /// The unknown-method answer: HTTP 200 in the legacy era and A2A, 404 in the modern era.
    pub(crate) fn unknown_method(method: &str, status: u16) -> Fail {
        Fail::protocol(
            -32601,
            status,
            reasons::UNKNOWN_METHOD,
            Stage::Parse,
            Message::built(format!("method not found: {method}")),
        )
    }

    pub(crate) fn invalid_params(message: &'static str) -> Fail {
        Fail::protocol(
            -32602,
            200,
            reasons::INVALID_PARAMS,
            Stage::Parse,
            Message::Fixed(message),
        )
    }

    pub(crate) fn jsonrpc_code(&self) -> i32 {
        self.rpc_code.unwrap_or_else(|| self.err.code.jsonrpc_code())
    }

    pub(crate) fn status(&self) -> u16 {
        self.status.unwrap_or_else(|| self.err.status())
    }

    /// The error body and the headers every error carries.
    pub(crate) fn render(&self, request_id: &RequestId) -> (u16, http::HeaderMap, Value) {
        let rendered = antirion_error::render(&self.err, Envelope::JsonRpc, request_id, Some(&self.id));
        let mut data = self.data.clone();
        data.insert(
            "antirion".into(),
            antirion_error::antirion_object(&self.err, request_id),
        );
        let body = json!({
            "jsonrpc": "2.0",
            "id": self.id,
            "error": {"code": self.jsonrpc_code(), "message": self.err.message.as_str(), "data": data},
        });
        (self.status(), rendered.headers, body)
    }
}

impl From<GatewayError> for Fail {
    fn from(err: GatewayError) -> Fail {
        Fail(Box::new(FailBody {
            err,
            rpc_code: None,
            status: None,
            data: Map::new(),
            id: Value::Null,
        }))
    }
}

impl std::ops::Deref for Fail {
    type Target = FailBody;

    fn deref(&self) -> &FailBody {
        &self.0
    }
}

impl std::ops::DerefMut for Fail {
    fn deref_mut(&mut self) -> &mut FailBody {
        &mut self.0
    }
}

/// The credential values sent to an upstream, as they could be echoed back: each whole header value
/// and, for `scheme token` values, the token. Values too short to be credentials are left out.
pub(crate) fn credentials(headers: &http::HeaderMap) -> Vec<String> {
    let mut out = Vec::new();
    for v in headers.values().filter_map(|v| v.to_str().ok()) {
        out.push(v.to_owned());
        if let Some((_, token)) = v.split_once(' ') {
            out.push(token.trim().to_owned());
        }
    }
    out.retain(|s| s.len() >= 8);
    out.sort_by_key(|s| std::cmp::Reverse(s.len()));
    out
}

/// Replaces every credential value inside the strings of `v`.
fn scrub(v: &mut Value, secrets: &[String]) {
    match v {
        Value::String(s) => {
            for secret in secrets {
                if s.contains(secret.as_str()) {
                    *s = s.replace(secret.as_str(), "[REDACTED]");
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|i| scrub(i, secrets)),
        Value::Object(map) => map.values_mut().for_each(|i| scrub(i, secrets)),
        _ => {}
    }
}

/// Adds `error.data.antirion` to an upstream's JSON-RPC error when its `data` is absent or an object
/// (any other `data` is left as it is), after removing any credential the upstream echoed.
pub(crate) fn decorate_upstream_error(
    msg: &mut Value,
    provider: &str,
    request_id: &RequestId,
    secrets: &[String],
) {
    let Some(error) = msg.get_mut("error").and_then(Value::as_object_mut) else {
        return;
    };
    for value in error.values_mut() {
        scrub(value, secrets);
    }
    let mut err = GatewayError::with_reason(
        reasons::UPSTREAM_JSONRPC_ERROR,
        Stage::Attempt,
        Message::Fixed("The upstream answered with an error."),
    );
    err.attempts = 1;
    err.upstream = Some(Box::new(UpstreamFacts {
        provider: provider.to_owned(),
        status: Some(200),
        failure_class: "upstream_error",
        client_status: Some(200),
        error: None,
    }));
    let antirion = antirion_error::antirion_object(&err, request_id);
    match error.get_mut("data") {
        None => {
            error.insert("data".into(), json!({"antirion": antirion}));
        }
        Some(Value::Object(data)) => {
            data.insert("antirion".into(), antirion);
        }
        Some(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_keeps_ids() {
        assert_eq!(classify(&json!({"foo": 1})), Err(Value::Null));
        assert_eq!(classify(&json!([])), Err(Value::Null));
        assert_eq!(
            classify(&json!({"jsonrpc": "1.0", "id": 5, "method": "x"})),
            Err(json!(5))
        );
        assert_eq!(classify(&json!({"id": "a", "method": "x"})), Err(json!("a")));
        assert_eq!(
            classify(&json!({"jsonrpc": "2.0", "id": 7, "method": 12})),
            Err(json!(7))
        );
        assert!(matches!(
            classify(&json!({"jsonrpc": "2.0", "id": 0, "method": "x"})),
            Ok(Msg::Request { id, .. }) if id == json!(0)
        ));
        assert!(matches!(
            classify(&json!({"jsonrpc": "2.0", "method": "n"})),
            Ok(Msg::Notification { .. })
        ));
        assert!(matches!(
            classify(&json!({"jsonrpc": "2.0", "id": "x", "result": {}})),
            Ok(Msg::Response { .. })
        ));
    }

    #[test]
    fn large_ids_survive() {
        let v: Value =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":9007199254740993,"method":"m"}"#).unwrap();
        let Ok(Msg::Request { id, .. }) = classify(&v) else {
            panic!()
        };
        assert_eq!(serde_json::to_string(&id).unwrap(), "9007199254740993");
    }

    #[test]
    fn upstream_errors_get_antirion_data_only_when_data_is_an_object() {
        let rid = RequestId::generate();
        let mut a = json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32001, "message": "x"}});
        decorate_upstream_error(&mut a, "s", &rid, &[]);
        assert_eq!(a["error"]["data"]["antirion"]["code"], "upstream_rejected");
        let mut b = json!({"jsonrpc": "2.0", "id": 1, "error": {"code": 1, "message": "x", "data": [1]}});
        decorate_upstream_error(&mut b, "s", &rid, &[]);
        assert_eq!(b["error"]["data"], json!([1]));
    }

    #[test]
    fn echoed_credentials_are_removed() {
        let mut h = http::HeaderMap::new();
        h.insert(
            "authorization",
            http::HeaderValue::from_static("Bearer sekret-token-123"),
        );
        let secrets = credentials(&h);
        let mut m = json!({"jsonrpc": "2.0", "id": 1, "error": {"code": 1, "message": "bad key sekret-token-123",
                           "data": {"auth": "Bearer sekret-token-123"}}});
        decorate_upstream_error(&mut m, "s", &RequestId::generate(), &secrets);
        let text = m.to_string();
        assert!(!text.contains("sekret-token-123"), "{text}");
        assert_eq!(m["error"]["message"], "bad key [REDACTED]");
    }

    #[test]
    fn protocol_errors_keep_their_status_and_code() {
        let f = Fail::unknown_method("tools/x", 200).id(&json!(3));
        let (status, headers, body) = f.render(&RequestId::generate());
        assert_eq!(status, 200);
        assert_eq!(body["error"]["code"], -32601);
        assert_eq!(body["id"], 3);
        assert_eq!(body["error"]["data"]["antirion"]["reason"], "unknown_method");
        assert_eq!(headers["content-type"], "application/json");
    }
}
