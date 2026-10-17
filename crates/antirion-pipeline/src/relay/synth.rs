//! Streams synthesized from a complete response for Anthropic, Responses, Gemini and ConverseStream
//! clients: every event at once, in the dialect's grammar.

use bytes::{BufMut, Bytes, BytesMut};
use serde_json::{Value, json};

use super::events::StreamFacts;
use super::render::{converse_frame, named};

/// The Anthropic events of a complete message.
pub(crate) fn anthropic(msg: &Value, facts: &mut StreamFacts) -> Option<Vec<Bytes>> {
    let content = msg.get("content")?.as_array()?;
    let usage = msg.get("usage").cloned().unwrap_or_else(|| json!({}));
    let mut start = msg.clone();
    if let Some(o) = start.as_object_mut() {
        o.insert("content".into(), json!([]));
        o.insert("stop_reason".into(), Value::Null);
        o.insert("stop_sequence".into(), Value::Null);
        o.insert(
            "usage".into(),
            json!({"input_tokens": usage.get("input_tokens").cloned().unwrap_or(json!(0)), "output_tokens": 0}),
        );
    }
    let mut out = vec![named(&json!({"type": "message_start", "message": start}))];
    let mut index = 0u64;
    for block in content {
        let (first, deltas) = match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = block.get("text").and_then(Value::as_str).unwrap_or_default();
                facts.output_chars += u64::try_from(text.chars().count()).unwrap_or(0);
                (
                    json!({"type": "text", "text": ""}),
                    vec![json!({"type": "text_delta", "text": text})],
                )
            }
            Some("tool_use") => {
                let mut b = block.clone();
                if let Some(o) = b.as_object_mut() {
                    o.insert("input".into(), json!({}));
                }
                let input = block
                    .get("input")
                    .map(Value::to_string)
                    .unwrap_or_else(|| "{}".to_owned());
                (
                    b,
                    vec![json!({"type": "input_json_delta", "partial_json": input})],
                )
            }
            Some("thinking") => {
                let mut deltas = Vec::new();
                if let Some(t) = block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .filter(|t| !t.is_empty())
                {
                    deltas.push(json!({"type": "thinking_delta", "thinking": t}));
                }
                let sig = block.get("signature").cloned().unwrap_or(json!(""));
                deltas.push(json!({"type": "signature_delta", "signature": sig}));
                (
                    json!({"type": "thinking", "thinking": "", "signature": ""}),
                    deltas,
                )
            }
            Some("redacted_thinking") => (block.clone(), Vec::new()),
            _ => continue,
        };
        out.push(named(
            &json!({"type": "content_block_start", "index": index, "content_block": first}),
        ));
        for delta in deltas {
            out.push(named(
                &json!({"type": "content_block_delta", "index": index, "delta": delta}),
            ));
        }
        out.push(named(&json!({"type": "content_block_stop", "index": index})));
        index += 1;
    }
    let stop = msg.get("stop_reason").cloned().unwrap_or(json!("end_turn"));
    if let Some(r) = stop.as_str() {
        facts.finish_reasons.push(r.to_owned());
    }
    out.push(named(&json!({
        "type": "message_delta",
        "delta": {"stop_reason": stop, "stop_sequence": msg.get("stop_sequence").cloned().unwrap_or(Value::Null)},
        "usage": usage,
    })));
    out.push(named(&json!({"type": "message_stop"})));
    Some(out)
}

/// The Responses events of a complete response.
pub(crate) fn responses(resp: &Value, facts: &mut StreamFacts) -> Option<Vec<Bytes>> {
    let output = resp.get("output")?.as_array()?;
    let mut seq = 0i64;
    let mut out = Vec::new();
    let mut emit = |mut v: Value, out: &mut Vec<Bytes>| {
        if let Some(o) = v.as_object_mut() {
            o.insert("sequence_number".into(), Value::from(seq));
        }
        seq += 1;
        out.push(named(&v));
    };
    let mut started = resp.clone();
    if let Some(o) = started.as_object_mut() {
        o.insert("status".into(), json!("in_progress"));
        o.insert("output".into(), json!([]));
        o.insert("usage".into(), Value::Null);
    }
    emit(json!({"type": "response.created", "response": started}), &mut out);
    emit(
        json!({"type": "response.in_progress", "response": started}),
        &mut out,
    );
    for (i, item) in output.iter().enumerate() {
        let item_id = item.get("id").cloned().unwrap_or(Value::Null);
        let mut added = item.clone();
        if let Some(o) = added.as_object_mut() {
            o.insert("status".into(), json!("in_progress"));
            if o.contains_key("content") {
                o.insert("content".into(), json!([]));
            }
        }
        emit(
            json!({"type": "response.output_item.added", "output_index": i, "item": added}),
            &mut out,
        );
        for (j, part) in item
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let text = part.get("text").and_then(Value::as_str).unwrap_or_default();
            facts.output_chars += u64::try_from(text.chars().count()).unwrap_or(0);
            let empty = json!({"type": "output_text", "text": "", "annotations": []});
            let ev = |kind: &str, field: &str, value: Value| json!({"type": kind, "item_id": item_id, "output_index": i, "content_index": j, (field): value});
            emit(ev("response.content_part.added", "part", empty), &mut out);
            emit(ev("response.output_text.delta", "delta", json!(text)), &mut out);
            emit(ev("response.output_text.done", "text", json!(text)), &mut out);
            emit(ev("response.content_part.done", "part", part.clone()), &mut out);
        }
        emit(
            json!({"type": "response.output_item.done", "output_index": i, "item": item}),
            &mut out,
        );
    }
    let done = if resp.get("status").and_then(Value::as_str) == Some("incomplete") {
        "response.incomplete"
    } else {
        "response.completed"
    };
    emit(json!({"type": done, "response": resp}), &mut out);
    facts.sequence = Some(seq - 1);
    Some(out)
}

/// One Gemini event carrying the whole response.
pub(crate) fn gemini(resp: &Value, facts: &mut StreamFacts) -> Option<Vec<Bytes>> {
    resp.get("candidates")?.as_array()?;
    facts.native_finished = true;
    let mut out = BytesMut::new();
    out.put_slice(b"data: ");
    out.put_slice(&serde_json::to_vec(resp).unwrap_or_default());
    out.put_slice(b"\r\n\r\n");
    Some(vec![out.freeze()])
}

/// The ConverseStream messages of a complete Converse response: `messageStart`, each content block
/// (`contentBlockStart` for a tool use, its deltas, `contentBlockStop`), `messageStop`, then
/// `metadata`.
pub(crate) fn converse(resp: &Value, facts: &mut StreamFacts) -> Option<Vec<Bytes>> {
    let content = resp.pointer("/output/message/content")?.as_array()?;
    let mut out = vec![converse_frame("messageStart", &json!({"role": "assistant"}))];
    for (i, block) in content.iter().enumerate() {
        let index = json!(i);
        let delta = |d: Value| {
            converse_frame(
                "contentBlockDelta",
                &json!({"contentBlockIndex": index, "delta": d}),
            )
        };
        if let Some(t) = block.get("text").and_then(Value::as_str) {
            facts.output_chars += u64::try_from(t.chars().count()).unwrap_or(0);
            out.push(delta(json!({"text": t})));
        } else if let Some(tu) = block.get("toolUse") {
            facts.tool_calls += 1;
            let start = json!({"toolUse": {"toolUseId": tu.get("toolUseId"), "name": tu.get("name")}});
            out.push(converse_frame(
                "contentBlockStart",
                &json!({"contentBlockIndex": index, "start": start}),
            ));
            let input = tu.get("input").map_or_else(|| "{}".to_owned(), Value::to_string);
            out.push(delta(json!({"toolUse": {"input": input}})));
        } else if let Some(r) = block.pointer("/reasoningContent/reasoningText") {
            if let Some(t) = r.get("text").and_then(Value::as_str) {
                out.push(delta(json!({"reasoningContent": {"text": t}})));
            }
            if let Some(s) = r.get("signature").and_then(Value::as_str) {
                out.push(delta(json!({"reasoningContent": {"signature": s}})));
            }
        } else {
            continue;
        }
        out.push(converse_frame(
            "contentBlockStop",
            &json!({"contentBlockIndex": index}),
        ));
    }
    let stop = resp.get("stopReason").cloned().unwrap_or(json!("end_turn"));
    if let Some(r) = stop.as_str() {
        facts.finish_reasons.push(r.to_owned());
    }
    let mut message_stop = json!({"stopReason": stop});
    if let (Some(extra), Some(o)) = (
        resp.get("additionalModelResponseFields"),
        message_stop.as_object_mut(),
    ) {
        o.insert("additionalModelResponseFields".into(), extra.clone());
    }
    out.push(converse_frame("messageStop", &message_stop));
    let usage = resp.get("usage").cloned().unwrap_or_else(|| json!({}));
    let metrics = resp
        .get("metrics")
        .cloned()
        .unwrap_or_else(|| json!({"latencyMs": 0}));
    out.push(converse_frame(
        "metadata",
        &json!({"usage": usage, "metrics": metrics}),
    ));
    facts.native_finished = true;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converse_messages_end_with_message_stop_then_metadata() {
        let resp = json!({"output": {"message": {"role": "assistant", "content": [
            {"text": "hi"}, {"toolUse": {"toolUseId": "t1", "name": "f", "input": {"a": 1}}}]}},
            "stopReason": "tool_use", "usage": {"inputTokens": 1, "outputTokens": 2, "totalTokens": 3}});
        let mut f = StreamFacts::default();
        let frames = converse(&resp, &mut f).unwrap();
        let mut d = antirion_wire::eventstream::Decoder::new(1 << 20);
        let mut msgs = Vec::new();
        for frame in &frames {
            d.push(frame, &mut msgs).unwrap();
        }
        let kinds: Vec<&str> = msgs.iter().filter_map(|m| m.header(":event-type")).collect();
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
        assert_eq!(f.tool_calls, 1);
        assert!(converse(&json!({"output": {}}), &mut f).is_none());
    }

    #[test]
    fn anthropic_grammar() {
        let msg = json!({"id": "msg_1", "type": "message", "role": "assistant", "model": "m",
                         "content": [{"type": "text", "text": "hi"}], "stop_reason": "end_turn",
                         "usage": {"input_tokens": 2, "output_tokens": 1}});
        let ev = anthropic(&msg, &mut StreamFacts::default()).unwrap();
        let names: Vec<String> = ev
            .iter()
            .map(|b| String::from_utf8_lossy(b).lines().next().unwrap().to_owned())
            .collect();
        assert_eq!(names.first().unwrap(), "event: message_start");
        assert_eq!(names.last().unwrap(), "event: message_stop");
        assert_eq!(ev.len(), 6);
    }

    #[test]
    fn responses_numbering() {
        let resp = json!({"id": "resp_1", "object": "response", "status": "completed", "model": "m",
                          "output": [{"type": "message", "id": "msg_1", "role": "assistant",
                                      "content": [{"type": "output_text", "text": "hi", "annotations": []}]}]});
        let mut f = StreamFacts::default();
        let ev = responses(&resp, &mut f).unwrap();
        assert_eq!(ev.len(), 9);
        assert_eq!(f.sequence, Some(8));
    }
}
