//! Tool search: a gateway that lists only three tools, which find the granted tools, show one
//! tool's definition and call one, checking its arguments against the tool's input schema before
//! any server is contacted.

use std::sync::Arc;

use antirion_config::schema::UserSchema;
use serde_json::{Value, json};

use crate::compile::ServerRt;
use crate::http::Req;
use crate::lists::{self, Kind, Mode};
use crate::rpc::Fail;
use crate::select::Selection;

pub(crate) const SEARCH: &str = "search_tools";
pub(crate) const SCHEMA: &str = "get_tool_schema";
pub(crate) const CALL: &str = "call_tool";

const DEFAULT_LIMIT: usize = 10;
const MAX_LIMIT: usize = 50;

/// Whether `name` is one of the three tools.
pub(crate) fn is_meta(name: &str) -> bool {
    matches!(name, SEARCH | SCHEMA | CALL)
}

/// The definitions `tools/list` answers with.
pub(crate) fn tools() -> Value {
    json!([
        {
            "name": SEARCH,
            "description": "Finds tools by words of their name or description. Returns names and descriptions; read a tool's input schema with get_tool_schema before calling it with call_tool.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Words to look for."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": MAX_LIMIT, "description": "How many tools to return (default 10)."}
                },
                "required": ["query"],
                "additionalProperties": false
            },
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": SCHEMA,
            "description": "Returns one tool's full definition, with its input schema.",
            "inputSchema": {
                "type": "object",
                "properties": {"name": {"type": "string", "description": "The tool's name, as search_tools returned it."}},
                "required": ["name"],
                "additionalProperties": false
            },
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": CALL,
            "description": "Calls one tool with arguments that satisfy its input schema.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "The tool's name."},
                    "arguments": {"type": "object", "description": "The tool's arguments."}
                },
                "required": ["name"],
                "additionalProperties": false
            }
        }
    ])
}

/// A tool result that reports a problem to the model.
fn error_result(text: &str) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": true})
}

fn json_result(v: &Value) -> Value {
    json!({
        "content": [{"type": "text", "text": v.to_string()}],
        "structuredContent": v,
        "isError": false,
    })
}

/// Every tool the caller may see on the gateway's servers, as `tools/list` would list them.
async fn catalog(
    req: &Req,
    sel: &Selection,
    mode: &Mode,
    servers: &[Arc<ServerRt>],
) -> Result<Vec<Value>, Fail> {
    let modern = matches!(mode, Mode::Modern { .. });
    let read = lists::entries(req, mode, servers, Kind::Tools, modern).await;
    let mut ok = Vec::new();
    let mut failed = Vec::new();
    for (server, r) in read {
        match r {
            Ok(e) => ok.push((server, e)),
            Err(e) => failed.push((server.name.clone(), e)),
        }
    }
    crate::fanout::policy(req, servers.len(), &failed)?;
    Ok(lists::merge(req, sel, Kind::Tools, &ok).items)
}

fn text_of(tool: &Value, key: &str) -> String {
    tool.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase()
}

/// The tools matching `query`: ranked by how many of its words a tool's name or description
/// contains, name matches first, then by name.
fn search(tools: &[Value], query: &str, limit: usize) -> Vec<Value> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut scored: Vec<(usize, usize, String, &Value)> = tools
        .iter()
        .filter_map(|t| {
            let name = text_of(t, "name");
            let description = text_of(t, "description");
            let in_name = terms.iter().filter(|w| name.contains(w.as_str())).count();
            let anywhere = terms
                .iter()
                .filter(|w| name.contains(w.as_str()) || description.contains(w.as_str()))
                .count();
            (anywhere > 0 || terms.is_empty()).then_some((anywhere, in_name, name, t))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then_with(|| a.2.cmp(&b.2)));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, _, _, t)| {
            json!({
                "name": t.get("name"),
                "description": t.get("description").cloned().unwrap_or(Value::Null),
            })
        })
        .collect()
}

/// What a call of one of the three tools asks for.
pub(crate) enum Asked {
    /// The tool's result, answered by the gateway.
    Answer(Value),
    /// A `tools/call` of the named tool with these arguments.
    Call { name: String, arguments: Value },
}

/// Answers `search_tools` and `get_tool_schema`, and checks a `call_tool`.
pub(crate) async fn ask(
    req: &Req,
    sel: &Selection,
    mode: &Mode,
    servers: &[Arc<ServerRt>],
    name: &str,
    args: &Value,
) -> Result<Asked, Fail> {
    let tools = catalog(req, sel, mode, servers).await?;
    let find = |n: &str| {
        tools
            .iter()
            .find(|t| t.get("name").and_then(Value::as_str) == Some(n))
    };
    match name {
        SEARCH => {
            let Some(query) = args.get("query").and_then(Value::as_str) else {
                return Ok(Asked::Answer(error_result("query must be a string.")));
            };
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .and_then(|l| usize::try_from(l).ok())
                .unwrap_or(DEFAULT_LIMIT)
                .clamp(1, MAX_LIMIT);
            Ok(Asked::Answer(json_result(
                &json!({"tools": search(&tools, query, limit)}),
            )))
        }
        SCHEMA => {
            let wanted = args.get("name").and_then(Value::as_str).unwrap_or_default();
            Ok(Asked::Answer(match find(wanted) {
                Some(t) => json_result(t),
                None => error_result(&format!("There is no tool named {wanted:?}.")),
            }))
        }
        _ => {
            let wanted = args.get("name").and_then(Value::as_str).unwrap_or_default();
            let Some(tool) = find(wanted) else {
                return Ok(Asked::Answer(error_result(&format!(
                    "There is no tool named {wanted:?}."
                ))));
            };
            let arguments = args.get("arguments").cloned().unwrap_or_else(|| json!({}));
            let schema = tool
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object"}));
            match UserSchema::compile(&schema) {
                Ok(s) => {
                    if let Some((at, why)) = s.first_violation(&arguments) {
                        let at = if at.is_empty() {
                            "arguments".to_owned()
                        } else {
                            format!("arguments.{at}")
                        };
                        return Ok(Asked::Answer(error_result(&format!(
                            "The arguments do not match the input schema of {wanted}: {at}: {why}"
                        ))));
                    }
                }
                Err(e) => {
                    return Ok(Asked::Answer(error_result(&format!(
                        "The input schema of {wanted} cannot be checked: {e}"
                    ))));
                }
            }
            Ok(Asked::Call {
                name: wanted.to_owned(),
                arguments,
            })
        }
    }
}

/// The `params` of the `tools/call` a checked `call_tool` becomes.
pub(crate) fn call_params(original: Option<&Value>, name: &str, arguments: Value) -> Value {
    let mut p = original.cloned().unwrap_or_else(|| json!({}));
    if let Some(o) = p.as_object_mut() {
        o.insert("name".into(), Value::from(name));
        o.insert("arguments".into(), arguments);
    }
    p
}

/// Answers a request of a tool-search gateway, or `None` for one the usual path serves.
pub(crate) async fn intercept(
    req: &Req,
    sel: &Selection,
    mode: &Mode,
    servers: &[Arc<ServerRt>],
    method: &str,
    params: Option<&Value>,
) -> Option<Result<Intercepted, Fail>> {
    if !req.gw.tool_search {
        return None;
    }
    match method {
        "tools/list" => Some(Ok(Intercepted::Result(json!({"tools": tools()})))),
        "tools/call" => {
            let name = params.and_then(|p| p.get("name")).and_then(Value::as_str)?;
            if !is_meta(name) {
                return None;
            }
            let args = params
                .and_then(|p| p.get("arguments"))
                .cloned()
                .unwrap_or_else(|| json!({}));
            Some(ask(req, sel, mode, servers, name, &args).await.map(|a| match a {
                Asked::Answer(v) => Intercepted::Result(v),
                Asked::Call { name, arguments } => {
                    Intercepted::Forward(call_params(params, &name, arguments))
                }
            }))
        }
        _ => None,
    }
}

/// What a tool-search gateway does with a request.
pub(crate) enum Intercepted {
    /// Answer with this result.
    Result(Value),
    /// Forward a `tools/call` with these params.
    Forward(Value),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_ranks_by_matching_words() {
        let tools = vec![
            json!({"name": "weather__forecast", "description": "Daily weather forecast for a city"}),
            json!({"name": "files__read", "description": "Read a file"}),
            json!({"name": "weather__alerts", "description": "Severe weather alerts"}),
        ];
        let hits = search(&tools, "weather forecast", 10);
        let names: Vec<&str> = hits.iter().filter_map(|h| h["name"].as_str()).collect();
        assert_eq!(names, ["weather__forecast", "weather__alerts"]);
        assert_eq!(search(&tools, "FILE", 10).len(), 1);
        assert_eq!(search(&tools, "weather", 1).len(), 1);
        assert!(search(&tools, "nothing", 10).is_empty());
    }

    #[test]
    fn the_three_tools() {
        let t = tools();
        let names: Vec<&str> = t
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|x| x["name"].as_str())
            .collect();
        assert_eq!(names, [SEARCH, SCHEMA, CALL]);
        let p = call_params(
            Some(&json!({"name": CALL, "_meta": {"k": 1}})),
            "a__b",
            json!({"x": 1}),
        );
        assert_eq!(
            p,
            json!({"name": "a__b", "arguments": {"x": 1}, "_meta": {"k": 1}})
        );
    }
}
