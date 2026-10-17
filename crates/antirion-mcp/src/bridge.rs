//! Era bridging. With `modern_bridge` on a gateway, a modern request that needs a legacy-only server
//! is served through a session opened for it alone (`initialize` → request → `DELETE`), and a server
//! request the legacy server sends during the call (elicitation, sampling, roots) becomes an
//! input-required result: the client's retry runs the call again in a new session and answers the
//! server's requests, in order, from `inputResponses`. With `protocol: modern` on a server, a legacy
//! client's `initialize` is answered from the server's `server/discover`, and the session's later
//! requests are sent as modern requests carrying the session's client capabilities and info.

use std::sync::Arc;
use std::time::Duration;

use antirion_core::Stage;
use antirion_error::Message;
use serde_json::{Map, Value, json};

use crate::compile::ServerRt;
use crate::connect::{gateway_client_info, open_with};
use crate::http::{Env, Req};
use crate::link::{Link, next_message};
use crate::modern::{META_CAPS, META_PV};
use crate::reasons;
use crate::rpc::Fail;
use crate::select::upstream_headers;
use crate::token::CursorClaims;
use crate::upstream::{Answer, Era, LATEST_LEGACY, MODERN, UpError};

pub(crate) const META_INFO: &str = "io.modelcontextprotocol/clientInfo";
/// Methods a server sends to the client that a bridged call turns into input requests.
const INPUT_METHODS: [&str; 3] = ["elicitation/create", "sampling/createMessage", "roots/list"];

/// The modern `_meta` of a legacy session's requests.
pub(crate) fn session_meta(caps: &Value, client_info: &Value) -> Value {
    json!({META_PV: MODERN, META_CAPS: caps, META_INFO: client_info})
}

/// Opens a legacy session to `server` for one modern request: the client's capabilities and info
/// come from its `_meta`.
pub(crate) async fn open_for_modern(
    req: &Req,
    server: &Arc<ServerRt>,
    meta: &Value,
) -> Result<(Link, Value), UpError> {
    let caps = meta.get(META_CAPS).cloned().unwrap_or_else(|| json!({}));
    let info = meta.get(META_INFO).cloned().unwrap_or_else(gateway_client_info);
    let headers = upstream_headers(req, server);
    open_with(&req.env, server, headers, LATEST_LEGACY, &caps, &info, false).await
}

/// `initialize` of a legacy session with a modern-only server: answered from `server/discover`.
pub(crate) async fn modern_initialize(
    env: &Env,
    server: &Arc<ServerRt>,
    headers: http::HeaderMap,
    version: &str,
    caps: &Value,
    client_info: &Value,
) -> Result<(Link, Value), UpError> {
    let link = Link {
        server: Arc::clone(server),
        era: Era::Modern,
        pv: version.to_owned(),
        sid: None,
        sse: None,
        headers,
        meta: Some(session_meta(caps, client_info)),
    };
    let msg = link.ask(env, "server/discover", None).await?;
    let found = msg.get("result").ok_or(UpError::Rejected)?;
    let mut result = Map::new();
    result.insert("protocolVersion".into(), Value::from(version));
    result.insert(
        "capabilities".into(),
        found.get("capabilities").cloned().unwrap_or_else(|| json!({})),
    );
    if let Some(info) = found.get("serverInfo") {
        result.insert("serverInfo".into(), info.clone());
    }
    if let Some(text) = found.get("instructions") {
        result.insert("instructions".into(), text.clone());
    }
    Ok((link, Value::Object(result)))
}

/// What a bridged call came to.
#[derive(Debug)]
pub(crate) enum Bridged {
    /// The server's final answer.
    Final(Value),
    /// The server asked the client for input the retry has not given yet.
    Input { key: String, request: Value },
}

/// Sends a call through a bridged session, answering the server's requests from `responses` (keys
/// `r0`, `r1`, … in the order the server asks).
pub(crate) async fn call(
    env: &Env,
    link: &Link,
    id: &Value,
    method: &str,
    params: Value,
    responses: &Map<String, Value>,
    timeout: Duration,
) -> Result<Bridged, UpError> {
    let answer = link.send(env, id, method, Some(params), timeout).await?;
    let mut src = match answer {
        Answer::Json(v) => return Ok(Bridged::Final(v)),
        Answer::Empty => return Err(UpError::NotMcp),
        Answer::Stream(src) => src,
    };
    let mut asked = 0usize;
    loop {
        let msg = next_message(&mut src, link.server.stream_idle_timeout.min(timeout)).await?;
        if msg.get("id") == Some(id) && (msg.get("result").is_some() || msg.get("error").is_some()) {
            return Ok(Bridged::Final(msg));
        }
        let Some(asked_method) = msg.get("method").and_then(Value::as_str) else {
            continue;
        };
        let Some(server_id) = msg.get("id").cloned() else {
            continue;
        };
        if !INPUT_METHODS.contains(&asked_method) {
            continue;
        }
        let key = format!("r{asked}");
        asked += 1;
        match responses.get(&key) {
            Some(answer) => {
                let reply = json!({"jsonrpc": "2.0", "id": server_id, "result": answer});
                link.post(env, &reply).await?;
            }
            None => {
                let mut request = json!({"method": asked_method});
                if let (Some(p), Some(obj)) = (msg.get("params"), request.as_object_mut()) {
                    obj.insert("params".into(), p.clone());
                }
                return Ok(Bridged::Input { key, request });
            }
        }
    }
}

/// The marker of a bridged call's input round, sealed so only this gateway reads it back.
fn state_claims(req: &Req, server: &str, item: &str, round: usize) -> CursorClaims {
    CursorClaims {
        sub: req.caller.principal.clone(),
        m: format!("bridge:{server}:{item}"),
        o: round,
    }
}

/// The input-required result asking the client for `request`.
pub(crate) fn input_required(
    req: &Req,
    server: &str,
    item: &str,
    key: &str,
    request: Value,
    round: usize,
) -> Value {
    let mut inputs = Map::new();
    inputs.insert(key.to_owned(), request);
    let mut result = json!({"resultType": "input_required", "inputRequests": inputs});
    if let (Some(state), Some(obj)) = (
        req.keys
            .seal_cursor(&req.gw.name, &state_claims(req, server, item, round)),
        result.as_object_mut(),
    ) {
        obj.insert("requestState".into(), Value::from(state));
    }
    result
}

/// The `inputResponses` of a retry, checked against its `requestState`; the params to send upstream
/// lose both members.
pub(crate) fn take_inputs(
    req: &Req,
    server: &str,
    item: &str,
    params: &mut Value,
) -> Result<Map<String, Value>, Fail> {
    let Some(obj) = params.as_object_mut() else {
        return Ok(Map::new());
    };
    let responses = match obj.remove("inputResponses") {
        Some(Value::Object(m)) => m,
        Some(_) => return Err(Fail::invalid_params("inputResponses must be an object.")),
        None => Map::new(),
    };
    if let Some(state) = obj.remove("requestState") {
        let valid = state
            .as_str()
            .and_then(|s| req.keys.open_cursor(&req.gw.name, s))
            .is_some_and(|c| c.sub == req.caller.principal && c.m == format!("bridge:{server}:{item}"));
        if !valid {
            return Err(Fail::gateway(
                reasons::INVALID_PARAMS,
                Stage::Parse,
                Message::Fixed("requestState is not one this gateway issued for this call."),
            ));
        }
    }
    Ok(responses)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_meta_carries_the_modern_fields() {
        let m = session_meta(&json!({"elicitation": {}}), &json!({"name": "c"}));
        assert_eq!(m[META_PV], MODERN);
        assert_eq!(m[META_CAPS], json!({"elicitation": {}}));
        assert_eq!(m[META_INFO]["name"], "c");
    }
}
