//! The API of an upstream response in a given cell: how its body is checked, how its stream is read,
//! what its usage is, and how it becomes the client's response.

use antirion_dialects::{Dialect, Endpoint};
use antirion_providers::{DeploymentRt, ProviderType};
use serde_json::{Value, json};

use crate::cell::{Cell, Upstream};
use crate::relay::{ClientDialect, StreamDialect};
use crate::reqlog::{Convention, Usage};

/// The API an upstream response is in.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Api {
    Chat,
    Completions,
    Responses,
    Anthropic,
    AnthropicCount,
    Gemini,
    GeminiCount,
    /// OpenAI Responses `input_tokens`.
    ResponsesCount,
    Converse,
    Invoke,
    /// Cohere's v2 Chat.
    Cohere,
    Embeddings,
    Moderations,
    /// Another provider's embedding API.
    EmbedNative(antirion_dialects::embed::EmbedApi),
    /// A provider's rerank API.
    Rerank,
    /// Bodies relayed as they are (audio, images).
    Other,
}

/// The upstream API of `cell` for `endpoint`.
pub(crate) fn upstream_api(endpoint: &Endpoint, cell: Cell) -> Api {
    match cell {
        Cell::Embed(api) => Api::EmbedNative(api),
        Cell::Rerank(_) => Api::Rerank,
        Cell::Translate {
            upstream: Upstream::Gemini,
            ..
        } if endpoint.counts_tokens() => Api::GeminiCount,
        Cell::Translate {
            upstream: Upstream::Anthropic,
            ..
        } if endpoint.counts_tokens() => Api::AnthropicCount,
        Cell::Translate {
            upstream: Upstream::Responses,
            ..
        } if endpoint.counts_tokens() => Api::ResponsesCount,
        Cell::InvokeAsMessages => Api::Anthropic,
        Cell::Translate { upstream, .. } => match upstream {
            Upstream::Chat => Api::Chat,
            Upstream::Responses => Api::Responses,
            Upstream::Anthropic => Api::Anthropic,
            Upstream::Gemini => Api::Gemini,
            Upstream::Converse => Api::Converse,
            Upstream::Cohere => Api::Cohere,
        },
        Cell::Identity => match (endpoint.dialect, endpoint.id, endpoint.action) {
            (Dialect::AnthropicMessages, "count_tokens", _) => Api::AnthropicCount,
            (Dialect::AnthropicMessages, _, _) => Api::Anthropic,
            (Dialect::Gemini, _, Some("countTokens")) => Api::GeminiCount,
            (Dialect::Gemini, _, _) => Api::Gemini,
            (Dialect::BedrockConverse, _, _) => Api::Converse,
            (Dialect::BedrockInvoke, _, _) => Api::Invoke,
            (Dialect::OpenAiResponses, _, Some(_)) => Api::Other,
            (_, "chat_completions", _) => Api::Chat,
            (_, "completions", _) => Api::Completions,
            (_, "responses", _) => Api::Responses,
            (_, "embeddings", _) => Api::Embeddings,
            (_, "moderations", _) => Api::Moderations,
            _ => Api::Other,
        },
    }
}

/// The client dialect id of an endpoint (none for MCP and A2A).
pub(crate) fn client_dialect_id(endpoint: &Endpoint) -> Option<&'static str> {
    (endpoint.dialect != Dialect::Mcp).then(|| client_id(endpoint))
}

fn client_id(endpoint: &Endpoint) -> &'static str {
    match endpoint.dialect {
        Dialect::OpenAiChat => "openai_chat",
        Dialect::OpenAiResponses => "openai_responses",
        Dialect::OpenAiCompletions => "openai_completions",
        Dialect::OpenAiEmbeddings => "openai_embeddings",
        Dialect::OpenAiImages => "openai_images",
        Dialect::OpenAiAudio => "openai_audio",
        Dialect::OpenAiModerations => "openai_moderations",
        Dialect::AnthropicMessages => "anthropic_messages",
        Dialect::Gemini => "gemini",
        Dialect::BedrockConverse => "bedrock_converse",
        Dialect::BedrockInvoke | Dialect::Mcp => "bedrock_invoke",
        Dialect::OpenAiFiles => "openai_files",
        Dialect::OpenAiBatches => "openai_batches",
        Dialect::OpenAiRealtime => "openai_realtime",
        Dialect::Rerank => "rerank",
    }
}

/// The dialect ids of the client and of the upstream, for `x-antirion-client-dialect` and
/// `x-antirion-upstream-dialect`.
pub(crate) fn dialect_names(endpoint: &Endpoint, cell: Cell) -> (&'static str, &'static str) {
    let client = client_id(endpoint);
    let upstream = match upstream_api(endpoint, cell) {
        Api::Chat => "openai_chat",
        Api::Completions => "openai_completions",
        Api::Responses | Api::ResponsesCount => "openai_responses",
        Api::Anthropic | Api::AnthropicCount => "anthropic_messages",
        Api::Gemini | Api::GeminiCount => "gemini",
        Api::Converse => "bedrock_converse",
        Api::Invoke => "bedrock_invoke",
        Api::Cohere => "cohere_chat",
        Api::Embeddings => "openai_embeddings",
        Api::EmbedNative(api) => {
            use antirion_dialects::embed::EmbedApi;
            match api {
                EmbedApi::Gemini | EmbedApi::VertexPredict => "gemini",
                EmbedApi::Titan | EmbedApi::BedrockCohere => "bedrock_invoke",
                EmbedApi::Cohere => "cohere_embed",
            }
        }
        Api::Rerank => "rerank",
        Api::Moderations | Api::Other => client,
    };
    (client, upstream)
}

/// How the upstream's stream is read, when the API streams.
pub(crate) fn stream_dialect(api: Api, d: &DeploymentRt) -> Option<StreamDialect> {
    let bedrock = d.provider.kind == ProviderType::Bedrock;
    Some(match api {
        Api::Chat | Api::Completions => StreamDialect::Chat,
        Api::Responses => StreamDialect::Responses,
        Api::Anthropic if bedrock => StreamDialect::InvokeAnthropic,
        Api::Anthropic => StreamDialect::Anthropic,
        Api::Gemini => StreamDialect::Gemini,
        Api::Converse => StreamDialect::Converse,
        Api::Invoke => StreamDialect::Invoke,
        Api::Cohere => StreamDialect::Cohere,
        _ => return None,
    })
}

/// The dialect the client reads.
pub(crate) fn client_dialect(endpoint: &Endpoint, cell: Cell) -> ClientDialect {
    match cell {
        Cell::Translate { client, .. } => client,
        Cell::InvokeAsMessages => ClientDialect::Anthropic,
        Cell::Embed(_) | Cell::Rerank(_) => ClientDialect::Chat,
        Cell::Identity => match endpoint.dialect {
            Dialect::AnthropicMessages => ClientDialect::Anthropic,
            Dialect::Gemini => ClientDialect::Gemini,
            Dialect::BedrockConverse | Dialect::BedrockInvoke => ClientDialect::Bedrock,
            Dialect::OpenAiResponses => ClientDialect::Responses,
            _ => ClientDialect::Chat,
        },
    }
}

/// Whether a parsed body has what the API needs to answer.
pub(crate) fn usable(api: Api, v: &Value) -> bool {
    let nonempty_array = |name: &str| {
        v.get(name)
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty())
    };
    match api {
        Api::Chat | Api::Completions => nonempty_array("choices"),
        Api::Embeddings => v.get("data").is_some_and(Value::is_array),
        Api::Moderations => v.get("results").is_some_and(Value::is_array),
        Api::Anthropic => v.get("content").is_some_and(Value::is_array),
        Api::ResponsesCount => v.get("input_tokens").is_some_and(Value::is_u64),
        Api::AnthropicCount => {
            v.get("input_tokens").is_some_and(Value::is_u64)
                || v.get("inputTokens").is_some_and(Value::is_u64)
        }
        Api::Gemini => v.get("candidates").is_some_and(Value::is_array) || v.get("promptFeedback").is_some(),
        Api::GeminiCount => v.get("totalTokens").is_some_and(Value::is_u64),
        Api::Converse => v.get("output").is_some_and(Value::is_object),
        Api::Cohere => v.get("message").is_some_and(Value::is_object),
        Api::Responses | Api::Invoke | Api::Other | Api::EmbedNative(_) | Api::Rerank => v.is_object(),
    }
}

/// The usage a complete response reports.
pub(crate) fn usage_of(v: &Value) -> Usage {
    let num = |u: Option<&Value>, names: &[&str]| {
        u.and_then(|u| names.iter().find_map(|n| u.get(*n).and_then(Value::as_u64)))
    };
    if let Some(counts) = v
        .pointer("/usage/tokens")
        .or_else(|| v.pointer("/usage/billed_units"))
    {
        return cohere_usage(counts);
    }
    if let Some(u) = v.get("usageMetadata") {
        let output = match (
            num(Some(u), &["candidatesTokenCount"]),
            num(Some(u), &["thoughtsTokenCount"]),
        ) {
            (None, None) => None,
            (c, t) => Some(c.unwrap_or(0) + t.unwrap_or(0)),
        };
        return Usage {
            input: num(Some(u), &["promptTokenCount"]),
            output,
            ..Usage::default()
        }
        .with_subsets(Convention::Gemini, u);
    }
    let u = v.get("usage").filter(|u| !u.is_null());
    let cache = num(u, &["cache_read_input_tokens"]).unwrap_or(0)
        + num(u, &["cache_creation_input_tokens"]).unwrap_or(0);
    let usage = Usage {
        input: num(u, &["prompt_tokens", "input_tokens", "inputTokens"]).map(|i| i + cache),
        output: num(u, &["completion_tokens", "output_tokens", "outputTokens"]),
        total: num(u, &["total_tokens", "totalTokens"]),
        ..Usage::default()
    };
    let Some(u) = u else {
        return usage;
    };
    let conv = if u.get("prompt_tokens").is_some() {
        Convention::OpenAiChat
    } else if u.get("inputTokens").is_some() {
        Convention::BedrockConverse
    } else if v.get("type").and_then(Value::as_str) == Some("message")
        || u.get("cache_read_input_tokens").is_some()
        || u.get("cache_creation_input_tokens").is_some()
    {
        Convention::Anthropic
    } else {
        Convention::OpenAiResponses
    };
    usage.with_subsets(conv, u)
}

/// The usage of a Cohere response or `message-end` event, from its counted or billed tokens (which
/// may be written as decimals).
pub(crate) fn cohere_usage(counts: &Value) -> Usage {
    let n = |k: &str| counts.get(k).and_then(Value::as_f64).map(|f| f.max(0.0) as u64);
    Usage {
        input: n("input_tokens"),
        output: n("output_tokens"),
        ..Usage::default()
    }
}

/// Characters of generated output in a complete response (text and tool-call arguments), for an
/// estimate when it reports no usage.
pub(crate) fn output_chars(v: &Value) -> u64 {
    fn walk(v: &Value, key: Option<&str>) -> u64 {
        match v {
            Value::String(s) if matches!(key, Some("content" | "text" | "arguments" | "input")) => {
                s.chars().count() as u64
            }
            Value::Array(a) => a.iter().map(|x| walk(x, key)).sum(),
            Value::Object(m) => m.iter().map(|(k, x)| walk(x, Some(k.as_str()))).sum(),
            _ => 0,
        }
    }
    ["choices", "content", "output", "candidates"]
        .iter()
        .filter_map(|k| v.get(*k))
        .map(|x| walk(x, None))
        .sum()
}

/// Tool calls in a complete response of any dialect.
pub(crate) fn tool_calls(v: &Value) -> u64 {
    let count = |items: Option<&Value>, is_call: &dyn Fn(&Value) -> bool| -> u64 {
        items
            .and_then(Value::as_array)
            .map_or(0, |a| a.iter().filter(|i| is_call(i)).count() as u64)
    };
    let typed = |name: &'static str| move |i: &Value| i.get("type").and_then(Value::as_str) == Some(name);
    let chat: u64 = v
        .get("choices")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|c| count(c.pointer("/message/tool_calls"), &|_| true))
        .sum();
    let responses = count(v.get("output"), &|i| {
        i.get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| t.ends_with("_call") && !t.starts_with("reasoning"))
    });
    let anthropic = count(v.get("content"), &typed("tool_use"));
    let gemini: u64 = v
        .get("candidates")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|c| count(c.pointer("/content/parts"), &|p| p.get("functionCall").is_some()))
        .sum();
    let converse = count(v.pointer("/output/message/content"), &|b| {
        b.get("toolUse").is_some()
    });
    chat + responses + anthropic + gemini + converse
}

/// The service tier a response (or one of its stream events) names: OpenAI's top-level member,
/// a Responses event's response, or Anthropic's usage.
pub(crate) fn service_tier(v: &Value) -> Option<&str> {
    [
        "/service_tier",
        "/usage/service_tier",
        "/response/service_tier",
        "/message/usage/service_tier",
    ]
    .into_iter()
    .find_map(|p| v.pointer(p).and_then(Value::as_str))
}

/// What a complete answer shows the route's response predicates: its finish reasons as the
/// upstream wrote them (Anthropic's `stop_reason` and Converse's `stopReason` included), and whether
/// it carries assistant text or a tool call.
pub(crate) fn answer_facts(v: &Value) -> antirion_resilience::AnswerFacts {
    let mut finish_reasons = finish_reasons(v);
    finish_reasons.extend(
        ["stop_reason", "stopReason"]
            .into_iter()
            .filter_map(|k| v.get(k).and_then(Value::as_str))
            .map(str::to_owned),
    );
    antirion_resilience::AnswerFacts {
        finish_reasons,
        content: output_chars(v) > 0 || tool_calls(v) > 0,
    }
}

/// The finish reasons a complete response reports.
pub(crate) fn finish_reasons(v: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (list, field) in [("choices", "finish_reason"), ("candidates", "finishReason")] {
        for c in v.get(list).and_then(Value::as_array).into_iter().flatten() {
            if let Some(r) = c.get(field).and_then(Value::as_str) {
                out.push(r.to_owned());
            }
        }
    }
    for field in ["stop_reason", "stopReason"] {
        if let Some(r) = v.get(field).and_then(Value::as_str) {
            out.push(r.to_owned());
        }
    }
    out
}

/// The rewritten body of a complete upstream response in `cell`, when the client does not receive the
/// upstream's own body: token counts in the client's shape.
pub(crate) fn client_body(cell: Cell, api: Api, v: &Value) -> Option<Value> {
    let count = match (cell, api) {
        (Cell::Translate { .. }, Api::GeminiCount) => v.get("totalTokens"),
        (Cell::Translate { .. }, Api::AnthropicCount | Api::ResponsesCount) => v.get("input_tokens"),
        (Cell::Identity, Api::AnthropicCount) => v.get("inputTokens"),
        _ => None,
    };
    let n = count.and_then(Value::as_u64)?;
    Some(match cell {
        Cell::Translate {
            client: ClientDialect::Responses,
            ..
        } => responses_count(n),
        _ => json!({"input_tokens": n}),
    })
}

/// A Responses `input_tokens` answer.
pub(crate) fn responses_count(n: u64) -> Value {
    json!({"object": "response.input_tokens", "input_tokens": n})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_calls_of_every_dialect() {
        assert_eq!(
            tool_calls(&json!({"choices": [{"message": {"tool_calls": [{}, {}]}}]})),
            2
        );
        assert_eq!(
            tool_calls(&json!({"output": [{"type": "function_call"}, {"type": "message"}]})),
            1
        );
        assert_eq!(
            tool_calls(&json!({"content": [{"type": "tool_use"}, {"type": "text"}]})),
            1
        );
        assert_eq!(
            tool_calls(
                &json!({"candidates": [{"content": {"parts": [{"functionCall": {}}, {"text": "x"}]}}]})
            ),
            1
        );
        assert_eq!(
            tool_calls(&json!({"output": {"message": {"content": [{"toolUse": {}}]}}})),
            1
        );
        assert_eq!(
            tool_calls(&json!({"choices": [{"message": {"content": "hi"}}]})),
            0
        );
    }

    #[test]
    fn usage_shapes() {
        let u = usage_of(&json!({"usageMetadata": {"promptTokenCount": 2, "candidatesTokenCount": 3}}));
        assert_eq!((u.input, u.output), (Some(2), Some(3)));
        let u = usage_of(&json!({"usage": {"inputTokens": 1, "outputTokens": 4}}));
        assert_eq!((u.input, u.output), (Some(1), Some(4)));
        let u = usage_of(
            &json!({"usage": {"input_tokens": 1, "cache_read_input_tokens": 2, "output_tokens": 4}}),
        );
        assert_eq!(u.input, Some(3));
        assert_eq!(finish_reasons(&json!({"stopReason": "end_turn"})), ["end_turn"]);
    }

    #[test]
    fn bedrock_count_tokens_become_anthropic() {
        let body = client_body(Cell::Identity, Api::AnthropicCount, &json!({"inputTokens": 5}));
        assert_eq!(body, Some(json!({"input_tokens": 5})));
    }
}
