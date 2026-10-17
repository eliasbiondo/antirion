//! Responses: an upstream response decoded into the IR, and the IR encoded for the client.

mod anthropic;
mod chat;
pub mod cite;
mod cohere;
mod converse;
mod decode;
mod gemini;
mod native;
mod responses;

use serde_json::Value;

pub(crate) use self::anthropic::{signature as anthropic_signature, usage as anthropic_usage_value};
pub(crate) use self::chat::{detail as chat_detail, tool_call as chat_tool_call, usage as chat_usage_value};
pub(crate) use self::cohere::{stop as cohere_stop, usage as cohere_usage};
pub(crate) use self::converse::{
    reasoning as converse_reasoning, stop_reason as converse_stop, usage as converse_usage_value,
};
pub use self::decode::decode;
pub(crate) use self::decode::unwrap_json_tool;
pub(crate) use self::decode::{chat_usage, responses_usage};
pub(crate) use self::gemini::{
    call_part as gemini_call_part, finish as gemini_finish, signature as gemini_signature,
    thought as gemini_thought, usage as gemini_usage_value,
};
pub(crate) use self::native::{anthropic_usage, converse_usage, gemini_call_id, gemini_usage};
pub(crate) use self::responses::{
    custom_call as responses_custom_call, custom_input as responses_custom_input, echo as responses_echo,
    function_call as responses_function_call, reasoning_item as responses_reasoning_item,
    status as responses_status, usage as responses_usage_value,
};
use super::encode::Restore;
use super::ir::{Client, Family};
use super::target::Upstream;
use super::warn::{Kind, Warnings};

/// The IR stop reason.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Stop {
    EndTurn,
    StopSequence,
    MaxTokens,
    ToolUse,
    ContentFilter,
    Refusal,
    PauseTurn,
    ContextWindow,
    Other,
}

impl Stop {
    #[must_use]
    pub fn from_chat(v: &str) -> Stop {
        match v {
            "stop" => Stop::EndTurn,
            "length" => Stop::MaxTokens,
            "tool_calls" | "function_call" => Stop::ToolUse,
            "content_filter" => Stop::ContentFilter,
            _ => Stop::Other,
        }
    }

    #[must_use]
    pub fn from_anthropic(v: &str) -> Stop {
        match v {
            "end_turn" => Stop::EndTurn,
            "stop_sequence" => Stop::StopSequence,
            "max_tokens" => Stop::MaxTokens,
            "tool_use" => Stop::ToolUse,
            "pause_turn" => Stop::PauseTurn,
            "refusal" => Stop::Refusal,
            "model_context_window_exceeded" => Stop::ContextWindow,
            _ => Stop::Other,
        }
    }

    /// A Gemini `finishReason`; `has_call` when the candidate has a function call.
    #[must_use]
    pub fn from_gemini(v: &str, has_call: bool) -> Stop {
        match v {
            "STOP" if has_call => Stop::ToolUse,
            "STOP" => Stop::EndTurn,
            "MAX_TOKENS" => Stop::MaxTokens,
            "SAFETY"
            | "RECITATION"
            | "BLOCKLIST"
            | "PROHIBITED_CONTENT"
            | "SPII"
            | "IMAGE_SAFETY"
            | "IMAGE_PROHIBITED_CONTENT"
            | "IMAGE_RECITATION"
            | "IMAGE_OTHER"
            | "NO_IMAGE"
            | "ESCALATION"
            | "PUP_LIMITED_DISABLED" => Stop::ContentFilter,
            _ => Stop::Other,
        }
    }

    #[must_use]
    pub fn from_converse(v: &str) -> Stop {
        match v {
            "end_turn" => Stop::EndTurn,
            "tool_use" => Stop::ToolUse,
            "max_tokens" => Stop::MaxTokens,
            "stop_sequence" => Stop::StopSequence,
            "guardrail_intervened" | "content_filtered" => Stop::ContentFilter,
            "model_context_window_exceeded" => Stop::ContextWindow,
            _ => Stop::Other,
        }
    }

    /// A Responses `status` and `incomplete_details.reason`; `has_call` when the output has a
    /// function call.
    #[must_use]
    pub fn from_responses(status: &str, reason: Option<&str>, has_call: bool) -> Stop {
        match (status, reason) {
            ("completed", _) if has_call => Stop::ToolUse,
            ("completed", _) => Stop::EndTurn,
            ("incomplete", Some("max_output_tokens")) => Stop::MaxTokens,
            ("incomplete", Some("content_filter")) => Stop::ContentFilter,
            _ => Stop::Other,
        }
    }

    #[must_use]
    pub fn chat(self) -> &'static str {
        match self {
            Stop::EndTurn | Stop::StopSequence | Stop::PauseTurn | Stop::Other => "stop",
            Stop::MaxTokens | Stop::ContextWindow => "length",
            Stop::ToolUse => "tool_calls",
            Stop::ContentFilter | Stop::Refusal => "content_filter",
        }
    }

    #[must_use]
    pub fn anthropic(self) -> &'static str {
        match self {
            Stop::EndTurn | Stop::Other => "end_turn",
            Stop::StopSequence => "stop_sequence",
            Stop::MaxTokens => "max_tokens",
            Stop::ToolUse => "tool_use",
            Stop::ContentFilter | Stop::Refusal => "refusal",
            Stop::PauseTurn => "pause_turn",
            Stop::ContextWindow => "model_context_window_exceeded",
        }
    }

    /// The Responses status and incomplete reason.
    #[must_use]
    pub fn responses(self) -> (&'static str, Option<&'static str>) {
        match self {
            Stop::MaxTokens | Stop::ContextWindow => ("incomplete", Some("max_output_tokens")),
            Stop::ContentFilter | Stop::Refusal => ("incomplete", Some("content_filter")),
            _ => ("completed", None),
        }
    }

    /// Whether encoding this stop for `client` is an approximation.
    #[must_use]
    pub fn approximated(self, client: Client) -> bool {
        match client {
            Client::Anthropic => self == Stop::Other,
            Client::Gemini => matches!(self, Stop::PauseTurn | Stop::ContextWindow),
            Client::Chat | Client::Responses | Client::Converse => {
                matches!(self, Stop::Other | Stop::PauseTurn)
            }
        }
    }
}

/// Normalised usage: cache counts and reasoning are subsets of input/output.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    /// Cache writes, when the upstream reports them.
    pub cache_write: Option<u64>,
    pub reasoning: Option<u64>,
}

/// One output element.
#[derive(Clone, Debug, PartialEq)]
pub enum Out {
    Text(String),
    Refusal(String),
    Call(OutCall),
    Reasoning(OutReasoning),
}

/// A tool call in a response.
#[derive(Clone, Debug, PartialEq)]
pub struct OutCall {
    pub id: String,
    pub name: String,
    /// The arguments text as the upstream gave it (objects serialised compactly).
    pub args: String,
    /// A Gemini thought signature on the call.
    pub signature: Option<String>,
}

/// A reasoning block in a response, with the signature or redacted payload of the family that
/// produced it.
#[derive(Clone, Debug, PartialEq)]
pub struct OutReasoning {
    pub text: String,
    pub signature: Option<String>,
    pub redacted: Option<String>,
    pub family: Family,
}

/// One candidate.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub blocks: Vec<Out>,
    pub stop: Stop,
    pub stop_sequence: Option<String>,
    /// Citations of spans of the candidate's text.
    pub cites: Vec<cite::Cite>,
    /// Web searches the upstream ran for the candidate.
    pub searches: Vec<cite::Search>,
}

/// A decoded upstream response.
#[derive(Clone, Debug, PartialEq)]
pub struct Response {
    pub id: Option<String>,
    /// The upstream's creation time, when it reports one.
    pub created: Option<i64>,
    pub candidates: Vec<Candidate>,
    pub usage: Option<Usage>,
}

/// Why an upstream response cannot become a client response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Bad {
    /// Not a response of the upstream's dialect.
    Unusable,
    /// A malformed function call or output with nothing usable (a retryable attempt failure).
    Malformed,
}

/// What decoding and encoding a response need to know.
#[derive(Clone, Debug)]
pub struct Cx<'a> {
    pub upstream: Upstream,
    pub client: Client,
    pub restore: &'a Restore,
    /// The provider the response came from (scope of `openai`-family payloads).
    pub provider: &'a str,
    /// The model name the client sees.
    pub model: &'a str,
    pub created: i64,
    /// An id for a response whose body has none.
    pub fallback_id: &'a str,
    /// The client's request body (Responses echo fields, `include`, `store`).
    pub request: &'a Value,
}

impl Cx<'_> {
    /// Whether a Responses client asked for encrypted reasoning.
    #[must_use]
    pub fn wants_encrypted(&self) -> bool {
        let include = self
            .request
            .get("include")
            .and_then(Value::as_array)
            .is_some_and(|a| {
                a.iter()
                    .any(|x| x.as_str() == Some("reasoning.encrypted_content"))
            });
        include || self.request.get("store") == Some(&Value::Bool(false))
    }
}

/// Translates an upstream response body for the client; response-side entries are added to `w`.
pub fn translate(body: &Value, cx: &Cx<'_>, w: &mut Warnings) -> Result<Value, Bad> {
    let resp = decode(body, cx, w)?;
    Ok(encode(&resp, cx, w))
}

/// Encodes a decoded response for the client.
#[must_use]
pub fn encode(resp: &Response, cx: &Cx<'_>, w: &mut Warnings) -> Value {
    if let Some(c) = resp.candidates.first()
        && c.stop.approximated(cx.client)
    {
        w.response(Kind::Approximated, "stop_reason");
    }
    match cx.client {
        Client::Chat => chat::encode(resp, cx, w),
        Client::Anthropic => anthropic::encode(resp, cx, w),
        Client::Responses => responses::encode(resp, cx),
        Client::Gemini => gemini::encode(resp, cx, w),
        Client::Converse => converse::encode(resp, cx, w),
    }
}

/// A client-facing id: the upstream id behind the dialect's prefix.
#[must_use]
pub fn prefixed(prefix: &str, upstream_id: Option<&str>) -> String {
    match upstream_id.filter(|i| !i.is_empty()) {
        Some(id) if id.starts_with(prefix) => id.to_owned(),
        Some(id) => {
            let clean: String = id
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            format!("{prefix}{clean}")
        }
        None => format!("{prefix}antirion"),
    }
}
