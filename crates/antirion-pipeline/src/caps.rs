//! The capabilities a request requires, derived from its body, and the deployments that have them.

use antirion_core::Stage;
use antirion_dialects::Endpoint;
use antirion_error::{GatewayError, Message, Reason};
use antirion_providers::DeploymentRt;
use serde_json::Value;

use crate::cell::operation_class;
use crate::parse::Parsed;
use crate::reasons;

/// One required capability: the flag, its reason, and the request field that created it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Need {
    pub(crate) flag: &'static str,
    reason: Reason,
    param: String,
    /// An operation class rather than a boolean flag.
    operation: Option<&'static str>,
    /// A count the flag (`max_n`) must reach.
    at_least: Option<u64>,
}

fn need(flag: &'static str, reason: Reason, param: &'static str) -> Need {
    Need {
        flag,
        reason,
        param: param.into(),
        operation: None,
        at_least: None,
    }
}

/// The first content part of `kind` in a message list, as a field path.
fn part_path(messages: Option<&Value>, kinds: &[&str]) -> Option<String> {
    for (i, m) in messages
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        for (j, p) in m
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            if p.get("type")
                .and_then(Value::as_str)
                .is_some_and(|t| kinds.contains(&t))
            {
                return Some(format!("messages[{i}].content[{j}]"));
            }
        }
    }
    None
}

fn need_at(flag: &'static str, reason: Reason, param: String) -> Need {
    Need {
        flag,
        reason,
        param,
        operation: None,
        at_least: None,
    }
}

/// The required set, in the normative row order.
pub(crate) fn required(endpoint: &Endpoint, parsed: &Parsed) -> Vec<Need> {
    let class = operation_class(endpoint);
    let mut out = vec![Need {
        flag: "operations",
        reason: reasons::CAP_OPERATION,
        param: "model".into(),
        operation: Some(class),
        at_least: None,
    }];
    let Parsed::Json { facts, .. } = parsed else {
        return out;
    };
    if facts.stream || endpoint.path_streams() {
        out.push(need("streaming", reasons::CAP_STREAMING, "stream"));
    }
    let anthropic = endpoint.id == "messages";
    if endpoint.id != "chat_completions" && !anthropic {
        return out;
    }
    let member = |name: &str| parsed.member(name);
    if member("tools").and_then(|t| t.as_array().map(|a| !a.is_empty())) == Some(true) {
        out.push(need("tools", reasons::CAP_TOOLS, "tools"));
    }
    let forced = match member("tool_choice").as_deref() {
        Some(Value::String(s)) => s == "required",
        Some(Value::Object(o)) if anthropic => {
            matches!(o.get("type").and_then(Value::as_str), Some("any" | "tool"))
        }
        Some(Value::Object(_)) => true,
        _ => false,
    };
    if forced {
        out.push(need(
            "tool_choice_forced",
            reasons::CAP_TOOL_CHOICE_FORCED,
            "tool_choice",
        ));
    }
    if member("parallel_tool_calls").as_deref() == Some(&Value::Bool(true)) {
        out.push(need(
            "parallel_tool_calls",
            reasons::CAP_PARALLEL_TOOL_CALLS,
            "parallel_tool_calls",
        ));
    }
    let format = member("response_format");
    let format_type = format
        .as_ref()
        .and_then(|f| f.get("type"))
        .and_then(Value::as_str);
    if format_type == Some("json_object") {
        out.push(need("json_mode", reasons::CAP_JSON_MODE, "response_format"));
    }
    if format_type == Some("json_schema") {
        out.push(need("json_schema", reasons::CAP_JSON_SCHEMA, "response_format"));
    }
    let messages = member("messages");
    let (image, audio, file): (&[&str], &[&str], &[&str]) = if anthropic {
        (&["image"], &[], &["document"])
    } else {
        (&["image_url"], &["input_audio"], &["file"])
    };
    if let Some(at) = part_path(messages.as_deref(), image) {
        out.push(need_at("vision", reasons::CAP_VISION, at));
    }
    if let Some(at) = part_path(messages.as_deref(), audio) {
        out.push(need_at("audio_input", reasons::CAP_AUDIO_INPUT, at));
    }
    if let Some(at) = part_path(messages.as_deref(), file) {
        out.push(need_at("pdf_input", reasons::CAP_PDF_INPUT, at));
    }
    let reasoning = ["reasoning_effort", "reasoning", "thinking"]
        .into_iter()
        .find(|n| member(n).is_some_and(|v| !v.is_null()));
    if let Some(field) = reasoning {
        out.push(need("reasoning", reasons::CAP_REASONING, field));
    }
    if let Some(n) = member("n").and_then(|n| n.as_u64()).filter(|n| *n > 1) {
        out.push(need("n_multiple", reasons::CAP_N_MULTIPLE, "n"));
        out.push(Need {
            at_least: Some(n),
            ..need("max_n", reasons::CAP_MAX_N, "n")
        });
    }
    if member("logprobs").as_deref() == Some(&Value::Bool(true)) {
        out.push(need("logprobs", reasons::CAP_LOGPROBS, "logprobs"));
    }
    out
}

/// The needs `deployment` does not satisfy, in order.
pub(crate) fn missing(deployment: &DeploymentRt, needs: &[Need]) -> Vec<Need> {
    needs
        .iter()
        .filter(|n| match (n.operation, n.at_least) {
            (Some(class), _) => !deployment.capabilities.operation(class),
            (None, Some(min)) => deployment
                .capabilities
                .get(n.flag)
                .0
                .as_u64()
                .is_some_and(|max| max < min),
            (None, None) => !deployment.capabilities.flag(n.flag),
        })
        .cloned()
        .collect()
}

/// The endpoint paths a deployment's operation classes serve.
fn served_paths(d: &DeploymentRt) -> Vec<&'static str> {
    [
        ("generate", "/v1/chat/completions"),
        ("embed", "/v1/embeddings"),
        ("image_generate", "/v1/images/generations"),
        ("speech", "/v1/audio/speech"),
        ("transcribe", "/v1/audio/transcriptions"),
        ("moderate", "/v1/moderations"),
        ("count_tokens", "/v1/messages/count_tokens"),
    ]
    .into_iter()
    .filter(|(class, _)| d.capabilities.operation(class))
    .map(|(_, path)| path)
    .collect()
}

/// The 400 for a request no deployment can serve, naming the first missing flag (and, for an
/// operation the model does not serve, the endpoints it does).
pub(crate) fn unsupported_on(d: &DeploymentRt, missing: &[Need]) -> GatewayError {
    let mut e = unsupported(missing);
    if missing.first().is_some_and(|n| n.operation.is_some()) {
        let paths = served_paths(d);
        if !paths.is_empty() {
            e.message = Message::built(format!(
                "This model does not serve this endpoint; it serves {}.",
                paths.join(", ")
            ));
        }
    }
    e
}

/// The 400 for a request no deployment can serve, naming the first missing flag.
pub(crate) fn unsupported(missing: &[Need]) -> GatewayError {
    let Some(first) = missing.first() else {
        return GatewayError::with_reason(
            reasons::CAP_OPERATION,
            Stage::Resolve,
            Message::Fixed("No deployment of this model can serve this request."),
        );
    };
    let flags: Vec<&str> = missing.iter().map(|n| n.flag).collect();
    GatewayError::with_reason(
        first.reason,
        Stage::Resolve,
        Message::built(format!(
            "No deployment of this model supports what this request needs: {}.",
            flags.join(", ")
        )),
    )
    .param(first.param.clone())
    .openai_code("unsupported_parameter")
}

/// Request sizes checked against a deployment's token limits.
#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct Sizes {
    /// The admission estimate of input tokens.
    pub(crate) input: u64,
    /// The requested maximum output, when the request sets one.
    pub(crate) output: Option<u64>,
}

/// The first token limit of `deployment` the request exceeds.
pub(crate) fn size_unmet(deployment: &DeploymentRt, s: Sizes) -> Option<&'static str> {
    let cap = |name: &str| deployment.capabilities.get(name).0.as_u64();
    if cap("max_input_tokens").is_some_and(|m| s.input > m) {
        return Some("max_input_tokens");
    }
    if cap("context_window").is_some_and(|m| s.input.saturating_add(s.output.unwrap_or(0)) > m) {
        return Some("context_window");
    }
    if output_unmet(deployment, s) {
        return Some("max_output_tokens");
    }
    None
}

/// Whether the requested maximum output exceeds the deployment's `max_output_tokens`. Routing
/// prefers deployments that can give it; when none can, the value is left to the upstream.
pub(crate) fn output_unmet(deployment: &DeploymentRt, s: Sizes) -> bool {
    let max = deployment.capabilities.get("max_output_tokens").0.as_u64();
    matches!((s.output, max), (Some(out), Some(m)) if out > m)
}

/// The reason that names an unmet capability.
pub(crate) fn reason_of(flag: &str) -> Reason {
    match flag {
        "streaming" => reasons::CAP_STREAMING,
        "tools" => reasons::CAP_TOOLS,
        "tool_choice_forced" => reasons::CAP_TOOL_CHOICE_FORCED,
        "json_mode" => reasons::CAP_JSON_MODE,
        "json_schema" => reasons::CAP_JSON_SCHEMA,
        "vision" => reasons::CAP_VISION,
        "audio_input" => reasons::CAP_AUDIO_INPUT,
        "pdf_input" => reasons::CAP_PDF_INPUT,
        "n_multiple" => reasons::CAP_N_MULTIPLE,
        "logprobs" => reasons::CAP_LOGPROBS,
        "max_n" => reasons::CAP_MAX_N,
        "parallel_tool_calls" => reasons::CAP_PARALLEL_TOOL_CALLS,
        "reasoning" => reasons::CAP_REASONING,
        "max_input_tokens" => reasons::CAP_MAX_INPUT_TOKENS,
        "max_output_tokens" => reasons::CAP_MAX_OUTPUT_TOKENS,
        "context_window" => reasons::CAP_CONTEXT_WINDOW,
        "dialects" => reasons::CAP_DIALECTS,
        _ => reasons::CAP_OPERATION,
    }
}

/// The 400 for a plan whose positions all lack a capability, naming the first one.
pub(crate) fn unsupported_flag(flag: &str) -> GatewayError {
    GatewayError::with_reason(
        reason_of(flag),
        Stage::Route,
        Message::built(format!(
            "No deployment of this model supports what this request needs: {flag}."
        )),
    )
    .openai_code("unsupported_parameter")
}
