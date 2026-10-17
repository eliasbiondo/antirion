//! The A2A proxy: agents behind `/a2a/<agent>`, their cards rewritten to point at the gateway, known
//! methods forwarded unchanged (streaming ones relayed event by event), push-notification URLs checked
//! against the egress rules, and hidden agents indistinguishable from missing ones.

use std::sync::Arc;
use std::time::Duration;

use antirion_core::Stage;
use antirion_error::reason::hygiene;
use antirion_error::{Code, GatewayError, Message};
use bytes::Bytes;
use http::header::{ACCEPT, CONTENT_TYPE};
use http::{HeaderMap, HeaderValue, Method, Response, Uri};
use http_body_util::BodyExt;
use serde_json::{Value, json};

use crate::caller::Caller;
use crate::compile::AgentRt;
use crate::egress::Egress;
use crate::http::Env;
use crate::reasons;
use crate::rpc::{Fail, Msg, classify, credentials, decorate_upstream_error};
use crate::sse::{ReadError, Reader};
use crate::{Body, respond};

const METHODS: [&str; 21] = [
    "message/send",
    "message/stream",
    "tasks/get",
    "tasks/cancel",
    "tasks/resubscribe",
    "tasks/pushNotificationConfig/set",
    "tasks/pushNotificationConfig/get",
    "tasks/pushNotificationConfig/list",
    "tasks/pushNotificationConfig/delete",
    "agent/getAuthenticatedExtendedCard",
    "SendMessage",
    "SendStreamingMessage",
    "GetTask",
    "ListTasks",
    "CancelTask",
    "SubscribeToTask",
    "CreateTaskPushNotificationConfig",
    "GetTaskPushNotificationConfig",
    "ListTaskPushNotificationConfigs",
    "DeleteTaskPushNotificationConfig",
    "GetExtendedAgentCard",
];

const STREAMING: [&str; 4] = [
    "message/stream",
    "SendStreamingMessage",
    "tasks/resubscribe",
    "SubscribeToTask",
];

const CARD_TIMEOUT: Duration = Duration::from_secs(10);

fn not_found(name: &str) -> Fail {
    Fail::gateway(
        reasons::AGENT_NOT_FOUND,
        Stage::Resolve,
        Message::built(format!("agent not found: {name}")),
    )
}

/// The agent, when it exists, is enabled and the caller may use it (or, for a public card, when no
/// credential is needed); otherwise the one not-found answer.
fn agent(env: &Env, name: &str, caller: Option<&Caller>) -> Result<Arc<AgentRt>, Fail> {
    let found = env.snap.agents.get(name).filter(|a| a.enabled).cloned();
    match (found, caller) {
        (Some(a), Some(c)) if c.agent_granted(&env.snap, name) => Ok(a),
        (Some(a), None) if a.public_card => Ok(a),
        _ => Err(not_found(name)),
    }
}

fn unavailable(message: &'static str, agent: &str) -> Fail {
    Fail::gateway(
        hygiene::CONNECTION_FAILED,
        Stage::Attempt,
        Message::Fixed(message),
    )
    .provider(agent)
}

fn unusable(agent: &str, field: &str) -> Fail {
    Fail::gateway(
        reasons::AGENT_CARD_UNUSABLE,
        Stage::Attempt,
        Message::built(format!("The agent card has no JSON-RPC endpoint ({field}).")),
    )
    .provider(agent)
}

fn elsewhere(agent: &str, field: &str) -> Fail {
    Fail::gateway(
        reasons::AGENT_CARD_UNUSABLE,
        Stage::Attempt,
        Message::built(format!(
            "The agent card's JSON-RPC endpoint ({field}) is not on the card's origin; set the agent's url to use it."
        )),
    )
    .provider(agent)
}

/// Whether two URLs share scheme, host and port (the scheme's default when absent).
fn same_origin(a: &Uri, b: &Uri) -> bool {
    let port = |u: &Uri| {
        u.port_u16().or_else(|| match u.scheme_str() {
            Some("https") => Some(443),
            Some("http") => Some(80),
            _ => None,
        })
    };
    a.scheme_str().map(str::to_ascii_lowercase) == b.scheme_str().map(str::to_ascii_lowercase)
        && a.host().map(str::to_ascii_lowercase) == b.host().map(str::to_ascii_lowercase)
        && port(a) == port(b)
}

fn timeout(agent: &str) -> Fail {
    Fail::from(GatewayError::new(
        Code::UpstreamTimeout,
        Stage::Attempt,
        Message::Fixed("The agent did not answer in time."),
    ))
    .provider(agent)
}

/// The upstream card, from the cache when fresh.
async fn fetch_card(env: &Env, agent: &AgentRt) -> Result<Value, Fail> {
    if let Some((at, card)) = env.state.cards.lock().get(&agent.generation).cloned()
        && env.now >= at
        && env.now.since(at) < agent.card_cache_ttl
    {
        return Ok(card);
    }
    let mut headers = agent.headers.clone();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    let resp = env
        .egress
        .send(
            Method::GET,
            agent.card_url.clone(),
            headers,
            Bytes::new(),
            CARD_TIMEOUT,
        )
        .await
        .map_err(|_| unavailable("The agent card could not be fetched.", &agent.name))?;
    if !resp.status.is_success() {
        return Err(unavailable("The agent card could not be fetched.", &agent.name)
            .upstream_status(resp.status.as_u16()));
    }
    let bytes = tokio::time::timeout(CARD_TIMEOUT, resp.body.collect())
        .await
        .ok()
        .and_then(Result::ok)
        .map(|c| c.to_bytes())
        .ok_or_else(|| unavailable("The agent card could not be read.", &agent.name))?;
    let card: Value = serde_json::from_slice(&bytes)
        .ok()
        .filter(Value::is_object)
        .ok_or_else(|| unavailable("The agent card is not JSON.", &agent.name))?;
    if !agent.card_cache_ttl.is_zero() {
        env.state
            .cards
            .lock()
            .insert(agent.generation, (env.now, card.clone()));
    }
    Ok(card)
}

fn is_v1(agent: &AgentRt, card: &Value) -> bool {
    match agent.protocol.as_deref() {
        Some(p) => p.starts_with('1'),
        None => card.get("supportedInterfaces").is_some(),
    }
}

/// The card's JSON-RPC endpoint, or the field that lacks one.
fn card_endpoint(agent: &AgentRt, card: &Value) -> Result<String, &'static str> {
    if is_v1(agent, card) {
        return card
            .get("supportedInterfaces")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|i| i.get("protocolBinding").and_then(Value::as_str) == Some("JSONRPC"))
            .find_map(|i| i.get("url").and_then(Value::as_str).map(str::to_owned))
            .ok_or("supportedInterfaces");
    }
    let additional = card
        .get("additionalInterfaces")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|i| i.get("transport").and_then(Value::as_str) == Some("JSONRPC"))
        .find_map(|i| i.get("url").and_then(Value::as_str).map(str::to_owned));
    additional
        .or_else(|| card.get("url").and_then(Value::as_str).map(str::to_owned))
        .ok_or("url")
}

/// The card as clients see it: every proxied endpoint at the gateway, other transports removed, the
/// gateway's security schemes, and no signatures.
fn rewrite(agent: &AgentRt, card: &Value, base: &str) -> Value {
    let url = format!("{base}/a2a/{}", agent.name);
    let mut out = card.clone();
    let Some(obj) = out.as_object_mut() else {
        return out;
    };
    obj.remove("signatures");
    if is_v1(agent, card) {
        if let Some(list) = obj.get_mut("supportedInterfaces").and_then(Value::as_array_mut) {
            list.retain(|i| {
                i.get("url").is_none() || i.get("protocolBinding").and_then(Value::as_str) == Some("JSONRPC")
            });
            for i in list.iter_mut() {
                if i.get("url").is_some()
                    && let Some(o) = i.as_object_mut()
                {
                    o.insert("url".into(), Value::from(url.clone()));
                }
            }
        }
        obj.insert(
            "securitySchemes".into(),
            json!({"antirion": {"httpAuthSecurityScheme": {"scheme": "Bearer"}}}),
        );
        obj.insert(
            "securityRequirements".into(),
            json!([{"schemes": {"antirion": {"list": []}}}]),
        );
    } else {
        obj.insert("url".into(), Value::from(url.clone()));
        if let Some(list) = obj.get_mut("additionalInterfaces").and_then(Value::as_array_mut) {
            list.retain(|i| {
                i.get("url").is_none() || i.get("transport").and_then(Value::as_str) == Some("JSONRPC")
            });
            for i in list.iter_mut() {
                if i.get("url").is_some()
                    && let Some(o) = i.as_object_mut()
                {
                    o.insert("url".into(), Value::from(url.clone()));
                }
            }
        }
        obj.insert("preferredTransport".into(), Value::from("JSONRPC"));
        obj.insert(
            "securitySchemes".into(),
            json!({"antirion": {"type": "http", "scheme": "bearer"}}),
        );
        obj.insert("security".into(), json!([{"antirion": []}]));
    }
    out
}

/// `GET /a2a/<agent>/.well-known/agent-card.json`.
pub(crate) async fn card(env: Env, name: &str, caller: Option<Caller>) -> Response<Body> {
    let result = async {
        let a = agent(&env, name, caller.as_ref())?;
        let card = fetch_card(&env, &a).await?;
        if a.url.is_none()
            && let Err(field) = card_endpoint(&a, &card)
        {
            return Err(unusable(&a.name, field));
        }
        Ok(rewrite(&a, &card, &env.public_base))
    }
    .await;
    match result {
        Ok(card) => respond::json(200, &card, "upstream"),
        Err(f) => respond::error(&f, &env),
    }
}

/// The push-notification URLs a request carries, with the path of each.
fn push_urls(method: &str, params: &Value) -> Vec<(&'static str, String)> {
    let candidates: &[(&'static str, &[&str])] = match method {
        "tasks/pushNotificationConfig/set" => &[
            (
                "params.pushNotificationConfig.url",
                &["pushNotificationConfig", "url"],
            ),
            ("params.url", &["url"]),
        ],
        "CreateTaskPushNotificationConfig" => &[
            (
                "params.config.pushNotificationConfig.url",
                &["config", "pushNotificationConfig", "url"],
            ),
            ("params.config.url", &["config", "url"]),
            (
                "params.pushNotificationConfig.url",
                &["pushNotificationConfig", "url"],
            ),
        ],
        "message/send" | "SendMessage" | "message/stream" | "SendStreamingMessage" => &[(
            "params.configuration.pushNotificationConfig.url",
            &["configuration", "pushNotificationConfig", "url"],
        )],
        _ => &[],
    };
    candidates
        .iter()
        .filter_map(|(path, keys)| {
            let mut v = params;
            for k in *keys {
                v = v.get(k)?;
            }
            Some((*path, v.as_str()?.to_owned()))
        })
        .collect()
}

async fn check_push(egress: &Egress, method: &str, params: Option<&Value>) -> Result<(), Fail> {
    let Some(params) = params else {
        return Ok(());
    };
    for (path, url) in push_urls(method, params) {
        if let Some(why) = egress.refuse_client_url(&url).await {
            return Err(Fail::protocol(
                -32602,
                200,
                reasons::URL_NOT_ALLOWED,
                Stage::Parse,
                Message::built(format!("{path}: {why}")),
            ));
        }
    }
    Ok(())
}

fn upstream_headers(agent: &AgentRt, client: &HeaderMap, caller: &Caller, streaming: bool) -> HeaderMap {
    let mut h = agent.headers.clone();
    if let Some(v) = client.get("a2a-version") {
        h.insert("a2a-version", v.clone());
    }
    caller.apply_identity(&agent.identity_headers, &mut h);
    h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    h.insert(
        ACCEPT,
        HeaderValue::from_static(if streaming {
            "text/event-stream, application/json"
        } else {
            "application/json, text/event-stream"
        }),
    );
    h
}

/// `POST /a2a/<agent>`.
pub(crate) async fn rpc(
    env: Env,
    name: &str,
    headers: HeaderMap,
    body: Bytes,
    caller: Caller,
) -> Response<Body> {
    let mut id = Value::Null;
    match forward(&env, name, &headers, body, &caller, &mut id).await {
        Ok(r) => r,
        Err(f) => respond::error(&f.id(&id), &env),
    }
}

async fn forward(
    env: &Env,
    name: &str,
    headers: &HeaderMap,
    body: Bytes,
    caller: &Caller,
    id: &mut Value,
) -> Result<Response<Body>, Fail> {
    let value: Value = serde_json::from_slice(&body).map_err(|_| {
        Fail::protocol(
            -32700,
            400,
            hygiene::INVALID_JSON,
            Stage::Parse,
            Message::Fixed("Parse error: the body is not valid JSON."),
        )
    })?;
    let msg = classify(&value).map_err(|rid| {
        *id = rid;
        Fail::gateway(
            reasons::INVALID_JSONRPC,
            Stage::Parse,
            Message::Fixed("The body is not a JSON-RPC 2.0 request."),
        )
    })?;
    let Msg::Request {
        id: rid,
        method,
        params,
    } = msg
    else {
        return Err(Fail::gateway(
            reasons::INVALID_JSONRPC,
            Stage::Parse,
            Message::Fixed("The body is not a JSON-RPC 2.0 request."),
        ));
    };
    *id = rid;
    env.log.update(|f| {
        f.method = Some(method.clone());
        f.task_id = params
            .as_ref()
            .and_then(|p| p.get("id").or_else(|| p.get("taskId")))
            .and_then(Value::as_str)
            .map(str::to_owned);
    });
    let a = agent(env, name, Some(caller))?;
    if !METHODS.contains(&method.as_str()) && !a.extra_methods.contains(&method) {
        return Err(Fail::unknown_method(&method, 200));
    }
    check_push(&env.egress, &method, params.as_ref()).await?;
    let endpoint: Uri = match &a.url {
        Some(u) => u.clone(),
        None => {
            let card = fetch_card(env, &a).await?;
            let field = if is_v1(&a, &card) {
                "supportedInterfaces"
            } else {
                "url"
            };
            let url = card_endpoint(&a, &card).map_err(|field| unusable(&a.name, field))?;
            let url: Uri = url.parse().map_err(|_| unusable(&a.name, field))?;
            // The card is the agent's to write, the agent's headers the operator's: an endpoint on
            // another origin than the card's would take them, and the caller's body, elsewhere.
            if !same_origin(&url, &a.card_url) {
                return Err(elsewhere(&a.name, field));
            }
            url
        }
    };
    let streaming = STREAMING.contains(&method.as_str());
    let wait = if streaming {
        a.stream_idle_timeout
    } else {
        a.request_timeout
    };
    let out_headers = upstream_headers(&a, headers, caller, streaming);
    let charge = env
        .hooks
        .admit(crate::hooks::Counted::Agent {
            agent: &a.name,
            method: &method,
        })
        .await
        .map_err(Fail::from)?;
    let resp = match env
        .egress
        .send(Method::POST, endpoint, out_headers, body, wait)
        .await
    {
        Ok(r) => r,
        Err(antirion_egress::TransportError::Timeout) => return Err(timeout(&a.name)),
        Err(_) => return Err(unavailable("The agent could not be reached.", &a.name)),
    };
    let ctype = resp
        .headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if ctype.starts_with("text/event-stream") && resp.status.is_success() {
        if let Some(c) = charge {
            c.answered(true).await;
        }
        return relay(env, &a, Reader::new(resp.body), id.clone()).await;
    }
    let bytes = match tokio::time::timeout(wait, resp.body.collect()).await {
        Err(_) => return Err(timeout(&a.name)),
        Ok(Err(_)) => return Err(unavailable("The agent connection broke.", &a.name)),
        Ok(Ok(c)) => c.to_bytes(),
    };
    let parsed = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .filter(|v| v.get("jsonrpc").is_some());
    let Some(mut answer) = parsed else {
        let status = resp.status.as_u16();
        return Err(Fail::gateway(
            reasons::UPSTREAM_NOT_MCP,
            Stage::Attempt,
            Message::Fixed("The agent did not answer with JSON-RPC."),
        )
        .provider(&a.name)
        .upstream_status(status));
    };
    env.log.upstream_answer(&answer);
    if let Some(c) = charge {
        c.answered(answer.get("error").is_none()).await;
    }
    if answer.get("error").is_some() {
        decorate_upstream_error(&mut answer, &a.name, &env.request_id, &credentials(&a.headers));
    }
    Ok(respond::json(200, &answer, "upstream"))
}

fn sse_data(msg: &Value) -> Bytes {
    Bytes::from(format!(
        "data: {}\n\n",
        serde_json::to_string(msg).unwrap_or_default()
    ))
}

/// Relays a streaming answer event by event; silence before the first event is a 504, after it a
/// final error event.
async fn relay(env: &Env, agent: &AgentRt, mut reader: Reader, id: Value) -> Result<Response<Body>, Fail> {
    let idle = agent.stream_idle_timeout;
    let first = match reader.next(idle).await {
        Ok(ev) => ev,
        Err(ReadError::Idle) => return Err(timeout(&agent.name)),
        Err(ReadError::Ended) => return Err(unavailable("The agent stream ended early.", &agent.name)),
    };
    let request_id = env.request_id.clone();
    let name = agent.name.clone();
    let secrets = credentials(&agent.headers);
    let shape = move |data: &str| -> Option<Bytes> {
        let mut msg: Value = serde_json::from_str(data).ok()?;
        if msg.get("error").is_some() {
            decorate_upstream_error(&mut msg, &name, &request_id, &secrets);
        }
        Some(sse_data(&msg))
    };
    let first_bytes = shape(&first.data);
    let timeout_event = {
        let fail = timeout(&agent.name).id(&id);
        sse_data(&fail.render(&env.request_id).2)
    };
    let rest = futures_util::stream::unfold(
        (reader, false, shape.clone(), timeout_event),
        move |(mut reader, done, shape, timeout_event)| async move {
            if done {
                return None;
            }
            loop {
                match reader.next(idle).await {
                    Ok(ev) => {
                        if let Some(b) = shape(&ev.data) {
                            return Some((b, (reader, false, shape, timeout_event)));
                        }
                    }
                    Err(ReadError::Idle) => {
                        let b = timeout_event.clone();
                        return Some((b, (reader, true, shape, timeout_event)));
                    }
                    Err(ReadError::Ended) => return None,
                }
            }
        },
    );
    let head = futures_util::stream::iter(first_bytes);
    Ok(respond::sse(futures_util::StreamExt::chain(head, rest)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_endpoint_must_share_the_cards_origin() {
        let card: Uri = "https://agent.example/.well-known/agent-card.json"
            .parse()
            .unwrap_or_default();
        let same: Uri = "https://AGENT.example:443/rpc".parse().unwrap_or_default();
        assert!(same_origin(&same, &card));
        for other in [
            "http://agent.example/rpc",
            "https://agent.example:8443/rpc",
            "https://169.254.169.254/rpc",
        ] {
            let other: Uri = other.parse().unwrap_or_default();
            assert!(!same_origin(&other, &card), "{other}");
        }
    }
    use crate::compile::compile;

    fn agent_of(protocol: Option<&str>) -> Arc<AgentRt> {
        let mut spec = json!({"card_url": "http://h/card"});
        if let Some(p) = protocol {
            spec["protocol"] = Value::from(p);
        }
        compile(&json!({"a2a": {"agents": {"w": spec}}})).unwrap().agents["w"].clone()
    }

    #[test]
    fn v03_rewrite() {
        let card = json!({"protocolVersion": "0.3.0", "url": "http://internal/rpc", "preferredTransport": "GRPC",
            "additionalInterfaces": [{"url": "http://internal/rpc", "transport": "JSONRPC"},
                                     {"url": "grpc://internal", "transport": "GRPC"}],
            "securitySchemes": {"k": {}}, "security": [{"k": []}], "signatures": [{}]});
        let got = rewrite(&agent_of(None), &card, "http://gw");
        assert_eq!(got["url"], "http://gw/a2a/w");
        assert_eq!(
            got["additionalInterfaces"],
            json!([{"url": "http://gw/a2a/w", "transport": "JSONRPC"}])
        );
        assert_eq!(got["preferredTransport"], "JSONRPC");
        assert!(got.get("signatures").is_none());
        assert_eq!(
            got["securitySchemes"],
            json!({"antirion": {"type": "http", "scheme": "bearer"}})
        );
    }

    #[test]
    fn v10_rewrite_and_endpoint() {
        let card = json!({"supportedInterfaces": [
            {"url": "http://internal/rpc", "protocolBinding": "JSONRPC"},
            {"url": "grpc://internal", "protocolBinding": "GRPC"},
            {"protocolBinding": "JSONRPC", "tenant": "t"}]});
        let a = agent_of(None);
        assert_eq!(card_endpoint(&a, &card).unwrap(), "http://internal/rpc");
        let got = rewrite(&a, &card, "http://gw");
        assert_eq!(
            got["supportedInterfaces"],
            json!([
            {"url": "http://gw/a2a/w", "protocolBinding": "JSONRPC"},
            {"protocolBinding": "JSONRPC", "tenant": "t"}])
        );
        let bare = json!({"supportedInterfaces": [{"protocolBinding": "JSONRPC"}]});
        assert_eq!(card_endpoint(&a, &bare), Err("supportedInterfaces"));
        assert_eq!(card_endpoint(&a, &json!({"name": "x"})), Err("url"));
    }

    #[tokio::test]
    async fn push_urls_are_found_and_checked() {
        let p = json!({"configuration": {"pushNotificationConfig": {"url": "http://169.254.169.254/"}}});
        let egress = Egress {
            client: antirion_egress::EgressClient::new(Duration::from_secs(1)),
            policy: std::sync::Arc::default(),
            tls: antirion_egress::TlsProfile::default_profile().unwrap(),
            request_id: antirion_core::RequestId::generate(),
            subject: None,
        };
        let err = check_push(&egress, "message/send", Some(&p)).await.unwrap_err();
        assert!(err.err.message.as_str().contains("pushNotificationConfig"));
        let ok = json!({"taskId": "t", "pushNotificationConfig": {"url": "https://93.184.215.14/hook"}});
        assert!(
            check_push(&egress, "tasks/pushNotificationConfig/set", Some(&ok))
                .await
                .is_ok()
        );
    }
}
