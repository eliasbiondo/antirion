//! The internal representation of a translated request: what every client codec decodes into and
//! every upstream codec encodes from. Each element keeps the client field it came from, so that a
//! warning or a refusal names it.

use serde_json::Value;

use super::warn::Src;

/// The client dialect a request was decoded from.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Client {
    Chat,
    Responses,
    Anthropic,
    /// Gemini `generateContent` and `streamGenerateContent`.
    Gemini,
    /// Bedrock Converse and ConverseStream.
    Converse,
}

/// A reasoning family (replay scope of signatures).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Family {
    Anthropic,
    OpenAi,
    Gemini,
    None,
}

impl Family {
    #[must_use]
    pub fn parse(s: &str) -> Family {
        match s {
            "anthropic" => Family::Anthropic,
            "openai" => Family::OpenAi,
            "gemini" => Family::Gemini,
            _ => Family::None,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Family::Anthropic => "anthropic",
            Family::OpenAi => "openai",
            Family::Gemini => "gemini",
            Family::None => "none",
        }
    }
}

/// A `cache_control` marker.
#[derive(Clone, Debug, PartialEq)]
pub struct Cache {
    pub value: Value,
    pub src: Src,
}

impl Cache {
    /// Whether the marker asks for the one-hour lifetime.
    #[must_use]
    pub fn one_hour(&self) -> bool {
        self.value.get("ttl").and_then(Value::as_str) == Some("1h")
    }
}

/// Where image or document bytes come from.
#[derive(Clone, Debug, PartialEq)]
pub enum Media {
    Base64 {
        mime: String,
        data: String,
    },
    Url(String),
    /// A provider file id, and the client dialect whose provider issued it.
    FileId(String),
    /// Anthropic plain-text document source.
    Text {
        mime: String,
        text: String,
    },
}

/// One content block.
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Text {
        text: String,
        cache: Option<Cache>,
        src: Src,
    },
    Image {
        media: Media,
        detail: Option<(String, Src)>,
        cache: Option<Cache>,
        /// The part, and the field holding its URL or data.
        src: Src,
        url_src: Src,
    },
    Document {
        media: Media,
        name: Option<(String, Src)>,
        cache: Option<Cache>,
        src: Src,
        url_src: Src,
    },
    Audio {
        data: String,
        format: String,
        src: Src,
    },
    ToolCall(Call),
    ToolResult(ToolResult),
    Reasoning(Reasoning),
    Refusal {
        text: String,
    },
}

/// A tool call in history.
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub id: String,
    pub name: String,
    /// The arguments as a JSON object (its member order is the client's), or `None` when the
    /// client's arguments were not a JSON object.
    pub args: Option<Value>,
    /// The arguments text exactly as an OpenAI client sent it.
    pub raw: Option<String>,
    /// A Gemini thought signature carried on the call (raw, Gemini family).
    pub signature: Option<String>,
    pub cache: Option<Cache>,
    pub src: Src,
    pub args_src: Src,
}

/// A tool result in history.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolResult {
    pub id: String,
    pub content: Vec<Block>,
    pub is_error: Option<(bool, Src)>,
    pub cache: Option<Cache>,
    pub src: Src,
    /// The field holding the result's content.
    pub content_src: Src,
    /// The field holding the id of the call it answers.
    pub id_src: Src,
}

/// A reasoning block in history (or in a response).
#[derive(Clone, Debug, PartialEq)]
pub struct Reasoning {
    pub text: String,
    /// A signature or an encrypted payload, unwrapped from its envelope.
    pub signature: Option<String>,
    /// Redacted thinking data.
    pub redacted: Option<String>,
    pub family: Family,
    /// The provider an `openai`-family payload belongs to, when known.
    pub provider: Option<String>,
    /// Re-inserted from the reasoning replay store, not sent by the client: never reported.
    pub restored: bool,
    pub src: Src,
}

/// A message role.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Role {
    System,
    Developer,
    User,
    Assistant,
    /// Tool results (Chat `tool` messages, Responses `function_call_output`, Anthropic `tool_result`).
    Tool,
}

/// One turn of the conversation after the leading system block.
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    pub role: Role,
    pub blocks: Vec<Block>,
    /// A message-level `cache_control` marker (Chat extension).
    pub cache: Option<Cache>,
    pub src: Src,
    pub role_src: Src,
}

/// One block of the leading system prompt.
#[derive(Clone, Debug, PartialEq)]
pub struct SysBlock {
    pub text: String,
    pub cache: Option<Cache>,
    pub developer: bool,
    pub src: Src,
}

/// A function tool definition.
#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    pub name: String,
    pub description: Option<String>,
    pub parameters: Option<Value>,
    pub strict: Option<(bool, Src)>,
    pub cache: Option<Cache>,
    /// A Responses `custom` tool (free-form string input), sent elsewhere as a function tool with
    /// one string parameter `input`; with the field of a grammar the target cannot enforce.
    pub custom: Option<Option<Src>>,
    pub src: Src,
    pub params_src: Src,
}

/// A hosted, server or provider-defined tool, never translated.
#[derive(Clone, Debug, PartialEq)]
pub struct Hosted {
    pub kind: String,
    /// Anthropic-defined client tools, `container`, `mcp_servers`: refused rather than dropped.
    pub unsupported: bool,
    pub src: Src,
}

/// An approximate user location for web search.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Location {
    pub city: Option<String>,
    pub region: Option<String>,
    pub country: Option<String>,
    pub timezone: Option<String>,
}

/// Web search asked for by the client: Chat `web_search_options`, a Responses `web_search` tool, an
/// Anthropic `web_search_*` server tool or Gemini `googleSearch`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebSearch {
    /// The client field that asked for it.
    pub src: Src,
    pub max_uses: Option<u64>,
    pub allowed_domains: Vec<String>,
    pub blocked_domains: Vec<String>,
    pub location: Option<Location>,
    /// OpenAI `search_context_size`.
    pub context_size: Option<String>,
}

impl WebSearch {
    /// Web search with no options, asked for at `src`.
    #[must_use]
    pub fn at(src: Src) -> WebSearch {
        WebSearch {
            src,
            max_uses: None,
            allowed_domains: Vec::new(),
            blocked_domains: Vec::new(),
            location: None,
            context_size: None,
        }
    }
}

/// `tool_choice`.
#[derive(Clone, Debug, PartialEq)]
pub enum Choice {
    Auto,
    None,
    Required,
    Named(String),
    /// Allowed tools: the mode (`auto` or `required`) and the names.
    Allowed {
        required: bool,
        names: Vec<String>,
    },
}

/// Structured output.
#[derive(Clone, Debug, PartialEq)]
pub enum Format {
    JsonSchema {
        name: Option<String>,
        schema: Value,
        strict: Option<bool>,
    },
    JsonObject,
}

/// The reasoning mode a request asks for.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Unspecified,
    Off,
    On,
    Adaptive,
}

/// Reasoning effort, in increasing order.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Effort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    pub const ALL: [Effort; 7] = [
        Effort::None,
        Effort::Minimal,
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::Xhigh,
        Effort::Max,
    ];

    #[must_use]
    pub fn parse(s: &str) -> Option<Effort> {
        Effort::ALL.into_iter().find(|e| e.as_str() == s)
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::None => "none",
            Effort::Minimal => "minimal",
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
            Effort::Max => "max",
        }
    }
}

/// The reasoning control of a request.
#[derive(Clone, Debug, PartialEq)]
pub struct Reason {
    pub mode: Mode,
    pub effort: Option<Effort>,
    pub budget: Option<u64>,
    pub include_thoughts: Option<bool>,
    /// The include came from a client field that asks for summaries (not Anthropic `thinking`).
    pub summaries: bool,
    /// The client field that carried the control.
    pub src: Src,
}

impl Default for Reason {
    fn default() -> Reason {
        Reason {
            mode: Mode::Unspecified,
            effort: None,
            budget: None,
            include_thoughts: None,
            summaries: false,
            src: Src::absent("reasoning"),
        }
    }
}

/// A numeric generation control and its field.
#[derive(Clone, Debug, PartialEq)]
pub struct Num {
    pub value: Value,
    pub src: Src,
}

impl Num {
    #[must_use]
    pub fn f64(&self) -> Option<f64> {
        self.value.as_f64()
    }
}

/// A field passed between OpenAI dialects and inert elsewhere (`metadata`, `store`, ...).
#[derive(Clone, Debug, PartialEq)]
pub struct Inert {
    /// The name in the client's dialect (`verbosity` or `text.verbosity`, `metadata`).
    pub name: &'static str,
    pub value: Value,
    pub src: Src,
}

/// A field the client codec does not recognise.
#[derive(Clone, Debug, PartialEq)]
pub struct Unknown {
    pub name: String,
    pub value: Value,
    pub top_level: bool,
    pub src: Src,
}

/// A feature that no target of this dialect can express (class U for every translated cell).
#[derive(Clone, Debug, PartialEq)]
pub struct Unsupported {
    pub src: Src,
    pub message: String,
}

/// A decoded client request.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub client: Client,
    pub system: Vec<SysBlock>,
    pub turns: Vec<Turn>,
    pub messages_src: Src,
    pub tools: Vec<Tool>,
    pub hosted: Vec<Hosted>,
    pub tools_src: Src,
    pub choice: Option<(Choice, Src)>,
    pub parallel: Option<(bool, Src)>,
    pub max_tokens: Option<(u64, Src)>,
    pub temperature: Option<Num>,
    pub top_p: Option<Num>,
    pub top_k: Option<Num>,
    pub frequency_penalty: Option<Num>,
    pub presence_penalty: Option<Num>,
    pub seed: Option<Num>,
    pub logit_bias: Option<Num>,
    pub logprobs: Option<Num>,
    pub top_logprobs: Option<Num>,
    pub n: Option<(u64, Src)>,
    pub stop: Option<(Vec<String>, Src)>,
    pub format: Option<(Format, Src)>,
    pub reason: Reason,
    pub end_user: Option<(String, Src)>,
    pub inert: Vec<Inert>,
    /// Anthropic top-level automatic caching marker.
    pub auto_cache: Option<Cache>,
    pub web_search: Option<WebSearch>,
    pub unsupported: Vec<Unsupported>,
    pub unknown: Vec<Unknown>,
    /// Fields ignored for every target (Chat message `name`, Responses `include`, ...).
    pub ignored: Vec<Src>,
    pub stream: bool,
    /// The client asked for encrypted reasoning in Responses output.
    pub include_encrypted: bool,
    /// The client sent `store: false` (Responses) or `store: true` (Chat).
    pub store: Option<bool>,
    /// The id field of the first tool result that answers no tool call of an earlier assistant turn.
    pub unpaired: Option<Src>,
}

impl Request {
    #[must_use]
    pub fn new(client: Client) -> Request {
        Request {
            client,
            system: Vec::new(),
            turns: Vec::new(),
            messages_src: Src::absent("messages"),
            tools: Vec::new(),
            hosted: Vec::new(),
            tools_src: Src::absent("tools"),
            choice: None,
            parallel: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            frequency_penalty: None,
            presence_penalty: None,
            seed: None,
            logit_bias: None,
            logprobs: None,
            top_logprobs: None,
            n: None,
            stop: None,
            format: None,
            reason: Reason::default(),
            end_user: None,
            inert: Vec::new(),
            auto_cache: None,
            web_search: None,
            unsupported: Vec::new(),
            unknown: Vec::new(),
            ignored: Vec::new(),
            stream: false,
            include_encrypted: false,
            store: None,
            unpaired: None,
        }
    }

    /// The id field of the first tool result whose id matches no tool call of an earlier assistant
    /// turn.
    #[must_use]
    pub fn first_unpaired(&self) -> Option<&Src> {
        let mut seen: Vec<&str> = Vec::new();
        for t in &self.turns {
            for b in &t.blocks {
                match b {
                    Block::ToolCall(c) if t.role == Role::Assistant => seen.push(&c.id),
                    Block::ToolResult(r) if !seen.contains(&r.id.as_str()) => return Some(&r.id_src),
                    _ => {}
                }
            }
        }
        None
    }

    /// Every tool call in history, in order.
    pub fn calls(&self) -> impl Iterator<Item = &Call> {
        self.turns.iter().flat_map(|t| {
            t.blocks.iter().filter_map(|b| match b {
                Block::ToolCall(c) => Some(c),
                _ => None,
            })
        })
    }

    /// The inert field `name`, if the client sent it.
    #[must_use]
    pub fn inert(&self, name: &str) -> Option<&Inert> {
        self.inert.iter().find(|i| i.name == name)
    }
}
