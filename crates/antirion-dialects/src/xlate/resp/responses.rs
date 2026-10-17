//! Client codec, response side: OpenAI Responses.

use serde_json::{Map, Value, json};

use super::{Cx, Out, OutReasoning, Response, Stop, Usage, prefixed};
use crate::xlate::envelope;
use crate::xlate::ir::Family;
use crate::xlate::util::parse_args;

/// Responses `usage`.
pub(crate) fn usage(u: &Usage) -> Value {
    let mut input = Map::new();
    input.insert("cached_tokens".into(), u.cache_read.into());
    if let Some(w) = u.cache_write {
        input.insert("cache_write_tokens".into(), w.into());
    }
    json!({
        "input_tokens": u.input,
        "input_tokens_details": input,
        "output_tokens": u.output,
        "output_tokens_details": {"reasoning_tokens": u.reasoning.unwrap_or(0)},
        "total_tokens": u.input + u.output,
    })
}

/// A reasoning output item; `encrypted` carries the payload in the envelope.
pub(crate) fn reasoning_item(id: &str, r: &OutReasoning, cx: &Cx<'_>) -> Value {
    let summary: Vec<Value> = if r.text.is_empty() {
        Vec::new()
    } else {
        vec![json!({"type": "summary_text", "text": r.text})]
    };
    let mut item = Map::new();
    item.insert("type".into(), "reasoning".into());
    item.insert("id".into(), id.into());
    item.insert("summary".into(), Value::Array(summary));
    if cx.wants_encrypted()
        && r.family != Family::None
        && let Some(payload) = r.signature.as_ref().or(r.redacted.as_ref())
    {
        let provider = (r.family == Family::OpenAi).then_some(cx.provider);
        item.insert(
            "encrypted_content".into(),
            envelope::wrap(r.family, provider, payload).into(),
        );
    }
    Value::Object(item)
}

pub(crate) fn function_call(id: &str, call_id: &str, name: &str, args: &str) -> Value {
    json!({"type": "function_call", "id": id, "call_id": call_id, "name": name, "arguments": args,
           "status": "completed"})
}

/// The free-form input of a custom tool's call: the `input` member of the function form's
/// arguments, or the arguments text when it has none.
pub(crate) fn custom_input(args: &str) -> String {
    match parse_args(args).as_ref().and_then(|a| a.get("input")) {
        Some(Value::String(s)) => s.clone(),
        Some(other) => crate::xlate::util::compact(other),
        None => args.to_owned(),
    }
}

/// A `custom_tool_call` item.
pub(crate) fn custom_call(id: &str, call_id: &str, name: &str, input: &str) -> Value {
    json!({"type": "custom_tool_call", "id": id, "call_id": call_id, "name": name, "input": input,
           "status": "completed"})
}

/// The request fields a Responses object echoes, with the API's defaults.
pub(crate) fn echo(out: &mut Map<String, Value>, request: &Value) {
    let get = |k: &str| request.get(k).cloned();
    out.insert("instructions".into(), get("instructions").unwrap_or(Value::Null));
    out.insert("tools".into(), get("tools").unwrap_or_else(|| json!([])));
    out.insert(
        "tool_choice".into(),
        get("tool_choice").unwrap_or_else(|| json!("auto")),
    );
    out.insert(
        "parallel_tool_calls".into(),
        get("parallel_tool_calls").unwrap_or(Value::Bool(true)),
    );
    out.insert("temperature".into(), get("temperature").unwrap_or(Value::Null));
    out.insert("top_p".into(), get("top_p").unwrap_or(Value::Null));
    out.insert(
        "max_output_tokens".into(),
        get("max_output_tokens").unwrap_or(Value::Null),
    );
    out.insert(
        "text".into(),
        get("text").unwrap_or_else(|| json!({"format": {"type": "text"}})),
    );
    out.insert(
        "reasoning".into(),
        get("reasoning").unwrap_or_else(|| json!({"effort": null, "summary": null})),
    );
    out.insert("store".into(), get("store").unwrap_or(Value::Bool(true)));
    out.insert("metadata".into(), get("metadata").unwrap_or_else(|| json!({})));
}

/// The status and incomplete details of a stop.
pub(crate) fn status(stop: Stop) -> (&'static str, Value) {
    let (status, reason) = stop.responses();
    (status, reason.map_or(Value::Null, |r| json!({"reason": r})))
}

pub(super) fn encode(resp: &Response, cx: &Cx<'_>) -> Value {
    let mut output = Vec::new();
    let mut parts: Vec<Value> = Vec::new();
    let stop = resp.candidates.first().map_or(Stop::EndTurn, |c| c.stop);
    let (cites, searches) = resp
        .candidates
        .first()
        .map(|c| (c.cites.as_slice(), c.searches.as_slice()))
        .unwrap_or_default();
    output.extend(searches.iter().map(super::cite::web_search_call));
    let mut offset = 0usize;
    let refusing = stop == Stop::Refusal;
    let mut n = 0usize;
    let flush = |parts: &mut Vec<Value>, output: &mut Vec<Value>, n: &mut usize| {
        if !parts.is_empty() {
            output.push(
                json!({"type": "message", "id": format!("msg_{n}"), "status": "completed",
                               "role": "assistant", "content": std::mem::take(parts)}),
            );
            *n += 1;
        }
    };
    for b in resp
        .candidates
        .first()
        .map(|c| c.blocks.as_slice())
        .unwrap_or_default()
    {
        match b {
            Out::Text(t) if refusing => {
                offset += t.chars().count();
                parts.push(json!({"type": "refusal", "refusal": t}));
            }
            Out::Text(t) => {
                let len = t.chars().count();
                let annotations: Vec<Value> = cites
                    .iter()
                    .filter(|c| c.start >= offset && c.start < offset + len)
                    .map(|c| super::cite::responses_annotation(c, offset))
                    .collect();
                offset += len;
                parts.push(json!({"type": "output_text", "text": t, "annotations": annotations}));
            }
            Out::Refusal(r) => parts.push(json!({"type": "refusal", "refusal": r})),
            Out::Reasoning(r) => {
                flush(&mut parts, &mut output, &mut n);
                output.push(reasoning_item(&format!("rs_{n}"), r, cx));
                n += 1;
            }
            Out::Call(c) if cx.restore.custom.contains(&c.name) => {
                flush(&mut parts, &mut output, &mut n);
                output.push(custom_call(
                    &format!("ctc_{n}"),
                    &c.id,
                    &c.name,
                    &custom_input(&c.args),
                ));
                n += 1;
            }
            Out::Call(c) => {
                flush(&mut parts, &mut output, &mut n);
                output.push(function_call(&format!("fc_{n}"), &c.id, &c.name, &c.args));
                n += 1;
            }
        }
    }
    flush(&mut parts, &mut output, &mut n);
    let (status, details) = status(stop);
    let mut out = Map::new();
    out.insert(
        "id".into(),
        prefixed("resp_", resp.id.as_deref().or(Some(cx.fallback_id))).into(),
    );
    out.insert("object".into(), "response".into());
    out.insert(
        "created_at".into(),
        resp.created.filter(|c| *c > 0).unwrap_or(cx.created).into(),
    );
    out.insert("status".into(), status.into());
    out.insert("incomplete_details".into(), details);
    out.insert("error".into(), Value::Null);
    out.insert("model".into(), cx.model.into());
    out.insert("output".into(), Value::Array(output));
    out.insert("usage".into(), resp.usage.as_ref().map_or(Value::Null, usage));
    echo(&mut out, cx.request);
    Value::Object(out)
}
