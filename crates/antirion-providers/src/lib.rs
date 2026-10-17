//! Providers and deployments: compiled once per generation, then used to build each upstream request
//! (URL, body shaping, provider-required headers and the upstream credential: a static key, a SigV4
//! signature or a cached access token).

#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unreachable
)]

mod anthropic;
mod body_edit;
mod caps;
mod compile;
mod compile_auth;
pub mod creds;
pub mod headroom;
mod kind;
pub mod params;
mod passthrough;
pub mod pressure;
mod request;
mod service;
pub mod sigv4;
mod url;

use std::collections::HashMap;
use std::sync::Arc;

use antirion_egress::{EgressPolicy, TlsProfile};
use http::{HeaderMap, HeaderName, HeaderValue};

pub use crate::anthropic::{merge_betas, split_betas};
pub use crate::caps::{Capabilities, FLAGS, Source};
pub use crate::compile::compile;
pub use crate::creds::{AssumeRole, AzureSource, CredError, Credentials, GcpSource, TokenSource};
pub use crate::kind::{Preset, ProviderType, TypeCatalog};
pub use crate::params::Params;
pub use crate::passthrough::Forward;
pub use crate::request::{Authorized, ClientAnthropic, Prepared, UpstreamCall, UpstreamOp};
pub use crate::sigv4::AwsCredentials;

/// How the gateway authenticates to a provider.
#[derive(Clone, Debug)]
pub enum UpstreamAuth {
    /// A header carrying a configured key, e.g. `Authorization: Bearer <key>`.
    Header {
        name: HeaderName,
        value: HeaderValue,
    },
    None,
    /// AWS SigV4: static credentials, or the default chain when `None`.
    Aws(Option<AwsCredentials>),
    /// AWS SigV4 with the credentials of a role assumed through STS with the base credentials
    /// (static, or the default chain when `None`).
    AwsAssumed {
        base: Option<AwsCredentials>,
        role: Arc<AssumeRole>,
    },
    /// A GCP access token, from a service account or the application default credentials.
    Gcp(Arc<GcpSource>),
    /// An Azure AD access token.
    Azure(Arc<AzureSource>),
    /// A method this build does not implement; requests to the provider fail.
    Unsupported(String),
}

impl UpstreamAuth {
    /// The method name of the configuration.
    #[must_use]
    pub fn method(&self) -> &str {
        match self {
            UpstreamAuth::Header { .. } => "api_key",
            UpstreamAuth::None => "none",
            UpstreamAuth::Aws(_) | UpstreamAuth::AwsAssumed { .. } => "aws_sigv4",
            UpstreamAuth::Gcp(_) => "gcp",
            UpstreamAuth::Azure(_) => "azure_ad",
            UpstreamAuth::Unsupported(m) => m,
        }
    }

    /// The source of the bearer tokens this method sends, when it sends tokens.
    #[must_use]
    pub fn token_source(&self) -> Option<TokenSource> {
        match self {
            UpstreamAuth::Gcp(s) => Some(TokenSource::Gcp(Arc::clone(s))),
            UpstreamAuth::Azure(s) => Some(TokenSource::Azure(Arc::clone(s))),
            _ => None,
        }
    }
}

/// One named credential of a provider, as the header that carries it.
#[derive(Clone, Debug)]
pub struct ProviderKey {
    pub name: String,
    pub weight: u32,
    pub value: HeaderValue,
}

/// Whether clients may bring their own provider key (a deployment's credential mode).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Byok {
    #[default]
    Disabled,
    Allowed,
    Required,
}

impl Byok {
    fn parse(v: Option<&str>) -> Option<Byok> {
        Some(match v? {
            "allowed" => Byok::Allowed,
            "required" => Byok::Required,
            _ => Byok::Disabled,
        })
    }

    /// Whether a client key may replace the configured credential.
    #[must_use]
    pub fn accepts(self) -> bool {
        self != Byok::Disabled
    }
}

/// The name of a provider's single `auth.api_key`.
pub const SINGLE_KEY: &str = "default";

/// The `anthropic` block of a provider.
#[derive(Clone, Debug, Default)]
pub struct AnthropicSettings {
    pub version: String,
    pub beta: Vec<String>,
}

/// A compiled provider.
#[derive(Clone, Debug)]
pub struct ProviderRt {
    pub name: String,
    pub kind: ProviderType,
    pub base_url: Option<String>,
    pub enabled: bool,
    pub auth: UpstreamAuth,
    /// The named API keys the gateway rotates over; empty for a provider without keys.
    pub keys: Vec<ProviderKey>,
    pub headers: HeaderMap,
    /// Headers the type requires (`OpenAI-Organization`, `x-goog-user-project`), from its block.
    pub required: HeaderMap,
    pub tls: TlsProfile,
    /// The generation's egress rules, which every connection to this provider is checked against.
    pub egress: Arc<EgressPolicy>,
    /// Operation paths that replace the type's defaults, by operation class.
    pub operations: HashMap<String, String>,
    /// The upstream credentials, for scrubbing upstream text.
    pub secrets: Vec<String>,
    pub anthropic: AnthropicSettings,
    /// `azure.api_style: deployments`, with its `api_version`.
    pub azure_deployments: Option<String>,
    /// `aws.region`.
    pub region: Option<String>,
    /// `gcp.project`.
    pub project: Option<String>,
    /// `gcp.location`.
    pub location: String,
    /// `gemini.api_version`.
    pub gemini_version: String,
    pub params: Params,
    pub byok: Byok,
    /// The header a key travels in and the text before it, for methods that send a key header;
    /// a client's own key goes there.
    pub key_header: Option<(HeaderName, String)>,
    /// The generation's model catalog, the third layer of every deployment's capabilities.
    pub catalog: Arc<antirion_catalog::Catalog>,
}

/// Which Bedrock API a deployment uses.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BedrockApi {
    Auto,
    Converse,
    Invoke,
    /// Bedrock's OpenAI-compatible Chat Completions endpoint.
    OpenAi,
}

/// The publisher of a Vertex model.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Publisher {
    Google,
    Anthropic,
}

/// A compiled deployment.
#[derive(Clone, Debug)]
pub struct DeploymentRt {
    pub name: String,
    pub provider: Arc<ProviderRt>,
    /// The upstream model id.
    pub model: String,
    /// The catalog and pricing key: `base_model`, else the upstream model id.
    pub base_model: String,
    pub enabled: bool,
    pub headers: HeaderMap,
    pub capabilities: Capabilities,
    /// `region`, else the provider's (`aws.region`).
    pub region: Option<String>,
    /// `location` (Vertex), else the provider's.
    pub location: String,
    pub bedrock_api: BedrockApi,
    pub publisher: Publisher,
    pub default_max_tokens: u64,
    pub params: Params,
    /// The effective credential mode: the deployment's `byok`, else the provider's.
    pub byok: Byok,
}

/// Every provider and deployment of one generation.
#[derive(Clone, Debug, Default)]
pub struct ProviderTable {
    pub providers: HashMap<String, Arc<ProviderRt>>,
    pub deployments: HashMap<String, Arc<DeploymentRt>>,
}

/// Why an upstream request could not be built.
#[derive(Debug, PartialEq, Eq)]
pub enum BuildError {
    /// The provider type or auth method cannot serve this request in this build.
    Unsupported,
    InvalidUrl,
    /// A client's own provider key cannot be sent in a header.
    InvalidKey,
}

/// An upstream API a deployment speaks natively.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Native {
    OpenAi,
    AnthropicMessages,
    Gemini,
    BedrockConverse,
    BedrockInvoke,
    /// Cohere's v2 Chat API.
    Cohere,
}

impl DeploymentRt {
    /// The region that region allow-lists match: the stated or AWS region, else a Vertex
    /// deployment's location; `None` when neither is known.
    #[must_use]
    pub fn effective_region(&self) -> Option<&str> {
        self.region
            .as_deref()
            .or_else(|| (self.provider.kind == ProviderType::Vertex).then_some(self.location.as_str()))
    }

    /// Whether the deployment can be selected at all.
    #[must_use]
    pub fn usable(&self) -> bool {
        self.enabled && self.provider.enabled
    }

    /// Whether the model is an Anthropic model (Bedrock and Vertex serve the Messages API for those).
    #[must_use]
    pub fn anthropic_model(&self) -> bool {
        match self.provider.kind {
            ProviderType::Anthropic | ProviderType::AnthropicCompatible => true,
            ProviderType::Vertex => self.publisher == Publisher::Anthropic,
            ProviderType::Bedrock => {
                let m = self.base_model.to_ascii_lowercase();
                m.contains("anthropic.") || m.contains("claude")
            }
            _ => false,
        }
    }

    /// The native APIs of the deployment, in preference order.
    #[must_use]
    pub fn natives(&self) -> Vec<Native> {
        match self.provider.kind {
            k if k.openai_family() => vec![Native::OpenAi],
            k if k.anthropic_api() => vec![Native::AnthropicMessages],
            ProviderType::Cohere => vec![Native::Cohere],
            ProviderType::Gemini => vec![Native::Gemini],
            ProviderType::Vertex => match self.publisher {
                Publisher::Google => vec![Native::Gemini],
                Publisher::Anthropic => vec![Native::AnthropicMessages],
            },
            ProviderType::Bedrock => match self.bedrock_api {
                BedrockApi::OpenAi => vec![Native::OpenAi],
                BedrockApi::Converse => vec![Native::BedrockConverse, Native::BedrockInvoke],
                BedrockApi::Invoke if self.anthropic_model() => {
                    vec![Native::AnthropicMessages, Native::BedrockInvoke]
                }
                BedrockApi::Invoke => vec![Native::BedrockInvoke],
                BedrockApi::Auto if self.anthropic_model() => vec![
                    Native::BedrockConverse,
                    Native::AnthropicMessages,
                    Native::BedrockInvoke,
                ],
                BedrockApi::Auto => vec![Native::BedrockConverse, Native::BedrockInvoke],
            },
            _ => Vec::new(),
        }
    }

    /// Whether the deployment speaks `api` natively.
    #[must_use]
    pub fn speaks(&self, api: Native) -> bool {
        self.natives().contains(&api)
    }
}
