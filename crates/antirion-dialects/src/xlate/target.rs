//! What a translation needs to know about the deployment it encodes for, and the translation policy.

use super::ir::Family;

/// An upstream dialect a request is translated to.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Upstream {
    Chat,
    Responses,
    Anthropic,
    Gemini,
    Converse,
    /// Cohere's v2 Chat.
    Cohere,
}

impl Upstream {
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Upstream::Chat => "openai_chat",
            Upstream::Responses => "openai_responses",
            Upstream::Anthropic => "anthropic_messages",
            Upstream::Gemini => "gemini",
            Upstream::Converse => "bedrock_converse",
            Upstream::Cohere => "cohere_chat",
        }
    }
}

/// How the deployment controls reasoning.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Control {
    Effort,
    Adaptive,
    Budget,
    Level,
    None,
}

impl Control {
    #[must_use]
    pub fn parse(s: &str) -> Control {
        match s {
            "effort" => Control::Effort,
            "adaptive" => Control::Adaptive,
            "budget" => Control::Budget,
            "level" => Control::Level,
            _ => Control::None,
        }
    }
}

/// A valid tool-name rule: a regular expression and a length limit.
#[derive(Clone, Debug)]
pub struct NameRule {
    pub pattern: String,
    pub max_len: usize,
}

/// The deployment a request is encoded for.
#[derive(Clone, Debug)]
pub struct Target {
    pub dialect: Upstream,
    /// The provider's name (the scope of `openai`-family payloads).
    pub provider: String,
    /// The upstream model id.
    pub model: String,
    /// An Anthropic model served through Converse.
    pub claude: bool,
    /// `openai_compatible` providers take `max_tokens`; OpenAI and Azure take `max_completion_tokens`.
    pub chat_max_field: &'static str,
    pub family: Family,
    pub names: NameRule,
    pub max_breakpoints: usize,
    pub accepts_cache_control: bool,
    pub native_structured: bool,
    pub temperature_range: Option<(f64, f64)>,
    pub exclusive_temperature_top_p: bool,
    pub sampling_params: bool,
    pub max_stop: Option<usize>,
    pub control: Control,
    pub efforts: Vec<String>,
    pub min_budget: Option<u64>,
    pub max_budget: Option<u64>,
    pub can_disable: bool,
    pub budget_ok: bool,
    pub thought_signatures: bool,
    pub image_formats: Vec<String>,
    pub image_url: bool,
    pub document_formats: Vec<String>,
    pub document_url: bool,
    pub audio_formats: Vec<String>,
    pub max_output_tokens: Option<u64>,
    pub default_max_tokens: u64,
    pub developer_role: bool,
    pub assistant_prefill: bool,
    /// The gateway may fetch remote media (`api.media_fetch.enabled`).
    pub media_fetch: bool,
    /// A Chat deployment honours `stream_options.include_usage`.
    pub stream_usage: bool,
}

impl Target {
    /// A target with the dialect's documented defaults, for tests and as a base.
    #[must_use]
    pub fn defaults(dialect: Upstream) -> Target {
        let anthropic = dialect == Upstream::Anthropic;
        Target {
            dialect,
            provider: "u".to_owned(),
            model: "m".to_owned(),
            claude: false,
            chat_max_field: "max_completion_tokens",
            family: match dialect {
                Upstream::Anthropic | Upstream::Converse => Family::Anthropic,
                Upstream::Gemini => Family::Gemini,
                Upstream::Cohere => Family::None,
                _ => Family::OpenAi,
            },
            names: default_names(dialect),
            max_breakpoints: 4,
            accepts_cache_control: false,
            native_structured: !matches!(dialect, Upstream::Anthropic | Upstream::Converse),
            temperature_range: Some(if anthropic { (0.0, 1.0) } else { (0.0, 2.0) }),
            exclusive_temperature_top_p: false,
            sampling_params: true,
            max_stop: None,
            control: if matches!(dialect, Upstream::Chat | Upstream::Responses) {
                Control::Effort
            } else {
                Control::None
            },
            efforts: vec!["low".into(), "medium".into(), "high".into()],
            min_budget: None,
            max_budget: None,
            can_disable: true,
            budget_ok: true,
            thought_signatures: false,
            image_formats: vec!["png".into(), "jpeg".into(), "gif".into(), "webp".into()],
            image_url: false,
            document_formats: vec!["pdf".into()],
            document_url: false,
            audio_formats: vec!["wav".into(), "mp3".into()],
            max_output_tokens: None,
            default_max_tokens: 4096,
            developer_role: true,
            assistant_prefill: true,
            media_fetch: true,
            stream_usage: true,
        }
    }

    /// The largest number of stop sequences the target takes.
    #[must_use]
    pub fn stop_limit(&self) -> Option<usize> {
        match self.dialect {
            Upstream::Chat => Some(4),
            Upstream::Gemini | Upstream::Cohere => Some(5),
            Upstream::Converse => Some(self.max_stop.unwrap_or(4)),
            Upstream::Anthropic | Upstream::Responses => self.max_stop,
        }
    }
}

/// The tool-name rule of a dialect when no layer sets one.
#[must_use]
pub fn default_names(dialect: Upstream) -> NameRule {
    match dialect {
        Upstream::Anthropic => NameRule {
            pattern: "^[a-zA-Z0-9_-]{1,128}$".into(),
            max_len: 128,
        },
        Upstream::Gemini => NameRule {
            pattern: "^[a-zA-Z_][a-zA-Z0-9_.:-]{0,63}$".into(),
            max_len: 64,
        },
        _ => NameRule {
            pattern: "^[a-zA-Z0-9_-]{1,64}$".into(),
            max_len: 64,
        },
    }
}

/// The reasoning budgets of `policies.translation.reasoning_budgets`, minimal to max.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Budgets(pub [u64; 6]);

impl Default for Budgets {
    fn default() -> Budgets {
        Budgets([1024, 2048, 8192, 16384, 32768, 32768])
    }
}

/// The effective translation policy of a request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    /// `on_lossy: reject` (or `x-antirion-translation: strict`).
    pub reject: bool,
    pub forward_unknown: bool,
    pub budgets: Budgets,
}
