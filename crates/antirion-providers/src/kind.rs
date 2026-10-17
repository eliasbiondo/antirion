//! The provider types and their catalog entries.

use antirion_dialects::Dialect;

/// A hosted OpenAI-compatible vendor: an `openai_compatible` provider with a default base URL and
/// capability defaults of its own.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Preset {
    Groq,
    Together,
    Fireworks,
    DeepSeek,
    Xai,
    Cerebras,
    DeepInfra,
    Perplexity,
}

impl Preset {
    const ALL: [Preset; 8] = [
        Preset::Groq,
        Preset::Together,
        Preset::Fireworks,
        Preset::DeepSeek,
        Preset::Xai,
        Preset::Cerebras,
        Preset::DeepInfra,
        Preset::Perplexity,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Preset::Groq => "groq",
            Preset::Together => "together",
            Preset::Fireworks => "fireworks",
            Preset::DeepSeek => "deepseek",
            Preset::Xai => "xai",
            Preset::Cerebras => "cerebras",
            Preset::DeepInfra => "deepinfra",
            Preset::Perplexity => "perplexity",
        }
    }

    fn base_url(self) -> &'static str {
        match self {
            Preset::Groq => "https://api.groq.com/openai/v1",
            Preset::Together => "https://api.together.xyz/v1",
            Preset::Fireworks => "https://api.fireworks.ai/inference/v1",
            Preset::DeepSeek => "https://api.deepseek.com/v1",
            Preset::Xai => "https://api.x.ai/v1",
            Preset::Cerebras => "https://api.cerebras.ai/v1",
            Preset::DeepInfra => "https://api.deepinfra.com/v1/openai",
            Preset::Perplexity => "https://api.perplexity.ai",
        }
    }

    /// Whether the vendor serves OpenAI embeddings.
    pub(crate) fn embeds(self) -> bool {
        matches!(self, Preset::Together | Preset::Fireworks | Preset::DeepInfra)
    }

    /// The vendor's capability defaults that differ from `openai_compatible`'s.
    pub(crate) fn default_flag(self, name: &str) -> Option<bool> {
        let off = match self {
            Preset::Perplexity => &[
                "tools",
                "tool_choice_forced",
                "parallel_tool_calls",
                "n_multiple",
                "logprobs",
            ][..],
            Preset::DeepSeek => &["vision", "n_multiple"][..],
            Preset::Cerebras => &["vision", "n_multiple", "logprobs"][..],
            Preset::Groq => &["n_multiple"][..],
            Preset::Xai => &["n_multiple"][..],
            Preset::Together | Preset::Fireworks | Preset::DeepInfra => &[][..],
        };
        off.contains(&name).then_some(false)
    }
}

/// The provider types of the schema this build implements.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ProviderType {
    OpenAi,
    OpenAiCompatible,
    AzureOpenAi,
    Anthropic,
    Bedrock,
    Vertex,
    Gemini,
    /// Any server speaking Anthropic Messages at its base URL, with `x-api-key`.
    AnthropicCompatible,
    /// Mistral's own API, which speaks Chat Completions and Embeddings.
    Mistral,
    /// Cohere's v2 Chat API.
    Cohere,
    Preset(Preset),
}

/// Known types this build does not implement, which validation refuses as not supported.
const UNSUPPORTED: &[&str] = &[
    "huggingface",
    "replicate",
    "databricks",
    "sagemaker",
    "watsonx",
    "ollama_native",
    "triton",
];

impl ProviderType {
    pub(crate) fn parse(s: &str) -> Option<ProviderType> {
        Some(match s {
            "openai" => ProviderType::OpenAi,
            "openai_compatible" => ProviderType::OpenAiCompatible,
            "azure_openai" => ProviderType::AzureOpenAi,
            "anthropic" => ProviderType::Anthropic,
            "bedrock" => ProviderType::Bedrock,
            "vertex" => ProviderType::Vertex,
            "gemini" => ProviderType::Gemini,
            "anthropic_compatible" => ProviderType::AnthropicCompatible,
            "mistral" => ProviderType::Mistral,
            "cohere" => ProviderType::Cohere,
            other => ProviderType::Preset(Preset::ALL.into_iter().find(|p| p.as_str() == other)?),
        })
    }

    /// Whether `s` names a known type this build does not implement.
    pub(crate) fn unsupported(s: &str) -> bool {
        UNSUPPORTED.contains(&s)
    }

    /// The types that speak OpenAI Chat Completions natively.
    #[must_use]
    pub fn openai_family(self) -> bool {
        matches!(
            self,
            ProviderType::OpenAi
                | ProviderType::OpenAiCompatible
                | ProviderType::AzureOpenAi
                | ProviderType::Mistral
                | ProviderType::Preset(_)
        )
    }

    /// The types that serve other vendors' models under those models' own ids, so the catalog
    /// entry of a model id counts whichever type it names.
    #[must_use]
    pub fn generic(self) -> bool {
        matches!(
            self,
            ProviderType::OpenAiCompatible | ProviderType::AnthropicCompatible | ProviderType::Preset(_)
        )
    }

    /// The types that speak Anthropic Messages at the Anthropic API's paths.
    #[must_use]
    pub fn anthropic_api(self) -> bool {
        matches!(self, ProviderType::Anthropic | ProviderType::AnthropicCompatible)
    }

    /// The types configured like `openai_compatible`: `auth.header` and `auth.prefix` apply, and
    /// Chat Completions takes `max_tokens`.
    #[must_use]
    pub fn compatible(self) -> bool {
        matches!(
            self,
            ProviderType::OpenAiCompatible | ProviderType::Mistral | ProviderType::Preset(_)
        )
    }

    /// Whether this is one of the OpenAI-family types and it speaks an OpenAI client dialect
    /// natively, so a request can pass through.
    #[must_use]
    pub fn serves(self, dialect: Dialect) -> bool {
        let openai_dialect = matches!(
            dialect,
            Dialect::OpenAiChat
                | Dialect::OpenAiResponses
                | Dialect::OpenAiCompletions
                | Dialect::OpenAiEmbeddings
                | Dialect::OpenAiImages
                | Dialect::OpenAiAudio
                | Dialect::OpenAiModerations
        );
        self.openai_family() && openai_dialect
    }

    pub(crate) fn default_base_url(self) -> Option<&'static str> {
        match self {
            ProviderType::OpenAi => Some("https://api.openai.com/v1"),
            ProviderType::Anthropic => Some("https://api.anthropic.com"),
            ProviderType::Gemini => Some("https://generativelanguage.googleapis.com"),
            ProviderType::Mistral => Some("https://api.mistral.ai/v1"),
            ProviderType::Cohere => Some("https://api.cohere.com"),
            ProviderType::Preset(p) => Some(p.base_url()),
            _ => None,
        }
    }

    /// Every provider type, in catalog order.
    pub const ALL: [ProviderType; 18] = [
        ProviderType::OpenAi,
        ProviderType::AzureOpenAi,
        ProviderType::Anthropic,
        ProviderType::Bedrock,
        ProviderType::Vertex,
        ProviderType::Gemini,
        ProviderType::OpenAiCompatible,
        ProviderType::AnthropicCompatible,
        ProviderType::Mistral,
        ProviderType::Cohere,
        ProviderType::Preset(Preset::Groq),
        ProviderType::Preset(Preset::Together),
        ProviderType::Preset(Preset::Fireworks),
        ProviderType::Preset(Preset::DeepSeek),
        ProviderType::Preset(Preset::Xai),
        ProviderType::Preset(Preset::Cerebras),
        ProviderType::Preset(Preset::DeepInfra),
        ProviderType::Preset(Preset::Perplexity),
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderType::OpenAi => "openai",
            ProviderType::OpenAiCompatible => "openai_compatible",
            ProviderType::AzureOpenAi => "azure_openai",
            ProviderType::Anthropic => "anthropic",
            ProviderType::Bedrock => "bedrock",
            ProviderType::Vertex => "vertex",
            ProviderType::Gemini => "gemini",
            ProviderType::AnthropicCompatible => "anthropic_compatible",
            ProviderType::Mistral => "mistral",
            ProviderType::Cohere => "cohere",
            ProviderType::Preset(p) => p.as_str(),
        }
    }

    /// Whether the type is one of the core types the provider-type catalog lists first.
    #[must_use]
    pub fn core(self) -> bool {
        matches!(
            self,
            ProviderType::OpenAi
                | ProviderType::OpenAiCompatible
                | ProviderType::AzureOpenAi
                | ProviderType::Anthropic
                | ProviderType::Bedrock
                | ProviderType::Vertex
                | ProviderType::Gemini
        )
    }

    /// The catalog entry of the type: its documented default base URL (templated for the cloud
    /// types), the upstream dialects it speaks and the auth methods this build supports.
    #[must_use]
    pub fn catalog(self) -> TypeCatalog {
        let openai_family: &'static [&'static str] = &[
            "openai_chat",
            "openai_responses",
            "openai_completions",
            "openai_embeddings",
        ];
        let compatible: &'static [&'static str] = &["openai_chat", "openai_completions", "openai_embeddings"];
        let (base, dialects, auth): (
            Option<&'static str>,
            &'static [&'static str],
            &'static [&'static str],
        ) = match self {
            ProviderType::OpenAi => (self.default_base_url(), openai_family, &["api_key"]),
            ProviderType::AzureOpenAi => (None, openai_family, &["api_key", "azure_ad"]),
            ProviderType::Anthropic => (self.default_base_url(), &["anthropic_messages"], &["api_key"]),
            ProviderType::Bedrock => (
                Some("https://bedrock-runtime.{region}.amazonaws.com"),
                &["bedrock_converse", "anthropic_messages"],
                &["aws_sigv4", "api_key"],
            ),
            ProviderType::Vertex => (
                Some("https://{location}-aiplatform.googleapis.com"),
                &["gemini", "anthropic_messages"],
                &["gcp", "api_key"],
            ),
            ProviderType::Gemini => (self.default_base_url(), &["gemini"], &["api_key"]),
            ProviderType::OpenAiCompatible => (None, compatible, &["api_key", "none"]),
            ProviderType::AnthropicCompatible => (None, &["anthropic_messages"], &["api_key"]),
            ProviderType::Mistral => (
                self.default_base_url(),
                &["openai_chat", "openai_embeddings"],
                &["api_key"],
            ),
            ProviderType::Cohere => (self.default_base_url(), &["cohere_chat"], &["api_key"]),
            ProviderType::Preset(p) if p.embeds() => (
                self.default_base_url(),
                &["openai_chat", "openai_embeddings"],
                &["api_key"],
            ),
            ProviderType::Preset(_) => (self.default_base_url(), &["openai_chat"], &["api_key"]),
        };
        TypeCatalog {
            default_base_url: base,
            dialects,
            auth_methods: auth,
        }
    }
}

/// A provider type's catalog entry.
#[derive(Copy, Clone, Debug)]
pub struct TypeCatalog {
    pub default_base_url: Option<&'static str>,
    pub dialects: &'static [&'static str],
    pub auth_methods: &'static [&'static str],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_type_round_trips_and_presets_have_base_urls() {
        for t in ProviderType::ALL {
            assert_eq!(ProviderType::parse(t.as_str()), Some(t));
            if let ProviderType::Preset(_) = t {
                assert!(t.compatible() && t.openai_family());
                assert!(t.catalog().default_base_url.is_some());
                assert!(!t.core());
            }
        }
        assert_eq!(ProviderType::parse("triton"), None);
        assert!(ProviderType::unsupported("triton"));
        assert!(!ProviderType::unsupported("foo"));
    }
}
