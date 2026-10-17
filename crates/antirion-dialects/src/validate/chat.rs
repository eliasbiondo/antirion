//! Chat Completions.

use serde_json::{Map, Value};

use super::{
    Check, Table, ValidateOpts, ViolationKind, array, bad_value, boolean, index, integer_in, key, missing,
    non_empty_string, number_in, object, string, violation, wrong_type,
};

pub(crate) static CHAT: Table = Table {
    member,
    required: &["messages"],
    finish: None,
    on_error: None,
};

const ROLES: &[&str] = &["system", "developer", "user", "assistant", "tool", "function"];

fn member(name: &str, v: &Value, _opts: &ValidateOpts) -> Check {
    if name == "messages" {
        return messages(v);
    }
    if v.is_null() {
        return Ok(());
    }
    match name {
        "temperature" => number_in(v, name, 0.0, 2.0),
        "top_p" => number_in(v, name, 0.0, 1.0),
        "presence_penalty" | "frequency_penalty" => number_in(v, name, -2.0, 2.0),
        "n" => integer_in(v, name, 1, Some(128)),
        "max_tokens" | "max_completion_tokens" => integer_in(v, name, 1, None),
        "top_logprobs" => integer_in(v, name, 0, Some(20)),
        "stop" => stop(v),
        "tools" => tools(v),
        "stream" => boolean(v, name),
        _ => Ok(()),
    }
}

fn messages(v: &Value) -> Check {
    let list = array(v, "messages")?;
    if list.is_empty() {
        return Err(bad_value("messages", "must contain at least one message."));
    }
    for (i, m) in list.iter().enumerate() {
        message(m, &index("messages", i))?;
    }
    Ok(())
}

fn message(v: &Value, path: &str) -> Check {
    let m = object(v, path)?;
    let role_path = key(path, "role");
    let role = match m.get("role") {
        None => return Err(missing(&role_path)),
        Some(r) => string(r, &role_path)?,
    };
    if !ROLES.contains(&role) {
        return Err(bad_value(
            &role_path,
            "must be one of system, developer, user, assistant, tool, function.",
        ));
    }
    content(m, role, &key(path, "content"))?;
    if role == "tool" {
        let id_path = key(path, "tool_call_id");
        match m.get("tool_call_id") {
            None => return Err(missing(&id_path)),
            Some(id) => {
                string(id, &id_path)?;
            }
        }
    }
    Ok(())
}

fn content(m: &Map<String, Value>, role: &str, path: &str) -> Check {
    match m.get("content") {
        None | Some(Value::Null) => {
            let assistant_without_text = role == "assistant"
                && ["tool_calls", "function_call", "refusal"]
                    .iter()
                    .any(|k| m.get(*k).is_some_and(|x| !x.is_null()));
            if assistant_without_text {
                return Ok(());
            }
            if m.get("content").is_none() {
                return Err(missing(path));
            }
            let kind = if role == "assistant" {
                ViolationKind::Value
            } else {
                ViolationKind::Type
            };
            Err(violation(
                path,
                kind,
                "may be null only for an assistant message with tool_calls, function_call or refusal.",
            ))
        }
        Some(Value::String(_)) => Ok(()),
        Some(Value::Array(parts)) => {
            for (i, p) in parts.iter().enumerate() {
                part(p, &index(path, i))?;
            }
            Ok(())
        }
        Some(_) => Err(wrong_type(path, "a string, an array of content parts or null")),
    }
}

fn part(v: &Value, path: &str) -> Check {
    let p = object(v, path)?;
    let type_path = key(path, "type");
    let kind = match p.get("type") {
        None => return Err(missing(&type_path)),
        Some(t) => string(t, &type_path)?,
    };
    match kind {
        "text" => required_string(p, path, "text"),
        "refusal" => required_string(p, path, "refusal"),
        "image_url" => {
            let img_path = key(path, "image_url");
            let img = match p.get("image_url") {
                None => return Err(missing(&img_path)),
                Some(i) => object(i, &img_path)?,
            };
            let url_path = key(&img_path, "url");
            match img.get("url") {
                None => Err(missing(&url_path)),
                Some(u) => non_empty_string(u, &url_path),
            }
        }
        "input_audio" | "file" => match p.get(kind) {
            None => Err(missing(&key(path, kind))),
            Some(x) => object(x, &key(path, kind)).map(|_| ()),
        },
        _ => Ok(()),
    }
}

fn required_string(p: &Map<String, Value>, path: &str, name: &str) -> Check {
    let at = key(path, name);
    match p.get(name) {
        None => Err(missing(&at)),
        Some(v) => string(v, &at).map(|_| ()),
    }
}

fn stop(v: &Value) -> Check {
    match v {
        Value::String(_) => Ok(()),
        Value::Array(items) => {
            for (i, s) in items.iter().enumerate() {
                string(s, &index("stop", i))?;
            }
            if items.is_empty() || items.len() > 4 {
                return Err(bad_value("stop", "must hold between 1 and 4 strings."));
            }
            Ok(())
        }
        _ => Err(wrong_type("stop", "a string or an array of strings")),
    }
}

fn tools(v: &Value) -> Check {
    for (i, t) in array(v, "tools")?.iter().enumerate() {
        tool(t, &index("tools", i))?;
    }
    Ok(())
}

fn tool(v: &Value, path: &str) -> Check {
    let t = object(v, path)?;
    let type_path = key(path, "type");
    let kind = match t.get("type") {
        None => return Err(missing(&type_path)),
        Some(k) => string(k, &type_path)?,
    };
    if kind != "function" {
        return Ok(());
    }
    let f_path = key(path, "function");
    let f = match t.get("function") {
        None => return Err(missing(&f_path)),
        Some(f) => object(f, &f_path)?,
    };
    required_string(f, &f_path, "name")?;
    if let Some(params) = f.get("parameters").filter(|p| !p.is_null()) {
        let p_path = key(&f_path, "parameters");
        let schema = object(params, &p_path)?;
        if !schema.contains_key("type") {
            return Err(missing(&key(&p_path, "type")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::super::ViolationKind::{Missing, Type, Value as Val};
    use super::super::tests::{check_at, rejects};

    const CHAT: &str = "/v1/chat/completions";

    fn body(overrides: Value) -> String {
        let mut b = json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]});
        for (k, v) in overrides.as_object().unwrap() {
            if v == "<absent>" {
                b.as_object_mut().unwrap().remove(k);
            } else {
                b[k] = v.clone();
            }
        }
        b.to_string()
    }

    #[test]
    fn invalid_values_name_the_field() {
        let u = json!({"role": "user", "content": "hi"});
        let cases = [
            (json!({"messages": "<absent>"}), "messages", Some(Missing)),
            (json!({"messages": "hello"}), "messages", Some(Type)),
            (json!({"messages": []}), "messages", Some(Val)),
            (
                json!({"messages": [u, {"role": "robot", "content": "x"}]}),
                "messages[1].role",
                Some(Val),
            ),
            (
                json!({"messages": [{"role": "user", "content": 5}]}),
                "messages[0].content",
                Some(Type),
            ),
            (
                json!({"messages": [{"role": "user", "content": null}]}),
                "messages[0].content",
                None,
            ),
            (
                json!({"messages": [u, {"role": "assistant", "content": null}, u]}),
                "messages[1].content",
                None,
            ),
            (
                json!({"messages": [{"role": "user", "content": [{"type": "image_url", "image_url": {}}]}]}),
                "messages[0].content[0].image_url.url",
                Some(Missing),
            ),
            (
                json!({"messages": [{"role": "user", "content": [{"type": "image_url", "image_url": {"url": ""}}]}]}),
                "messages[0].content[0].image_url.url",
                Some(Val),
            ),
            (
                json!({"messages": [u, {"role": "tool", "content": "r"}]}),
                "messages[1].tool_call_id",
                Some(Missing),
            ),
            (
                json!({"messages": [u, {"role": "tool", "content": "r", "tool_call_id": 7}]}),
                "messages[1].tool_call_id",
                Some(Type),
            ),
            (json!({"temperature": 2.01}), "temperature", Some(Val)),
            (json!({"temperature": -0.1}), "temperature", Some(Val)),
            (json!({"temperature": "1"}), "temperature", Some(Type)),
            (json!({"top_p": 1.5}), "top_p", Some(Val)),
            (json!({"top_p": -0.5}), "top_p", Some(Val)),
            (json!({"n": 0}), "n", Some(Val)),
            (json!({"n": 129}), "n", Some(Val)),
            (json!({"n": 1.5}), "n", Some(Type)),
            (json!({"max_tokens": 0}), "max_tokens", Some(Val)),
            (json!({"max_tokens": -5}), "max_tokens", Some(Val)),
            (
                json!({"max_completion_tokens": 0}),
                "max_completion_tokens",
                Some(Val),
            ),
            (json!({"max_tokens": "10"}), "max_tokens", Some(Type)),
            (json!({"presence_penalty": 2.5}), "presence_penalty", Some(Val)),
            (json!({"frequency_penalty": -2.5}), "frequency_penalty", Some(Val)),
            (json!({"top_logprobs": 21}), "top_logprobs", Some(Val)),
            (json!({"top_logprobs": -1}), "top_logprobs", Some(Val)),
            (json!({"stop": ["a", "b", "c", "d", "e"]}), "stop", Some(Val)),
            (json!({"stop": []}), "stop", Some(Val)),
            (json!({"stop": 5}), "stop", Some(Type)),
            (json!({"stop": [1, 2]}), "stop[0]", Some(Type)),
            (
                json!({"tools": [{"function": {"name": "f"}}]}),
                "tools[0].type",
                Some(Missing),
            ),
            (
                json!({"tools": [{"type": "function", "function": {}}]}),
                "tools[0].function.name",
                Some(Missing),
            ),
            (
                json!({"tools": [{"type": "function", "function": {"name": 3}}]}),
                "tools[0].function.name",
                Some(Type),
            ),
            (json!({"stream": "yes"}), "stream", Some(Type)),
        ];
        for (overrides, param, kind) in cases {
            rejects(CHAT, &body(overrides), param, kind);
        }
    }

    #[test]
    fn malformed_parts_are_rejected() {
        let cases = [
            (
                json!({"messages": [{"role": "user", "content": [42]}]}),
                "messages[0].content[0]",
            ),
            (
                json!({"messages": [{"role": "user", "content": [{"type": "image_url"}]}]}),
                "messages[0].content[0].image_url",
            ),
            (
                json!({"tools": [{"type": "function", "function": {"name": "f", "parameters": {}}}]}),
                "tools[0].function.parameters.type",
            ),
            (json!({"tools": [null]}), "tools[0]"),
            (json!({"messages": [null]}), "messages[0]"),
            (
                json!({"messages": [{"role": "user", "content": {"a": 1}}]}),
                "messages[0].content",
            ),
            (json!({"messages": [{"content": "x"}]}), "messages[0].role"),
        ];
        for (overrides, param) in cases {
            rejects(CHAT, &body(overrides), param, None);
        }
    }

    #[test]
    fn boundaries_and_open_values_pass() {
        let u = json!({"role": "user", "content": "hi"});
        let cases = [
            json!({"temperature": 0}),
            json!({"temperature": 2}),
            json!({"top_p": 0}),
            json!({"top_p": 1}),
            json!({"n": 1}),
            json!({"max_tokens": 1}),
            json!({"presence_penalty": -2, "frequency_penalty": 2}),
            json!({"top_logprobs": 20, "logprobs": true}),
            json!({"stop": "END"}),
            json!({"stop": ["a", "b", "c", "d"]}),
            json!({"temperature": null, "n": null, "stop": null, "stream": null}),
            json!({"messages": [{"role": "system", "content": "s"}, {"role": "developer", "content": "d"}, u,
                {"role": "assistant", "content": null, "tool_calls": [{"id": "c1", "type": "function",
                 "function": {"name": "f", "arguments": "{}"}}]},
                {"role": "tool", "tool_call_id": "c1", "content": "r"},
                {"role": "function", "name": "f", "content": "r"},
                {"role": "assistant", "content": null, "refusal": "no"}, u]}),
            json!({"messages": [{"role": "user", "content": [{"type": "text", "text": "a"},
                {"type": "image_url", "image_url": {"url": "https://example.com/a.png"}},
                {"type": "input_audio", "input_audio": {"data": "AAAA", "format": "wav"}},
                {"type": "file", "file": {"file_id": "file-1"}}]}]}),
            json!({"messages": [{"role": "user", "content": [{"type": "vendor_thing", "x": 1}, {"type": "text", "text": "a"}]}]}),
            json!({"reasoning_effort": "turbo", "max_tokens": 10_000_000, "logit_bias": {"50256": -100}, "prompt": "x"}),
            json!({"tools": [{"type": "function", "function": {"name": "f", "parameters": {"type": "object"}}},
                             {"type": "vendor_tool"}]}),
            json!({"stream": true, "stream_options": {"include_usage": true}}),
        ];
        for overrides in cases {
            let b = body(overrides);
            assert_eq!(check_at(CHAT, &b), Ok(()), "{b}");
        }
    }

    #[test]
    fn first_error_in_document_order() {
        rejects(
            CHAT,
            r#"{"temperature": 5, "model": "m", "messages": [{"role": "robot", "content": "x"}]}"#,
            "temperature",
            Some(Val),
        );
        rejects(
            CHAT,
            r#"{"model": "m", "messages": [{"role": "robot", "content": "x"}], "temperature": 5}"#,
            "messages[0].role",
            Some(Val),
        );
        rejects(CHAT, r#"{"model": "m"}"#, "messages", Some(Missing));
        rejects(CHAT, r#"{"temperature": 5}"#, "temperature", Some(Val));
    }

    #[test]
    fn model_is_not_checked_here() {
        assert_eq!(
            check_at(CHAT, r#"{"messages":[{"role":"user","content":"x"}]}"#),
            Ok(())
        );
        assert_eq!(
            check_at(CHAT, r#"{"model":5,"messages":[{"role":"user","content":"x"}]}"#),
            Ok(())
        );
    }
}
