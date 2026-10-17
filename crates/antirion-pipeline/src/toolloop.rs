//! The server-side tool loop (`policies.mcp.mcp_tools`): the caller's granted MCP tools are offered
//! to the model in the client's dialect; when every tool call of an answer names a tool
//! `auto_execute` matches, the gateway runs them, appends their results and calls the model again,
//! at most `max_iterations` model calls in all. Any other answer goes to the client as it is. Model
//! calls are made without streaming; a client that asked for a stream gets the final answer as one,
//! in its own dialect.

use std::sync::Arc;

use antirion_core::Stage;
use antirion_dialects::Endpoint;
use antirion_error::{Code, GatewayError, Message};
use antirion_mcp::{Env, Hooks, OfferedTool, Toolbox};
use bytes::{Bytes, BytesMut};
use http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use http::request::Parts;
use http::{HeaderName, HeaderValue, Response};
use http_body_util::BodyExt;
use serde_json::{Value, json};

use crate::attempt::Done;
use crate::auth::Authenticated;
use crate::ctx::{Body, Ctx, full};
use crate::policy::Levels;

/// The most model calls one request may make, whatever the configuration says.
const MAX_ITERATIONS: u64 = 50;
const DEFAULT_ITERATIONS: u64 = 10;
/// The response header saying the loop stopped at its bound.
const LOOP_HEADER: &str = "x-antirion-mcp-loop";

/// The client dialects the loop speaks.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Flavor {
    Chat,
    Messages,
    Responses,
}

/// One tool call of a model answer.
#[derive(Clone, Debug)]
pub(crate) struct ToolCall {
    id: String,
    function: String,
    arguments: Value,
}

/// What to do with a model answer.
#[derive(Debug)]
pub(crate) enum Decision {
    /// Return it to the client.
    Final,
    /// Run these calls and ask the model again.
    Execute(Vec<ToolCall>),
}

/// The loop of one request.
#[derive(Debug)]
pub(crate) struct ToolLoop {
    flavor: Flavor,
    toolbox: Toolbox,
    auto_execute: Vec<String>,
    pub(crate) max_iterations: u64,
}

/// A request whose body the loop cannot rewrite.
pub(crate) fn unusable() -> GatewayError {
    GatewayError::new(
        Code::InvalidRequest,
        Stage::Parse,
        Message::Fixed("The request body cannot take the MCP tools."),
    )
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

/// The MCP environment of a model request's tool calls: the node's MCP state, the egress path, and
/// the caller's limits and MCP guardrails.
fn env(cx: &Ctx, auth: &Authenticated) -> Option<Env> {
    let egress = cx.rt.mcp_egress(&cx.snap, &cx.request_id)?;
    let caller = crate::mcp::caller(cx, auth);
    let guardrails = crate::mcpguard::McpGuard::new(cx, &auth.key, auth.end_user.as_deref())
        .map(|g| Arc::new(g) as Arc<dyn antirion_mcp::CallHook>);
    let hooks = Hooks {
        limits: Some(Arc::new(crate::mcplimits::McpLimits {
            rt: Arc::clone(&cx.rt),
            snap: Arc::clone(&cx.snap),
            key: Arc::clone(&auth.key),
            end_user: auth.end_user.clone(),
            tags: caller.tags,
            ip: cx.client_ip,
            log_id: cx.log_id.clone(),
            family: "mcp",
        })),
        guardrails,
    };
    Some(Env {
        public_base: String::new(),
        snap: Arc::clone(&cx.snap.mcp),
        state: Arc::clone(&cx.rt.state.mcp),
        egress,
        now: cx.rt.now(),
        request_id: cx.request_id.clone(),
        tasks: cx.rt.tasks.clone(),
        shutdown: cx.rt.shutdown.clone(),
        hooks,
        log: Arc::new(antirion_mcp::CallLog::default()),
    })
}

/// The loop of a request, when its levels configure `mcp_tools` for an endpoint that can use it and
/// the caller is granted at least one of the tools.
pub(crate) async fn open(
    cx: &Ctx,
    _parts: &Parts,
    auth: &Authenticated,
    endpoint: &Endpoint,
    policies: &Levels<'_>,
) -> Option<ToolLoop> {
    let flavor = match (endpoint.id, endpoint.action) {
        ("chat_completions", None) => Flavor::Chat,
        ("messages", None) => Flavor::Messages,
        ("responses", None) => Flavor::Responses,
        _ => return None,
    };
    let effective = policies.effective("mcp");
    let cfg = effective.get("mcp_tools")?;
    let servers = strings(cfg.get("servers"));
    if servers.is_empty() {
        return None;
    }
    let max_iterations = cfg
        .get("max_iterations")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_ITERATIONS)
        .clamp(1, MAX_ITERATIONS);
    let mut caller = crate::mcp::caller(cx, auth);
    caller.jwt.clone_from(&auth.claims);
    let toolbox = Toolbox::open(env(cx, auth)?, caller, &servers).await;
    crate::mcp::report_definition_changes(cx);
    Some(ToolLoop {
        flavor,
        toolbox: toolbox?,
        auto_execute: strings(cfg.get("auto_execute")),
        max_iterations,
    })
}

fn function_tool(flavor: Flavor, t: &OfferedTool) -> Value {
    let description = t.description.clone().unwrap_or_default();
    match flavor {
        Flavor::Chat => json!({"type": "function", "function": {
            "name": t.function, "description": description, "parameters": t.input_schema}}),
        Flavor::Messages => {
            json!({"name": t.function, "description": description, "input_schema": t.input_schema})
        }
        Flavor::Responses => json!({"type": "function", "name": t.function, "description": description,
                                    "parameters": t.input_schema}),
    }
}

fn parsed_arguments(v: Option<&Value>) -> Value {
    match v {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or_else(|_| json!({})),
        Some(other) => other.clone(),
        None => json!({}),
    }
}

impl ToolLoop {
    /// Adds the offered tools to the request's tools and asks for an answer without streaming.
    pub(crate) fn offer(&self, body: &mut Value) {
        let Some(obj) = body.as_object_mut() else {
            return;
        };
        let tools = obj.entry("tools").or_insert_with(|| json!([]));
        if let Some(list) = tools.as_array_mut() {
            list.extend(self.toolbox.tools().iter().map(|t| function_tool(self.flavor, t)));
        }
        obj.remove("stream_options");
        obj.insert("stream".into(), Value::Bool(false));
    }

    /// The tool calls of a model answer.
    fn calls(&self, answer: &Value) -> Vec<ToolCall> {
        match self.flavor {
            Flavor::Chat => answer
                .pointer("/choices/0/message/tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|c| ToolCall {
                    id: c.get("id").and_then(Value::as_str).unwrap_or_default().to_owned(),
                    function: c
                        .pointer("/function/name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    arguments: parsed_arguments(c.pointer("/function/arguments")),
                })
                .collect(),
            Flavor::Messages => answer
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
                .map(|b| ToolCall {
                    id: b.get("id").and_then(Value::as_str).unwrap_or_default().to_owned(),
                    function: b
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    arguments: b.get("input").cloned().unwrap_or_else(|| json!({})),
                })
                .collect(),
            Flavor::Responses => answer
                .get("output")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|i| i.get("type").and_then(Value::as_str) == Some("function_call"))
                .map(|i| ToolCall {
                    id: i
                        .get("call_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    function: i
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    arguments: parsed_arguments(i.get("arguments")),
                })
                .collect(),
        }
    }

    /// Whether the gateway runs an answer's tool calls: only when every one is an offered tool that
    /// `auto_execute` names.
    pub(crate) fn decide(&self, answer: &Value) -> Decision {
        let calls = self.calls(answer);
        let automatic = |c: &ToolCall| {
            self.toolbox.by_function(&c.function).is_some_and(|t| {
                self.auto_execute
                    .iter()
                    .any(|p| antirion_identity::access::pattern_matches(p, &t.name))
            })
        };
        if calls.is_empty() || !calls.iter().all(automatic) {
            return Decision::Final;
        }
        Decision::Execute(calls)
    }

    /// Runs the calls in parallel; each answer's text, and whether the tool failed.
    pub(crate) async fn execute(&self, calls: &[ToolCall]) -> Vec<(String, bool)> {
        let runs = calls.iter().enumerate().map(|(n, c)| async move {
            match self.toolbox.by_function(&c.function) {
                Some(t) => self.toolbox.call(t, c.arguments.clone(), n as u64).await,
                None => ("Unknown tool.".to_owned(), true),
            }
        });
        futures_util::future::join_all(runs).await
    }

    /// Appends the model's answer and the tool results to the conversation.
    pub(crate) fn append(
        &self,
        body: &mut Value,
        answer: &Value,
        calls: &[ToolCall],
        results: &[(String, bool)],
    ) {
        let Some(obj) = body.as_object_mut() else {
            return;
        };
        match self.flavor {
            Flavor::Chat => {
                let Some(messages) = obj.get_mut("messages").and_then(Value::as_array_mut) else {
                    return;
                };
                if let Some(m) = answer.pointer("/choices/0/message") {
                    messages.push(m.clone());
                }
                for (c, (text, _)) in calls.iter().zip(results) {
                    messages.push(json!({"role": "tool", "tool_call_id": c.id, "content": text}));
                }
            }
            Flavor::Messages => {
                let Some(messages) = obj.get_mut("messages").and_then(Value::as_array_mut) else {
                    return;
                };
                messages.push(json!({"role": "assistant",
                                     "content": answer.get("content").cloned().unwrap_or_else(|| json!([]))}));
                let blocks: Vec<Value> = calls
                    .iter()
                    .zip(results)
                    .map(|(c, (text, failed))| {
                        json!({"type": "tool_result", "tool_use_id": c.id, "content": text, "is_error": failed})
                    })
                    .collect();
                messages.push(json!({"role": "user", "content": blocks}));
            }
            Flavor::Responses => {
                let input = obj.entry("input").or_insert_with(|| json!([]));
                if let Value::String(text) = input {
                    *input = json!([{"role": "user", "content": text.clone()}]);
                }
                let Some(items) = input.as_array_mut() else {
                    return;
                };
                for item in answer
                    .get("output")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if item.get("type").and_then(Value::as_str) == Some("function_call") {
                        items.push(item.clone());
                    }
                }
                for (c, (text, _)) in calls.iter().zip(results) {
                    items.push(json!({"type": "function_call_output", "call_id": c.id, "output": text}));
                }
            }
        }
    }

    /// The answer the client gets: the model's, as a stream in the client's dialect when it asked for
    /// one, marked when the loop stopped at its bound.
    pub(crate) fn finish(
        &self,
        mut head: http::response::Parts,
        answer: &Value,
        raw: Bytes,
        stream: bool,
        wants_usage: bool,
        capped: bool,
    ) -> Response<Body> {
        if capped {
            head.headers.insert(
                HeaderName::from_static(LOOP_HEADER),
                HeaderValue::from_static("max_iterations"),
            );
        }
        let events = stream.then(|| {
            let mut facts = crate::relay::StreamFacts::default();
            match self.flavor {
                Flavor::Chat => {
                    crate::relay::synthesize(answer, None, wants_usage, &mut facts).map(|mut e| {
                        e.push(Bytes::from_static(b"data: [DONE]\n\n"));
                        e
                    })
                }
                Flavor::Messages => crate::relay::synth::anthropic(answer, &mut facts),
                Flavor::Responses => crate::relay::synth::responses(answer, &mut facts),
            }
        });
        let body = match events.flatten() {
            Some(events) => {
                let mut b = BytesMut::new();
                for e in events {
                    b.extend_from_slice(&e);
                }
                head.headers
                    .insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
                b.freeze()
            }
            None => raw,
        };
        head.headers.remove(CONTENT_LENGTH);
        Response::from_parts(head, full(body))
    }
}

/// A model answer read whole: its head, bytes and JSON; `Err` gives back what is not a JSON answer.
pub(crate) async fn read(done: Done) -> Result<(http::response::Parts, Bytes, Value), Box<Done>> {
    let Done::Response(resp) = done else {
        return Err(Box::new(done));
    };
    let ok = resp.status().is_success()
        && resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|c| c.starts_with("application/json"));
    if !ok {
        return Err(Box::new(Done::Response(resp)));
    }
    let (head, body) = resp.into_parts();
    let Ok(collected) = body.collect().await else {
        return Err(Box::new(Done::Response(Response::from_parts(
            head,
            crate::ctx::empty(),
        ))));
    };
    let bytes = collected.to_bytes();
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(v) => Ok((head, bytes, v)),
        Err(_) => Err(Box::new(Done::Response(Response::from_parts(head, full(bytes))))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_read_from_strings_or_objects() {
        assert_eq!(parsed_arguments(Some(&json!("{\"x\":1}"))), json!({"x": 1}));
        assert_eq!(parsed_arguments(Some(&json!({"y": 2}))), json!({"y": 2}));
        assert_eq!(parsed_arguments(Some(&json!("not json"))), json!({}));
        assert_eq!(parsed_arguments(None), json!({}));
    }

    #[test]
    fn tools_take_each_dialects_shape() {
        let t = OfferedTool {
            function: "a__add".into(),
            name: "a__add".into(),
            description: Some("Adds.".into()),
            input_schema: json!({"type": "object"}),
        };
        assert_eq!(function_tool(Flavor::Chat, &t)["function"]["name"], "a__add");
        assert_eq!(
            function_tool(Flavor::Messages, &t)["input_schema"]["type"],
            "object"
        );
        assert_eq!(function_tool(Flavor::Responses, &t)["type"], "function");
    }
}
