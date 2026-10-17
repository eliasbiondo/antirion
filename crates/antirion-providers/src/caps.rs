//! Capability flags: a deployment's own settings, then the provider's, then the type defaults.

use std::collections::HashMap;

use serde_json::Value;

use crate::ProviderType;

/// A deployment's resolved capability flags: the deployment's own settings, then the provider's,
/// then the type defaults.
#[derive(Clone, Debug, Default)]
pub struct Capabilities {
    deployment: HashMap<String, Value>,
    provider: HashMap<String, Value>,
    /// The model catalog's entry for the deployment's base model.
    catalog: HashMap<String, Value>,
    /// The catalog's time-to-first-token hint for the model.
    ttft: Option<std::time::Duration>,
    kind: Option<ProviderType>,
    /// The upstream dialects of this deployment when they depend on more than its type (a Vertex
    /// publisher, a Bedrock API).
    dialects: Option<Vec<&'static str>>,
    anthropic_model: bool,
}

/// Every capability flag, in table order.
pub const FLAGS: &[&str] = &[
    "dialects",
    "operations",
    "streaming",
    "stream_usage",
    "tools",
    "tool_choice_forced",
    "parallel_tool_calls",
    "json_mode",
    "json_schema",
    "vision",
    "audio_input",
    "pdf_input",
    "reasoning",
    "n_multiple",
    "logprobs",
    "context_window",
    "max_input_tokens",
    "max_output_tokens",
    "tokenizer",
    "prompt_cache_markers",
    "max_n",
    "accepts_max_tokens",
    "developer_role",
    "accepts_cache_control",
    "max_cache_breakpoints",
    "temperature_range",
    "exclusive_temperature_top_p",
    "native_structured_output",
    "reasoning_control",
    "reasoning_efforts",
    "reasoning_min_budget",
    "reasoning_max_budget",
    "reasoning_can_disable",
    "reasoning_budget_ok",
    "reasoning_family",
    "tool_name_pattern",
    "tool_name_max_len",
    "image_formats",
    "image_url_input",
    "document_formats",
    "document_url_input",
    "audio_formats",
    "count_tokens_native",
    "responses_native",
];

/// Where a resolved flag came from.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Deployment,
    Provider,
    Catalog,
    TypeDefault,
}

impl Source {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Deployment => "deployment",
            Source::Provider => "provider",
            Source::Catalog => "catalog",
            Source::TypeDefault => "type_default",
        }
    }
}

fn layer(v: Option<&Value>) -> HashMap<String, Value> {
    v.and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

impl Capabilities {
    /// The flags of a deployment of a `kind` provider, from the provider's and the deployment's
    /// `capabilities` blocks.
    #[must_use]
    pub fn resolve(kind: ProviderType, provider: Option<&Value>, deployment: Option<&Value>) -> Capabilities {
        Capabilities {
            deployment: layer(deployment),
            provider: layer(provider),
            catalog: HashMap::new(),
            ttft: None,
            kind: Some(kind),
            dialects: None,
            anthropic_model: kind.anthropic_api(),
        }
    }

    /// Sets the type-default dialects of a deployment and whether it serves an Anthropic model.
    #[must_use]
    pub fn with_defaults(mut self, dialects: Vec<&'static str>, anthropic_model: bool) -> Capabilities {
        self.dialects = Some(dialects);
        self.anthropic_model = anthropic_model;
        self
    }

    /// Adds the model catalog's entry, the layer between the provider's settings and the type
    /// defaults.
    #[must_use]
    pub fn with_catalog(mut self, entry: Option<&antirion_catalog::Entry>) -> Capabilities {
        if let Some(e) = entry {
            self.catalog = e
                .capabilities
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            self.ttft = e.ttft;
        }
        self
    }

    /// The catalog's time-to-first-token hint for the deployment's model.
    #[must_use]
    pub fn ttft_hint(&self) -> Option<std::time::Duration> {
        self.ttft
    }

    /// A flag's value and where it came from.
    #[must_use]
    pub fn get(&self, name: &str) -> (Value, Source) {
        if let Some(v) = self.deployment.get(name) {
            return (v.clone(), Source::Deployment);
        }
        if let Some(v) = self.provider.get(name) {
            return (v.clone(), Source::Provider);
        }
        if let Some(v) = self.catalog.get(name) {
            return (v.clone(), Source::Catalog);
        }
        (self.type_default(name), Source::TypeDefault)
    }

    fn anthropic_like(&self) -> bool {
        self.anthropic_model || matches!(self.kind, Some(ProviderType::Bedrock))
    }

    fn type_default(&self, name: &str) -> Value {
        let kind = self.kind.unwrap_or(ProviderType::OpenAi);
        let openai_family = kind.openai_family();
        if let ProviderType::Preset(p) = kind
            && let Some(b) = p.default_flag(name)
        {
            return Value::Bool(b);
        }
        if kind == ProviderType::Mistral && name == "stream_usage" {
            return Value::Bool(false);
        }
        if kind == ProviderType::Cohere
            && matches!(
                name,
                "pdf_input" | "audio_input" | "n_multiple" | "logprobs" | "reasoning"
            )
        {
            return Value::Bool(false);
        }
        match name {
            "dialects" => Value::from(
                self.dialects
                    .clone()
                    .unwrap_or_else(|| kind.catalog().dialects.to_vec()),
            ),
            "operations" => Value::from(
                [
                    "generate",
                    "embed",
                    "image_generate",
                    "transcribe",
                    "speech",
                    "moderate",
                    "rerank",
                    "count_tokens",
                    "files",
                    "batches",
                    "realtime",
                ]
                .iter()
                .filter(|c| default_operation(kind, c))
                .copied()
                .collect::<Vec<_>>(),
            ),
            "n_multiple" | "logprobs" | "json_mode" => Value::Bool(!self.anthropic_like()),
            "context_window"
            | "max_input_tokens"
            | "max_output_tokens"
            | "reasoning_min_budget"
            | "reasoning_max_budget" => Value::Null,
            "tokenizer" => Value::from("unknown"),
            "prompt_cache_markers" => Value::Bool(self.anthropic_like()),
            "max_n" => Value::from(if kind == ProviderType::Gemini { 8 } else { 128 }),
            "developer_role" => Value::Bool(matches!(kind, ProviderType::OpenAi | ProviderType::AzureOpenAi)),
            "accepts_cache_control" | "exclusive_temperature_top_p" => Value::Bool(false),
            "max_cache_breakpoints" => Value::from(4),
            "temperature_range" => {
                if self.anthropic_like() {
                    Value::from(vec![0, 1])
                } else {
                    Value::from(vec![0, 2])
                }
            }
            "native_structured_output" => Value::Bool(openai_family || kind == ProviderType::Gemini),
            "reasoning_control" => Value::from(if openai_family { "effort" } else { "none" }),
            "reasoning_efforts" => Value::from(vec!["low", "medium", "high"]),
            "reasoning_family" => Value::from(match kind {
                ProviderType::Anthropic | ProviderType::AnthropicCompatible | ProviderType::Bedrock => {
                    "anthropic"
                }
                ProviderType::Cohere => "cohere",
                ProviderType::Gemini | ProviderType::Vertex => "gemini",
                _ => "openai",
            }),
            "tool_name_pattern" => Value::from(if self.anthropic_like() {
                "^[a-zA-Z0-9_-]{1,128}$"
            } else {
                "^[a-zA-Z0-9_-]{1,64}$"
            }),
            "tool_name_max_len" => Value::from(if self.anthropic_like() { 128 } else { 64 }),
            "image_formats" => Value::from(vec!["png", "jpeg", "gif", "webp"]),
            "document_formats" => Value::from(vec!["pdf"]),
            "audio_formats" => Value::from(vec!["wav", "mp3"]),
            "count_tokens_native" => Value::Bool(self.operation("count_tokens")),
            "responses_native" => Value::Bool(self.speaks("openai_responses")),
            _ => Value::Bool(true),
        }
    }

    /// A boolean flag.
    #[must_use]
    pub fn flag(&self, name: &str) -> bool {
        self.get(name).0.as_bool().unwrap_or(true)
    }

    /// The upstream dialects the deployment accepts natively.
    #[must_use]
    pub fn dialects(&self) -> Vec<String> {
        match self.get("dialects").0 {
            Value::Array(list) => list.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
            _ => Vec::new(),
        }
    }

    /// Whether `dialect` is one of [`Capabilities::dialects`], read without building the list.
    #[must_use]
    pub fn speaks(&self, dialect: &str) -> bool {
        match self
            .deployment
            .get("dialects")
            .or_else(|| self.provider.get("dialects"))
            .or_else(|| self.catalog.get("dialects"))
        {
            Some(Value::Array(list)) => list.iter().any(|d| d.as_str() == Some(dialect)),
            Some(_) => false,
            None => match &self.dialects {
                Some(list) => list.contains(&dialect),
                None => self
                    .kind
                    .unwrap_or(ProviderType::OpenAi)
                    .catalog()
                    .dialects
                    .contains(&dialect),
            },
        }
    }

    /// Whether the deployment serves an operation class.
    #[must_use]
    pub fn operation(&self, class: &str) -> bool {
        for layer in [&self.deployment, &self.provider, &self.catalog] {
            if let Some(list) = layer.get("operations").and_then(Value::as_array) {
                return list.iter().any(|o| o.as_str() == Some(class));
            }
        }
        default_operation(self.kind.unwrap_or(ProviderType::OpenAi), class)
    }
}

fn default_operation(kind: ProviderType, class: &str) -> bool {
    match kind {
        ProviderType::OpenAi | ProviderType::AzureOpenAi => true,
        ProviderType::OpenAiCompatible | ProviderType::Mistral => matches!(class, "generate" | "embed"),
        ProviderType::Preset(p) => class == "generate" || (class == "embed" && p.embeds()),
        ProviderType::Anthropic => matches!(class, "generate" | "count_tokens" | "files" | "batches"),
        ProviderType::AnthropicCompatible => matches!(class, "generate" | "count_tokens"),
        ProviderType::Cohere => matches!(class, "generate" | "embed" | "rerank"),
        ProviderType::Bedrock => matches!(class, "generate" | "embed" | "rerank" | "count_tokens"),
        ProviderType::Vertex | ProviderType::Gemini => matches!(class, "generate" | "embed" | "count_tokens"),
    }
}
