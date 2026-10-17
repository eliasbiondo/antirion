//! How a deployment serves an endpoint: in the client's own dialect (identity), or translated to an
//! API the deployment speaks.

use antirion_dialects::embed::EmbedApi;
use antirion_dialects::rerank::RerankApi;
use antirion_dialects::{Dialect, Endpoint, Kind};
use antirion_providers::{DeploymentRt, Native, ProviderType, Publisher, UpstreamOp};

use crate::parse::Parsed;
use crate::relay::ClientDialect;

/// The API a translated request is sent in.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Upstream {
    Chat,
    /// OpenAI Responses, on a deployment that declares no Chat Completions.
    Responses,
    Anthropic,
    Gemini,
    Converse,
    Cohere,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Cell {
    Identity,
    /// An Anthropic body on the Bedrock invoke route, sent as a Messages request to a deployment
    /// that speaks Messages on another transport.
    InvokeAsMessages,
    /// The request is translated to `upstream` and the response back to `client`.
    Translate {
        client: ClientDialect,
        upstream: Upstream,
    },
    /// An OpenAI embeddings request sent to another provider's embedding API.
    Embed(EmbedApi),
    /// A rerank request in the provider's rerank shape.
    Rerank(RerankApi),
}

/// The embedding API a deployment outside the OpenAI family serves, if any.
fn embed_api(d: &DeploymentRt) -> Option<EmbedApi> {
    if !d.capabilities.operation("embed") {
        return None;
    }
    match d.provider.kind {
        ProviderType::Gemini => Some(EmbedApi::Gemini),
        ProviderType::Vertex if d.publisher == Publisher::Google => Some(EmbedApi::VertexPredict),
        ProviderType::Cohere => Some(EmbedApi::Cohere),
        ProviderType::Bedrock => {
            let m = d.base_model.to_ascii_lowercase();
            if m.contains("titan-embed") {
                Some(EmbedApi::Titan)
            } else if m.contains("cohere.embed") {
                Some(EmbedApi::BedrockCohere)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// The rerank API of a deployment: Cohere's and Bedrock's own, or the `/rerank` of an
/// OpenAI-compatible server that declares the operation (in `top_k` form for the host that takes it).
fn rerank_api(d: &DeploymentRt) -> Option<RerankApi> {
    let caps = &d.capabilities;
    match d.provider.kind {
        ProviderType::Cohere if caps.operation("rerank") => Some(RerankApi::Cohere),
        ProviderType::Bedrock if caps.operation("rerank") => Some(RerankApi::Bedrock),
        k if k.openai_family()
            && (caps.speaks("rerank")
                || (!matches!(k, ProviderType::OpenAi | ProviderType::AzureOpenAi)
                    && caps.operation("rerank"))) =>
        {
            let top_k = d
                .provider
                .base_url
                .as_deref()
                .and_then(|u| u.parse::<http::Uri>().ok())
                .and_then(|u| {
                    u.host()
                        .map(|h| h == "voyageai.com" || h.ends_with(".voyageai.com"))
                })
                .unwrap_or(false);
            Some(if top_k {
                RerankApi::TopK
            } else {
                RerankApi::Compatible
            })
        }
        _ => None,
    }
}

/// A deployment that speaks the OpenAI APIs: an OpenAI-family type, or Bedrock's OpenAI-compatible
/// endpoint.
fn openai_family(d: &DeploymentRt) -> bool {
    d.provider.kind.openai_family() || d.speaks(Native::OpenAi)
}

/// The upstream a translated request goes to: Chat Completions on OpenAI-family deployments (unless
/// they declare only Responses), else the first native API that has a translation.
fn pivot(d: &DeploymentRt) -> Option<Upstream> {
    if openai_family(d) {
        return if d.capabilities.speaks("openai_chat") {
            Some(Upstream::Chat)
        } else if d.capabilities.speaks("openai_responses") {
            Some(Upstream::Responses)
        } else {
            None
        };
    }
    d.natives().iter().find_map(|n| match n {
        Native::AnthropicMessages => Some(Upstream::Anthropic),
        Native::Gemini => Some(Upstream::Gemini),
        Native::BedrockConverse => Some(Upstream::Converse),
        Native::Cohere => Some(Upstream::Cohere),
        _ => None,
    })
}

/// Whether a Gemini endpoint generates content (the endpoints a Gemini client may reach in another
/// dialect).
fn generates(endpoint: &Endpoint) -> bool {
    matches!(endpoint.action, Some("generateContent" | "streamGenerateContent"))
}

/// Whether a body on the Bedrock invoke route is an Anthropic Messages body.
pub(crate) fn anthropic_invoke_body(parsed: &Parsed) -> bool {
    match parsed {
        Parsed::Json { skim, .. } => {
            skim.member("anthropic_version").is_some() || skim.member("messages").is_some()
        }
        Parsed::Multipart { .. } => false,
    }
}

/// The cell of `endpoint` on `deployment`, or `None` when the deployment cannot serve it.
pub(crate) fn cell(endpoint: &Endpoint, parsed: &Parsed, deployment: &DeploymentRt) -> Option<Cell> {
    if parsed
        .pinned_provider()
        .is_some_and(|p| p != deployment.provider.name)
    {
        return None;
    }
    let caps = &deployment.capabilities;
    let speaks = |n| deployment.speaks(n);
    match endpoint.dialect {
        Dialect::OpenAiResponses if endpoint.action == Some("compact") => {
            (openai_family(deployment) && caps.flag("responses_native")).then_some(Cell::Identity)
        }
        Dialect::OpenAiResponses if endpoint.action == Some("input_tokens") => {
            if openai_family(deployment) && caps.flag("responses_native") {
                return Some(Cell::Identity);
            }
            let counts = caps.operation("count_tokens");
            let upstream = if speaks(Native::Gemini) && counts {
                Upstream::Gemini
            } else if speaks(Native::AnthropicMessages) && counts {
                Upstream::Anthropic
            } else {
                return None;
            };
            Some(Cell::Translate {
                client: ClientDialect::Responses,
                upstream,
            })
        }
        Dialect::OpenAiResponses if endpoint.kind == Kind::Inference => {
            if openai_family(deployment) && caps.flag("responses_native") {
                Some(Cell::Identity)
            } else {
                pivot(deployment).map(|upstream| Cell::Translate {
                    client: ClientDialect::Responses,
                    upstream,
                })
            }
        }
        Dialect::AnthropicMessages if speaks(Native::AnthropicMessages) => Some(Cell::Identity),
        Dialect::AnthropicMessages if endpoint.id == "messages" => {
            pivot(deployment).map(|upstream| Cell::Translate {
                client: ClientDialect::Anthropic,
                upstream,
            })
        }
        Dialect::AnthropicMessages
            if openai_family(deployment)
                && caps.flag("responses_native")
                && caps.operation("count_tokens") =>
        {
            Some(Cell::Translate {
                client: ClientDialect::Anthropic,
                upstream: Upstream::Responses,
            })
        }
        Dialect::AnthropicMessages
            if speaks(Native::Gemini) && deployment.capabilities.operation("count_tokens") =>
        {
            Some(Cell::Translate {
                client: ClientDialect::Anthropic,
                upstream: Upstream::Gemini,
            })
        }
        Dialect::AnthropicMessages => None,
        Dialect::OpenAiChat
            if endpoint.id == "chat_completions"
                && (!openai_family(deployment) || !caps.speaks("openai_chat")) =>
        {
            pivot(deployment).map(|upstream| Cell::Translate {
                client: ClientDialect::Chat,
                upstream,
            })
        }
        Dialect::Gemini if speaks(Native::Gemini) => Some(Cell::Identity),
        Dialect::Gemini if generates(endpoint) => pivot(deployment).map(|upstream| Cell::Translate {
            client: ClientDialect::Gemini,
            upstream,
        }),
        Dialect::Gemini => None,
        Dialect::BedrockConverse if speaks(Native::BedrockConverse) => Some(Cell::Identity),
        Dialect::BedrockConverse => pivot(deployment).map(|upstream| Cell::Translate {
            client: ClientDialect::Bedrock,
            upstream,
        }),
        Dialect::BedrockInvoke if deployment.provider.kind == ProviderType::Bedrock => Some(Cell::Identity),
        Dialect::BedrockInvoke if anthropic_invoke_body(parsed) && speaks(Native::AnthropicMessages) => {
            Some(Cell::InvokeAsMessages)
        }
        Dialect::BedrockInvoke => None,
        Dialect::OpenAiEmbeddings if !openai_family(deployment) => embed_api(deployment)
            .filter(|_| crate::embed::text_inputs(parsed))
            .map(Cell::Embed),
        Dialect::Rerank => rerank_api(deployment).map(Cell::Rerank),
        Dialect::OpenAiCompletions if !caps.speaks("openai_completions") => None,
        d if deployment.provider.kind.serves(d) => Some(Cell::Identity),
        Dialect::OpenAiChat if deployment.speaks(Native::OpenAi) => Some(Cell::Identity),
        _ => None,
    }
}

/// The operation class of an endpoint, for capability checks and path overrides.
pub(crate) fn operation_class(endpoint: &Endpoint) -> &'static str {
    match (endpoint.id, endpoint.upstream_path, endpoint.action) {
        ("embeddings", _, _) | (_, _, Some("embedContent" | "batchEmbedContents")) => "embed",
        ("rerank", _, _) => "rerank",
        ("images", _, _) => "image_generate",
        ("moderations", _, _) => "moderate",
        ("audio", Some("/audio/speech"), _) => "speech",
        ("audio", _, _) => "transcribe",
        _ if endpoint.counts_tokens() => "count_tokens",
        _ => "generate",
    }
}

/// What the upstream is asked to do in `cell`.
pub(crate) fn upstream_op(endpoint: &Endpoint, cell: Cell) -> UpstreamOp<'static> {
    match cell {
        Cell::Embed(api) => crate::embed::upstream_op(api, false),
        Cell::Rerank(api) => crate::embed::rerank_op(api),
        Cell::Translate {
            upstream: Upstream::Anthropic,
            ..
        } if endpoint.counts_tokens() => UpstreamOp::CountTokens,
        Cell::InvokeAsMessages
        | Cell::Translate {
            upstream: Upstream::Anthropic,
            ..
        } => UpstreamOp::Messages,
        Cell::Translate {
            upstream: Upstream::Gemini,
            ..
        } if endpoint.counts_tokens() => UpstreamOp::GeminiCountTokens,
        Cell::Translate {
            upstream: Upstream::Gemini,
            ..
        } => UpstreamOp::GeminiGenerate,
        Cell::Translate {
            upstream: Upstream::Converse,
            ..
        } => UpstreamOp::Converse,
        Cell::Translate {
            upstream: Upstream::Cohere,
            ..
        } => UpstreamOp::CohereChat,
        Cell::Translate {
            upstream: Upstream::Chat,
            ..
        } => UpstreamOp::OpenAi {
            class: "generate",
            path: "/chat/completions",
        },
        Cell::Translate {
            upstream: Upstream::Responses,
            ..
        } if endpoint.counts_tokens() => UpstreamOp::OpenAi {
            class: "count_tokens",
            path: "/responses/input_tokens",
        },
        Cell::Translate {
            upstream: Upstream::Responses,
            ..
        } => UpstreamOp::OpenAi {
            class: "generate",
            path: "/responses",
        },
        Cell::Identity => match endpoint.dialect {
            Dialect::AnthropicMessages if endpoint.id == "count_tokens" => UpstreamOp::CountTokens,
            Dialect::AnthropicMessages => UpstreamOp::Messages,
            Dialect::Gemini if endpoint.action == Some("countTokens") => UpstreamOp::GeminiCountTokens,
            Dialect::Gemini => UpstreamOp::GeminiGenerate,
            Dialect::BedrockConverse => UpstreamOp::Converse,
            Dialect::BedrockInvoke => UpstreamOp::Invoke,
            _ => UpstreamOp::OpenAi {
                class: operation_class(endpoint),
                path: endpoint.upstream_path.unwrap_or("/chat/completions"),
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn deployments() -> antirion_providers::ProviderTable {
        antirion_providers::compile(&json!({
            "providers": {
                "g": {"type": "gemini", "base_url": "http://h/g", "auth": {"api_key": "k"}},
                "b": {"type": "bedrock", "base_url": "http://h/b", "aws": {"region": "us-east-1"},
                      "auth": {"method": "api_key", "api_key": "k"}},
                "c": {"type": "cohere", "base_url": "http://h/c", "auth": {"api_key": "k"}},
                "o": {"type": "openai", "base_url": "http://h/o", "auth": {"api_key": "k"}},
                "j": {"type": "openai_compatible", "base_url": "http://jina.local/v1", "auth": {"api_key": "k"},
                      "capabilities": {"operations": ["rerank"]}},
                "y": {"type": "openai_compatible", "base_url": "https://api.voyageai.com/v1", "auth": {"api_key": "k"},
                      "capabilities": {"operations": ["rerank"]}},
            },
            "deployments": {
                "g": {"provider": "g", "model": "text-embedding-004"},
                "titan": {"provider": "b", "model": "amazon.titan-embed-text-v2:0"},
                "bcohere": {"provider": "b", "model": "cohere.embed-english-v3"},
                "claude": {"provider": "b", "model": "anthropic.claude-v1:0"},
                "c": {"provider": "c", "model": "embed-v4.0"},
                "o": {"provider": "o", "model": "gpt-x"},
                "j": {"provider": "j", "model": "jina-reranker"},
                "y": {"provider": "y", "model": "rerank-2"},
            }
        }))
        .unwrap()
    }

    #[test]
    fn embedding_apis_by_provider_and_model() {
        let t = deployments();
        let api = |d: &str| embed_api(&t.deployments[d]);
        assert_eq!(api("g"), Some(EmbedApi::Gemini));
        assert_eq!(api("titan"), Some(EmbedApi::Titan));
        assert_eq!(api("bcohere"), Some(EmbedApi::BedrockCohere));
        assert_eq!(api("claude"), None);
        assert_eq!(api("c"), Some(EmbedApi::Cohere));
        assert_eq!(api("o"), None);
    }

    #[test]
    fn rerank_apis_by_provider() {
        let t = deployments();
        let api = |d: &str| rerank_api(&t.deployments[d]);
        assert_eq!(api("c"), Some(RerankApi::Cohere));
        assert_eq!(api("claude"), Some(RerankApi::Bedrock));
        assert_eq!(api("j"), Some(RerankApi::Compatible));
        assert_eq!(api("y"), Some(RerankApi::TopK));
        assert_eq!(
            api("o"),
            None,
            "OpenAI serves no rerank unless its dialects say so"
        );
        assert_eq!(api("g"), None);
    }
}
