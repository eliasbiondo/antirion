//! Facts read off an inference request: token sizes, `Accept`, `alt=sse` and the dialect name.

use antirion_dialects::{Dialect, Endpoint};
use http::request::Parts;

use crate::parse::Parsed;

pub(super) fn input_tokens(parsed: &Parsed) -> u64 {
    match parsed {
        Parsed::Json { raw, .. } => u64::try_from(raw.len() / 4).unwrap_or(u64::MAX),
        Parsed::Multipart { .. } => 0,
    }
}

pub(super) fn max_output(endpoint: &Endpoint, parsed: &Parsed) -> Option<u64> {
    match parsed {
        Parsed::Json { .. } => parsed
            .doc()
            .and_then(|b| crate::account::requested_output(endpoint, b)),
        Parsed::Multipart { .. } => None,
    }
}

/// Whether some media range of `Accept` (with a non-zero quality) matches what the endpoint answers
/// with: JSON, and for a streaming request also its stream type. An absent `Accept` is satisfied.
pub(super) fn accept_satisfied(parts: &Parts, endpoint: &Endpoint, stream: bool) -> bool {
    let mut seen = false;
    let stream_type = if matches!(
        endpoint.dialect,
        Dialect::BedrockConverse | Dialect::BedrockInvoke
    ) {
        "application/vnd.amazon.eventstream"
    } else {
        "text/event-stream"
    };
    for value in parts.headers.get_all(http::header::ACCEPT) {
        let Ok(v) = value.to_str() else { continue };
        for range in v.split(',') {
            let mut fields = range.split(';');
            let media = fields.next().unwrap_or_default().trim().to_ascii_lowercase();
            if media.is_empty() {
                continue;
            }
            seen = true;
            let zero = fields.any(|p| {
                p.trim()
                    .strip_prefix("q=")
                    .and_then(|q| q.trim().parse::<f32>().ok())
                    .is_some_and(|q| q <= 0.0)
            });
            if zero {
                continue;
            }
            let matches = |target: &str| {
                let (tt, _) = target.split_once('/').unwrap_or((target, ""));
                media == "*/*" || media == target || media == format!("{tt}/*")
            };
            if matches("application/json") || (stream && matches(stream_type)) {
                return true;
            }
        }
    }
    !seen
}

/// Whether the query asks for server-sent events (`alt=sse`).
pub(super) fn alt_sse(parts: &Parts) -> bool {
    parts
        .uri
        .query()
        .unwrap_or_default()
        .split('&')
        .any(|p| p == "alt=sse")
}

/// The dialect name of an endpoint, for messages.
pub(super) fn dialect_name(endpoint: &Endpoint) -> &'static str {
    match endpoint.dialect {
        Dialect::AnthropicMessages => "anthropic_messages",
        Dialect::Gemini => "gemini",
        Dialect::BedrockConverse => "bedrock_converse",
        Dialect::BedrockInvoke => "bedrock_invoke",
        Dialect::OpenAiResponses => "openai_responses",
        _ => "openai",
    }
}
