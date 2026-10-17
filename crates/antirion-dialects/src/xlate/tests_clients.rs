use serde_json::{Value, json};

use super::decode::decode;
use super::encode::{Restore, encode};
use super::ir::Client;
use super::resp::{self, Cx};
use super::stream::{Frame, StreamCx, Translator};
use super::target::{Policy, Target, Upstream};
use super::warn::{RefusalKind, Warnings};

fn encoded(client: Client, body: &Value, up: Upstream) -> super::encode::Encoded {
    let ir = decode(client, body).unwrap();
    encode(&ir, &Target::defaults(up), &Policy::default(), None).unwrap()
}

fn respond(
    client: Client,
    up: Upstream,
    restore: &Restore,
    request: &Value,
    body: &Value,
) -> (Value, Warnings) {
    let cx = Cx {
        upstream: up,
        client,
        restore,
        provider: "u",
        model: "m",
        created: 1,
        fallback_id: "fb",
        request,
    };
    let mut w = Warnings::default();
    let out = resp::translate(body, &cx, &mut w).unwrap();
    (out, w)
}

fn stream(client: Client, up: Upstream, restore: Restore, events: &[(Option<&str>, Value)]) -> Vec<Frame> {
    let mut t = Translator::new(StreamCx {
        client,
        upstream: up,
        restore,
        provider: "u".into(),
        model: "m".into(),
        created: 1,
        fallback_id: "fb".into(),
        request: json!({}),
        include_usage: false,
        response_id: None,
    });
    let mut out = Vec::new();
    for (name, e) in events {
        out.extend(t.push(*name, e).unwrap());
    }
    out.extend(t.finish());
    out
}

#[test]
fn gemini_request_reaches_chat_with_ids_paired_by_name() {
    let body = json!({
        "systemInstruction": {"parts": [{"text": "Be brief."}]},
        "contents": [
            {"role": "user", "parts": [{"text": "weather in Paris?"}]},
            {"role": "model", "parts": [{"functionCall": {"name": "get_weather", "args": {"city": "Paris"}}}]},
            {"role": "user", "parts": [{"functionResponse": {"name": "get_weather", "response": {"content": "18C"}}}]}
        ],
        "tools": [{"functionDeclarations": [{"name": "get_weather",
                   "parameters": {"type": "OBJECT", "properties": {"city": {"type": "STRING"}}}}]}],
        "toolConfig": {"functionCallingConfig": {"mode": "ANY"}},
        "generationConfig": {"temperature": 0.2, "maxOutputTokens": 50, "stopSequences": ["END"],
                             "responseMimeType": "application/json"}
    });
    let e = encoded(Client::Gemini, &body, Upstream::Chat);
    let b = &e.body;
    assert_eq!(
        b["messages"][0],
        json!({"role": "system", "content": "Be brief."})
    );
    let call = &b["messages"][2]["tool_calls"][0];
    assert_eq!(call["id"], "call_1_0");
    assert_eq!(call["function"]["arguments"], "{\"city\":\"Paris\"}");
    assert_eq!(b["messages"][3]["tool_call_id"], "call_1_0");
    assert_eq!(b["messages"][3]["content"], "18C");
    assert_eq!(
        b["tools"][0]["function"]["parameters"],
        json!({"type": "object", "properties": {"city": {"type": "string"}}})
    );
    assert_eq!(b["tool_choice"], "required");
    assert_eq!(b["temperature"], 0.2);
    assert_eq!(b["stop"], json!(["END"]));
    assert_eq!(b["response_format"], json!({"type": "json_object"}));
}

#[test]
fn chat_answer_reaches_a_gemini_client() {
    let req = json!({});
    let (out, _) = respond(
        Client::Gemini,
        Upstream::Chat,
        &Restore::default(),
        &req,
        &json!({"id": "c1", "choices": [{"message": {"role": "assistant", "content": "Hi.",
            "tool_calls": [{"id": "call_a", "type": "function", "function": {"name": "f", "arguments": "{\"x\":1}"}}]},
            "finish_reason": "tool_calls"}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 3, "completion_tokens_details": {"reasoning_tokens": 1}}}),
    );
    let c = &out["candidates"][0];
    assert_eq!(c["content"]["parts"][0], json!({"text": "Hi."}));
    assert_eq!(
        c["content"]["parts"][1]["functionCall"],
        json!({"id": "call_a", "name": "f", "args": {"x": 1}})
    );
    assert_eq!(c["finishReason"], "STOP");
    assert_eq!(
        out["usageMetadata"],
        json!({"promptTokenCount": 5, "candidatesTokenCount": 2, "totalTokenCount": 8, "thoughtsTokenCount": 1})
    );
    assert_eq!(out["modelVersion"], "m");
}

#[test]
fn converse_request_reaches_anthropic() {
    let body = json!({
        "system": [{"text": "S"}, {"cachePoint": {"type": "default"}}],
        "messages": [
            {"role": "user", "content": [{"text": "hi"}]},
            {"role": "assistant", "content": [{"toolUse": {"toolUseId": "t1", "name": "f", "input": {"a": 1}}}]},
            {"role": "user", "content": [{"toolResult": {"toolUseId": "t1", "content": [{"json": {"ok": true}}],
                                                         "status": "error"}}]}
        ],
        "inferenceConfig": {"maxTokens": 100, "stopSequences": ["X"]},
        "toolConfig": {"tools": [{"toolSpec": {"name": "f", "inputSchema": {"json": {"type": "object"}}}}],
                       "toolChoice": {"tool": {"name": "f"}}},
        "additionalModelRequestFields": {"top_k": 5}
    });
    let e = encoded(Client::Converse, &body, Upstream::Anthropic);
    let b = &e.body;
    assert_eq!(
        b["system"],
        json!([{"type": "text", "text": "S", "cache_control": {"type": "ephemeral"}}])
    );
    assert_eq!(b["max_tokens"], 100);
    assert_eq!(b["stop_sequences"], json!(["X"]));
    assert_eq!(b["top_k"], 5);
    assert_eq!(b["tool_choice"], json!({"type": "tool", "name": "f"}));
    let result = &b["messages"][2]["content"][0];
    assert_eq!(result["type"], "tool_result");
    assert_eq!(result["is_error"], true);
    assert_eq!(result["content"], "{\"ok\":true}");
    assert_eq!(result["tool_use_id"], b["messages"][1]["content"][0]["id"]);
}

#[test]
fn anthropic_answer_reaches_a_converse_client() {
    let req = json!({});
    let (out, _) = respond(
        Client::Converse,
        Upstream::Anthropic,
        &Restore::default(),
        &req,
        &json!({"id": "msg_1", "content": [
            {"type": "thinking", "thinking": "hmm", "signature": "sig"},
            {"type": "text", "text": "Done."}],
            "stop_reason": "stop_sequence", "stop_sequence": "X",
            "usage": {"input_tokens": 3, "cache_read_input_tokens": 2, "output_tokens": 4}}),
    );
    assert_eq!(
        out["output"]["message"]["content"],
        json!([{"reasoningContent": {"reasoningText": {"text": "hmm", "signature": "sig"}}}, {"text": "Done."}])
    );
    assert_eq!(out["stopReason"], "stop_sequence");
    assert_eq!(out["additionalModelResponseFields"]["stop_sequence"], "X");
    assert_eq!(
        out["usage"],
        json!({"inputTokens": 3, "outputTokens": 4, "totalTokens": 9, "cacheReadInputTokens": 2,
               "cacheWriteInputTokens": 0})
    );
}

#[test]
fn anthropic_stream_reaches_a_converse_client() {
    let events = [
        json!({"type": "message_start", "message": {"id": "msg_1", "usage": {"input_tokens": 4}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "hi"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "t", "name": "f", "input": {}}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"a\":"}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "1}"}}),
        json!({"type": "content_block_stop", "index": 1}),
        json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 2}}),
    ];
    let events: Vec<(Option<&str>, Value)> = events.into_iter().map(|e| (None, e)).collect();
    let frames = stream(Client::Converse, Upstream::Anthropic, Restore::default(), &events);
    let kinds: Vec<&str> = frames
        .iter()
        .filter_map(|f| match f {
            Frame::Event(k, _) => Some(*k),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "messageStart",
            "contentBlockDelta",
            "contentBlockStop",
            "contentBlockStart",
            "contentBlockDelta",
            "contentBlockStop",
            "messageStop",
            "metadata"
        ]
    );
    let Some(Frame::Event(_, tool)) = frames.get(4) else {
        panic!("no tool delta");
    };
    assert_eq!(tool["delta"]["toolUse"]["input"], "{\"a\":1}");
    let Some(Frame::Event(_, meta)) = frames.last() else {
        panic!("no metadata");
    };
    assert_eq!(meta["usage"]["inputTokens"], 4);
    assert_eq!(meta["usage"]["outputTokens"], 2);
}

#[test]
fn converse_stream_reaches_a_gemini_client() {
    let events = [
        (Some("messageStart"), json!({"role": "assistant"})),
        (
            Some("contentBlockDelta"),
            json!({"contentBlockIndex": 0, "delta": {"text": "Hi"}}),
        ),
        (Some("contentBlockStop"), json!({"contentBlockIndex": 0})),
        (Some("messageStop"), json!({"stopReason": "max_tokens"})),
        (
            Some("metadata"),
            json!({"usage": {"inputTokens": 2, "outputTokens": 1}}),
        ),
    ];
    let frames = stream(Client::Gemini, Upstream::Converse, Restore::default(), &events);
    let chunks: Vec<&Value> = frames
        .iter()
        .filter_map(|f| match f {
            Frame::Gemini(v) => Some(v),
            _ => None,
        })
        .collect();
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0]["candidates"][0]["content"]["parts"][0]["text"], "Hi");
    assert_eq!(chunks[1]["candidates"][0]["finishReason"], "MAX_TOKENS");
    assert_eq!(chunks[1]["usageMetadata"]["promptTokenCount"], 2);
}

#[test]
fn responses_custom_tools_round_trip_through_chat() {
    let body = json!({"model": "m", "input": [
            {"role": "user", "content": "run it"},
            {"type": "custom_tool_call", "call_id": "c0", "name": "shell", "input": "ls"},
            {"type": "custom_tool_call_output", "call_id": "c0", "output": "a.txt"}],
        "tools": [{"type": "custom", "name": "shell", "description": "Runs a command",
                   "format": {"type": "grammar", "syntax": "lark", "definition": "start: /.+/"}}]});
    let e = encoded(Client::Responses, &body, Upstream::Chat);
    assert_eq!(
        e.body["tools"][0]["function"]["parameters"],
        json!({"type": "object", "properties": {"input": {"type": "string"}}, "required": ["input"]})
    );
    assert_eq!(
        e.body["messages"][1]["tool_calls"][0]["function"]["arguments"],
        "{\"input\":\"ls\"}"
    );
    assert_eq!(e.warnings.entries(), ["approximated=tools[].format"]);
    assert_eq!(e.restore.custom, ["shell"]);
    let (out, _) = respond(
        Client::Responses,
        Upstream::Chat,
        &e.restore,
        &body,
        &json!({"choices": [{"message": {"role": "assistant", "tool_calls": [{"id": "c1", "type": "function",
            "function": {"name": "shell", "arguments": "{\"input\":\"cat a.txt\"}"}}]}, "finish_reason": "tool_calls"}]}),
    );
    let item = &out["output"][0];
    assert_eq!(item["type"], "custom_tool_call");
    assert_eq!(item["call_id"], "c1");
    assert_eq!(item["input"], "cat a.txt");
}

#[test]
fn custom_tool_calls_stream_to_responses_clients_whole() {
    let restore = Restore {
        custom: vec!["shell".into()],
        ..Restore::default()
    };
    let chunk = |delta: Value, finish: Option<&str>| json!({"id": "c", "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]});
    let events = [
        (
            None,
            chunk(
                json!({"tool_calls": [{"index": 0, "id": "c1", "type": "function", "function": {"name": "shell", "arguments": ""}}]}),
                None,
            ),
        ),
        (
            None,
            chunk(
                json!({"tool_calls": [{"index": 0, "function": {"arguments": "{\"input\":\"ls\"}"}}]}),
                None,
            ),
        ),
        (None, chunk(json!({}), Some("tool_calls"))),
    ];
    let frames = stream(Client::Responses, Upstream::Chat, restore, &events);
    let types: Vec<&str> = frames
        .iter()
        .filter_map(|f| match f {
            Frame::Named(v) => v["type"].as_str(),
            _ => None,
        })
        .collect();
    assert!(types.contains(&"response.custom_tool_call_input.delta"));
    assert!(!types.contains(&"response.function_call_arguments.delta"));
    let done = frames.iter().find_map(|f| match f {
        Frame::Named(v) if v["type"] == "response.custom_tool_call_input.done" => Some(v["input"].clone()),
        _ => None,
    });
    assert_eq!(done, Some(json!("ls")));
}

#[test]
fn an_unpaired_tool_result_is_refused_with_its_path() {
    let body = json!({"model": "m", "messages": [
        {"role": "user", "content": "hi"},
        {"role": "assistant", "content": null, "tool_calls": [
            {"id": "a", "type": "function", "function": {"name": "f", "arguments": "{}"}}]},
        {"role": "tool", "tool_call_id": "nope", "content": "x"}]});
    let ir = decode(Client::Chat, &body).unwrap();
    let r = encode(
        &ir,
        &Target::defaults(Upstream::Anthropic),
        &Policy::default(),
        None,
    )
    .unwrap_err();
    assert_eq!(r.kind, RefusalKind::Unpaired);
    assert_eq!(r.param, "messages[2].tool_call_id");
}

#[test]
fn a_streamed_anthropic_thought_round_trips_through_a_gemini_client() {
    let events = [
        json!({"type": "message_start", "message": {"id": "msg_1", "usage": {"input_tokens": 4}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "Let me "}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "think."}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "t", "name": "f", "input": {}}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{}"}}),
        json!({"type": "content_block_stop", "index": 1}),
        json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 2}}),
    ];
    let events: Vec<(Option<&str>, Value)> = events.into_iter().map(|e| (None, e)).collect();
    let frames = stream(Client::Gemini, Upstream::Anthropic, Restore::default(), &events);
    let parts: Vec<Value> = frames
        .iter()
        .filter_map(|f| match f {
            Frame::Gemini(v) => v["candidates"][0]["content"]["parts"].as_array().cloned(),
            _ => None,
        })
        .flatten()
        .filter(|p| p != &json!({"text": ""}))
        .collect();
    let call = parts.iter().find(|p| p.get("functionCall").is_some()).unwrap();
    assert!(
        call.get("thoughtSignature").is_none(),
        "the thought's signature stays on the thought"
    );
    let history = json!({"contents": [
        {"role": "user", "parts": [{"text": "go"}]},
        {"role": "model", "parts": parts},
        {"role": "user", "parts": [{"functionResponse": {"name": "f", "response": {"content": "ok"}}}]}]});
    let e = encoded(Client::Gemini, &history, Upstream::Anthropic);
    assert_eq!(
        e.body["messages"][1]["content"][0],
        json!({"type": "thinking", "thinking": "Let me think.", "signature": "sig"})
    );
}
