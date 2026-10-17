use serde_json::{Value, json};

use super::decode::decode;
use super::encode::{Encoded, Restore, encode};
use super::ir::{Client, Family};
use super::resp::{self, Bad, Cx};
use super::stream::{Frame, StreamCx, Translator};
use super::target::{Control, Policy, Target, Upstream};
use super::warn::{RefusalKind, Warnings};

fn encoded(client: Client, body: &Value, t: &Target, policy: &Policy) -> Encoded {
    let ir = decode(client, body).unwrap();
    encode(&ir, t, policy, None).unwrap()
}

fn translate(client: Client, body: Value, up: Upstream) -> Value {
    encoded(client, &body, &Target::defaults(up), &Policy::default()).body
}

#[test]
fn responses_text_format_reaches_chat() {
    let schema = json!({"type": "object", "properties": {"answer": {"type": "string"}}});
    let body = json!({"model": "m", "input": [{"role": "user", "content": "hi"}],
                      "text": {"format": {"type": "json_schema", "name": "answer", "schema": schema, "strict": true}}});
    let out = translate(Client::Responses, body, Upstream::Chat);
    assert_eq!(
        out["response_format"],
        json!({"type": "json_schema", "json_schema": {"name": "answer", "schema": schema, "strict": true}})
    );
}

#[test]
fn chat_to_anthropic_system_turns_and_tools() {
    let body = json!({"model": "m", "max_tokens": 50, "messages": [
        {"role": "system", "content": "S"},
        {"role": "user", "content": "weather?"},
        {"role": "assistant", "content": null, "tool_calls": [
            {"id": "call:1", "type": "function", "function": {"name": "get.weather", "arguments": "{\"b\":1,\"a\":2}"}}]},
        {"role": "tool", "tool_call_id": "call:1", "content": "18C"},
        {"role": "user", "content": "thanks"}],
        "tools": [{"type": "function", "function": {"name": "get.weather", "parameters": {"type": "object"}}}]});
    let e = encoded(
        Client::Chat,
        &body,
        &Target::defaults(Upstream::Anthropic),
        &Policy::default(),
    );
    let b = &e.body;
    assert_eq!(b["system"], "S");
    assert_eq!(b["tools"][0]["name"], "get_weather");
    let call = &b["messages"][1]["content"][0];
    assert_eq!(call["name"], "get_weather");
    assert!(call["id"].as_str().unwrap().starts_with("toolu_"));
    let keys: Vec<&String> = call["input"].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["b", "a"]);
    let user = b["messages"][2]["content"].as_array().unwrap();
    assert_eq!(user[0]["type"], "tool_result");
    assert_eq!(user[0]["tool_use_id"], call["id"]);
    assert_eq!(user[1], json!({"type": "text", "text": "thanks"}));
    assert_eq!(e.restore.names.client("get_weather"), "get.weather");
    assert_eq!(e.restore.ids.client(call["id"].as_str().unwrap()), "call:1");
}

#[test]
fn lossy_fields_follow_the_policy() {
    let body = json!({"model": "m", "max_tokens": 50, "messages": [{"role": "user", "content": "hi"}],
                      "prompt_cache_key": "pk", "logit_bias": {"1": 5}});
    let t = Target::defaults(Upstream::Anthropic);
    let e = encoded(Client::Chat, &body, &t, &Policy::default());
    assert_eq!(
        e.warnings.entries(),
        ["ignored=prompt_cache_key", "dropped=logit_bias"]
    );
    assert!(e.body.get("logit_bias").is_none());
    let strict = Policy {
        reject: true,
        ..Policy::default()
    };
    let ir = decode(Client::Chat, &body).unwrap();
    let refusal = encode(&ir, &t, &strict, None).unwrap_err();
    assert_eq!(
        (refusal.kind, refusal.param.as_str()),
        (RefusalKind::Strict, "logit_bias")
    );
}

#[test]
fn unsupported_features_refuse_with_the_first_field() {
    let body = json!({"model": "m", "messages": [{"role": "user", "content": "hi"}],
                      "modalities": ["text", "audio"], "audio": {"voice": "alloy"}});
    let ir = decode(Client::Chat, &body).unwrap();
    let r = encode(&ir, &Target::defaults(Upstream::Gemini), &Policy::default(), None).unwrap_err();
    assert_eq!(
        (r.kind, r.param.as_str()),
        (RefusalKind::Unsupported, "modalities")
    );
}

#[test]
fn cache_markers_beyond_the_limit_go_from_the_earliest() {
    let mark = json!({"type": "ephemeral"});
    let messages: Vec<Value> = (0..5)
        .map(|i| json!({"role": "user", "content": [{"type": "text", "text": format!("t{i}"), "cache_control": mark}]}))
        .collect();
    let body = json!({"model": "m", "max_tokens": 50, "messages": messages});
    let e = encoded(
        Client::Chat,
        &body,
        &Target::defaults(Upstream::Anthropic),
        &Policy::default(),
    );
    let blocks = e.body["messages"][0]["content"].as_array().unwrap();
    assert!(blocks[0].get("cache_control").is_none());
    assert_eq!(
        blocks.iter().filter(|b| b.get("cache_control").is_some()).count(),
        4
    );
    assert_eq!(
        e.warnings.entries(),
        ["dropped=messages[].content[].cache_control"]
    );
}

#[test]
fn effort_becomes_a_budget_below_max_tokens() {
    let mut t = Target::defaults(Upstream::Anthropic);
    t.control = Control::Budget;
    let body = json!({"model": "m", "max_tokens": 2000, "reasoning_effort": "high", "temperature": 0.2,
                      "messages": [{"role": "user", "content": "hi"}]});
    let e = encoded(Client::Chat, &body, &t, &Policy::default());
    assert_eq!(
        e.body["thinking"],
        json!({"type": "enabled", "budget_tokens": 1999})
    );
    assert!(e.body.get("temperature").is_none());
    assert!(e.warnings.entries().contains(&"dropped=temperature".to_owned()));
}

#[test]
fn converse_and_gemini_tool_history() {
    let body = json!({"model": "m", "messages": [
        {"role": "user", "content": "weather?"},
        {"role": "assistant", "content": null, "tool_calls": [
            {"id": "call_1", "type": "function", "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}}]},
        {"role": "tool", "tool_call_id": "call_1", "content": "{\"temp\":18}"}]});
    let cv = translate(Client::Chat, body.clone(), Upstream::Converse);
    assert_eq!(cv["toolConfig"]["tools"][0]["toolSpec"]["name"], "get_weather");
    assert_eq!(cv["messages"][2]["content"][0]["toolResult"]["status"], "success");
    let ge = translate(Client::Chat, body, Upstream::Gemini);
    assert_eq!(
        ge["contents"][2]["parts"][0],
        json!({"functionResponse": {"id": "call_1", "name": "get_weather", "response": {"temp": 18}}})
    );
}

fn cx<'a>(client: Client, upstream: Upstream, restore: &'a Restore, request: &'a Value) -> Cx<'a> {
    Cx {
        upstream,
        client,
        restore,
        provider: "u",
        model: "m",
        created: 1,
        fallback_id: "x",
        request,
    }
}

#[test]
fn anthropic_usage_and_reasoning_reach_chat() {
    let up = json!({"id": "msg_1", "type": "message", "content": [
        {"type": "thinking", "thinking": "hmm", "signature": "sig"},
        {"type": "text", "text": "Hello "}, {"type": "text", "text": "world"}],
        "stop_reason": "end_turn", "usage": {"input_tokens": 10, "cache_read_input_tokens": 5,
                                             "cache_creation_input_tokens": 3, "output_tokens": 7}});
    let restore = Restore::default();
    let req = json!({});
    let mut w = Warnings::default();
    let out = resp::translate(
        &up,
        &cx(Client::Chat, Upstream::Anthropic, &restore, &req),
        &mut w,
    )
    .unwrap();
    let msg = &out["choices"][0]["message"];
    assert_eq!(msg["content"], "Hello world");
    assert_eq!(msg["reasoning_details"][0]["signature"], "sig");
    assert_eq!(out["usage"]["prompt_tokens"], 18);
    assert_eq!(out["usage"]["total_tokens"], 25);
    assert_eq!(out["usage"]["prompt_tokens_details"]["cache_write_tokens"], 3);
}

#[test]
fn openai_reasoning_reaches_anthropic_clients_enveloped() {
    let up = json!({"id": "resp_1", "status": "completed", "output": [
        {"type": "reasoning", "summary": [{"type": "summary_text", "text": "s"}], "encrypted_content": "ENC"}]});
    let restore = Restore::default();
    let req = json!({});
    let mut w = Warnings::default();
    let out = resp::translate(
        &up,
        &cx(Client::Anthropic, Upstream::Responses, &restore, &req),
        &mut w,
    )
    .unwrap();
    let sig = out["content"][0]["signature"].as_str().unwrap();
    let u = super::envelope::unwrap(sig).unwrap();
    assert_eq!(
        (u.family, u.provider.as_deref(), u.payload.as_str()),
        (Family::OpenAi, Some("u"), "ENC")
    );
}

#[test]
fn malformed_gemini_call_is_a_failed_attempt() {
    let up = json!({"candidates": [{"finishReason": "MALFORMED_FUNCTION_CALL", "index": 0}]});
    let restore = Restore::default();
    let req = json!({});
    let mut w = Warnings::default();
    let r = resp::translate(&up, &cx(Client::Chat, Upstream::Gemini, &restore, &req), &mut w);
    assert_eq!(r, Err(Bad::Malformed));
}

fn stream(client: Client, upstream: Upstream, events: &[Value]) -> Vec<Frame> {
    let mut t = Translator::new(StreamCx {
        client,
        upstream,
        restore: Restore::default(),
        provider: "u".into(),
        model: "m".into(),
        created: 1,
        fallback_id: "x".into(),
        request: json!({}),
        include_usage: true,
        response_id: None,
    });
    let mut out = Vec::new();
    for e in events {
        out.extend(t.push(None, e).unwrap());
    }
    out.extend(t.finish());
    out
}

fn chat_chunk(delta: Value, finish: Option<&str>) -> Value {
    json!({"id": "c", "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
}

#[test]
fn interleaved_chat_tool_calls_reach_anthropic_one_block_at_a_time() {
    let tc = |k: u64, mut element: Value| {
        element["index"] = k.into();
        chat_chunk(json!({"tool_calls": [element]}), None)
    };
    let events = [
        chat_chunk(json!({"content": "Checking."}), None),
        tc(
            0,
            json!({"id": "a", "type": "function", "function": {"name": "f", "arguments": ""}}),
        ),
        tc(
            1,
            json!({"id": "b", "type": "function", "function": {"name": "f", "arguments": ""}}),
        ),
        tc(0, json!({"function": {"arguments": "{\"x\":1}"}})),
        tc(1, json!({"function": {"arguments": "{\"x\":2}"}})),
        chat_chunk(json!({}), Some("tool_calls")),
    ];
    let frames = stream(Client::Anthropic, Upstream::Chat, &events);
    let types: Vec<String> = frames
        .iter()
        .filter_map(|f| match f {
            Frame::Named(v) => v["type"].as_str().map(str::to_owned),
            _ => None,
        })
        .collect();
    let starts = types.iter().filter(|t| *t == "content_block_start").count();
    let stops = types.iter().filter(|t| *t == "content_block_stop").count();
    assert_eq!((starts, stops), (3, 3));
    assert_eq!(types.first().map(String::as_str), Some("message_start"));
    assert_eq!(types.last().map(String::as_str), Some("message_stop"));
    let open_order: Vec<&str> = frames
        .iter()
        .filter_map(|f| match f {
            Frame::Named(v) if v["type"] == "content_block_start" => v["content_block"]["id"].as_str(),
            _ => None,
        })
        .collect();
    assert_eq!(open_order, ["a", "b"]);
}

#[test]
fn anthropic_stream_reaches_chat_with_one_finish_and_usage() {
    let events = [
        json!({"type": "message_start", "message": {"id": "msg_1", "usage": {"input_tokens": 4}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "hi"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 2}}),
        json!({"type": "message_stop"}),
    ];
    let frames = stream(Client::Chat, Upstream::Anthropic, &events);
    let finishes: Vec<&Value> = frames
        .iter()
        .filter_map(|f| match f {
            Frame::Data(v) => v["choices"]
                .get(0)
                .and_then(|c| c.get("finish_reason"))
                .filter(|f| !f.is_null()),
            _ => None,
        })
        .collect();
    assert_eq!(finishes, [&json!("stop")]);
    let usage = frames.iter().find_map(|f| match f {
        Frame::Data(v) if v.get("usage").is_some() => Some(v["usage"].clone()),
        _ => None,
    });
    assert_eq!(usage.unwrap()["total_tokens"], 6);
    assert_eq!(frames.last(), Some(&Frame::Done));
}

#[test]
fn orphan_anthropic_delta_is_malformed() {
    let mut t = Translator::new(StreamCx {
        client: Client::Chat,
        upstream: Upstream::Anthropic,
        restore: Restore::default(),
        provider: "u".into(),
        model: "m".into(),
        created: 1,
        fallback_id: "x".into(),
        request: json!({}),
        include_usage: false,
        response_id: None,
    });
    let orphan =
        json!({"type": "content_block_delta", "index": 7, "delta": {"type": "text_delta", "text": "x"}});
    assert!(t.push(None, &orphan).is_err());
}
