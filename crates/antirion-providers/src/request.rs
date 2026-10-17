//! Building one upstream request: the URL of the operation on the provider's type, the body with the
//! provider's envelope changes, the required headers, and finally the credential.

use std::sync::Arc;
use std::time::Duration;

use antirion_core::{Clock, RequestId, headers as h};
use antirion_egress::{OutboundRequest, Payload, TlsProfile, TrustClass};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use http::header::{AUTHORIZATION, CONTENT_TYPE};
use http::{HeaderMap, HeaderName, HeaderValue, Method, Uri};
use serde_json::Value;

use crate::anthropic::{Platform, filter, merge_betas};
use crate::body_edit::BodyEdit;
use crate::creds::{CredError, Credentials, TokenSource};
use crate::params::Applied;
use crate::url::{fill, join, segment};
use crate::{BuildError, DeploymentRt, ProviderType, UpstreamAuth, sigv4};

/// What the upstream is asked to do.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum UpstreamOp<'a> {
    /// An OpenAI-family endpoint: its operation class and its path relative to the base URL.
    OpenAi {
        class: &'a str,
        path: &'a str,
    },
    /// Anthropic Messages, on whichever transport the provider type has.
    Messages,
    /// Anthropic token counting.
    CountTokens,
    GeminiGenerate,
    GeminiCountTokens,
    Converse,
    /// Bedrock InvokeModel with a model-native body.
    Invoke,
    /// Cohere's v2 Chat.
    CohereChat,
    /// Gemini `embedContent`, or `batchEmbedContents` for several texts.
    GeminiEmbed {
        batch: bool,
    },
    /// Vertex AI `predict` on a Google model (embeddings).
    VertexPredict,
    /// Cohere's v2 `embed`.
    CohereEmbed,
    /// Cohere's v2 `rerank`.
    CohereRerank,
    /// Bedrock's agent runtime `Rerank`, served from its own host.
    BedrockRerank,
    /// The Realtime WebSocket endpoint, opened with `GET` and an upgrade.
    Realtime,
}

impl UpstreamOp<'_> {
    fn class(&self) -> &str {
        match self {
            UpstreamOp::OpenAi { class, .. } => class,
            UpstreamOp::CountTokens | UpstreamOp::GeminiCountTokens => "count_tokens",
            UpstreamOp::GeminiEmbed { .. } | UpstreamOp::VertexPredict | UpstreamOp::CohereEmbed => "embed",
            UpstreamOp::CohereRerank | UpstreamOp::BedrockRerank => "rerank",
            UpstreamOp::Realtime => "realtime",
            _ => "generate",
        }
    }
}

/// The Anthropic headers an Anthropic-dialect client sent.
#[derive(Clone, Debug, Default)]
pub struct ClientAnthropic {
    pub version: Option<String>,
    pub betas: Vec<String>,
}

/// What one upstream call sends.
#[derive(Debug)]
pub struct UpstreamCall<'a> {
    pub op: UpstreamOp<'a>,
    pub stream: bool,
    /// The body in the upstream's dialect, before the provider's envelope changes.
    pub body: Payload,
    pub content_type: Option<HeaderValue>,
    pub request_id: &'a RequestId,
    pub response_timeout: Duration,
    pub connect_timeout: Option<Duration>,
    pub anthropic: Option<&'a ClientAnthropic>,
    /// The name of the provider key to send; the provider's first key when `None`.
    pub key: Option<&'a str>,
    /// The client's own provider key, sent instead of the configured one where the deployment's
    /// credential mode accepts it.
    pub byok: Option<&'a str>,
}

/// A built request, waiting for its credential.
#[derive(Debug)]
pub struct Prepared {
    pub request: OutboundRequest,
    /// A platform beta table dropped some client flags.
    pub dropped_betas: bool,
    /// The `params` entries that changed the body.
    pub applied: Applied,
}

/// The credential a request was sent with, when it can be refreshed after a rejection.
#[derive(Clone)]
pub enum Authorized {
    Static,
    /// Signed with AWS credentials the gateway obtained; their secret parts.
    Signed {
        secrets: Vec<String>,
    },
    Token {
        source: TokenSource,
        token: String,
    },
}

impl std::fmt::Debug for Authorized {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Authorized::Static => "Static",
            Authorized::Signed { .. } => "Signed",
            Authorized::Token { .. } => "Token",
        })
    }
}

impl Authorized {
    /// The obtained secrets the request carried, which upstream text is scrubbed of.
    #[must_use]
    pub fn secrets(&self) -> Vec<String> {
        match self {
            Authorized::Static => Vec::new(),
            Authorized::Signed { secrets } => secrets.clone(),
            Authorized::Token { token, .. } => vec![token.clone()],
        }
    }
}

/// Signs a Bedrock request with `c` over its final bytes.
pub(crate) fn sign_bedrock(
    req: &mut OutboundRequest,
    c: &crate::sigv4::AwsCredentials,
    region: &str,
) -> Authorized {
    let now = antirion_core::SystemClock.now();
    let amz_date = jiff::Timestamp::from_microsecond(now.0)
        .map(|t| t.strftime("%Y%m%dT%H%M%SZ").to_string())
        .unwrap_or_default();
    let body = req.body.contiguous();
    sigv4::sign(
        &mut req.headers,
        &sigv4::SigningInput {
            method: &req.method,
            uri: &req.uri,
            body: &body,
            region,
            service: "bedrock",
            amz_date: &amz_date,
        },
        c,
    );
    Authorized::Signed {
        secrets: std::iter::once(c.secret_access_key.expose().to_owned())
            .chain(c.session_token.as_ref().map(|t| t.expose().to_owned()))
            .collect(),
    }
}

/// An operation's query parameters.
type Query = Vec<(&'static str, String)>;

const VERTEX_ANTHROPIC_VERSION: &str = "vertex-2023-10-16";
const BEDROCK_ANTHROPIC_VERSION: &str = "bedrock-2023-05-31";

impl DeploymentRt {
    pub(crate) fn base(&self) -> Result<String, BuildError> {
        let p = &self.provider;
        let region = self.region.as_deref().unwrap_or("us-east-1");
        let location = self.location.as_str();
        if let Some(b) = &p.base_url {
            return Ok(fill(b, &[("region", region), ("location", location)]));
        }
        Ok(match p.kind {
            ProviderType::Bedrock => format!("https://bedrock-runtime.{region}.amazonaws.com"),
            ProviderType::Vertex if self.express() || location == "global" => {
                "https://aiplatform.googleapis.com".to_owned()
            }
            ProviderType::Vertex if matches!(location, "us" | "eu") => {
                format!("https://aiplatform.{location}.rep.googleapis.com")
            }
            ProviderType::Vertex => format!("https://{location}-aiplatform.googleapis.com"),
            other => other
                .default_base_url()
                .ok_or(BuildError::Unsupported)?
                .to_owned(),
        })
    }

    /// The base URL of an operation: Bedrock's rerank is served by the agent runtime host unless the
    /// provider names its own base.
    fn base_for(&self, op: UpstreamOp<'_>) -> Result<String, BuildError> {
        if op == UpstreamOp::BedrockRerank
            && self.provider.kind == ProviderType::Bedrock
            && self.provider.base_url.is_none()
        {
            let region = self.region.as_deref().unwrap_or("us-east-1");
            return Ok(format!("https://bedrock-agent-runtime.{region}.amazonaws.com"));
        }
        self.base()
    }

    /// Vertex express mode: an API key, Google models only, no project path.
    fn express(&self) -> bool {
        self.provider.kind == ProviderType::Vertex
            && matches!(self.provider.auth, UpstreamAuth::Header { .. })
    }

    fn vertex_model_path(&self, publisher: &str, model: &str) -> String {
        if self.express() {
            return format!("/v1/publishers/{publisher}/models/{model}");
        }
        format!(
            "/v1/projects/{}/locations/{}/publishers/{publisher}/models/{model}",
            segment(self.provider.project.as_deref().unwrap_or_default()),
            segment(&self.location),
        )
    }

    fn gemini_model(&self) -> String {
        if self.model.starts_with("models/") || self.model.starts_with("tunedModels/") {
            self.model.clone()
        } else {
            format!("models/{}", segment(&self.model))
        }
    }

    /// The path and the operation's query parameters.
    fn path(&self, op: UpstreamOp<'_>, stream: bool) -> Result<(String, Query), BuildError> {
        let p = &self.provider;
        let model = segment(&self.model);
        if let Some(over) = p.operations.get(op.class()) {
            let path = fill(
                over,
                &[
                    ("model", &model),
                    ("project", p.project.as_deref().unwrap_or_default()),
                    ("location", &self.location),
                    ("region", self.region.as_deref().unwrap_or_default()),
                    ("api_version", &p.gemini_version),
                ],
            );
            return Ok((path, Vec::new()));
        }
        let sse = || vec![("alt", "sse".to_owned())];
        Ok(match (p.kind, op) {
            (ProviderType::AzureOpenAi, UpstreamOp::OpenAi { path, .. }) => match &p.azure_deployments {
                Some(v) => (
                    format!("/openai/deployments/{model}{path}"),
                    vec![("api-version", v.clone())],
                ),
                None => (format!("/openai/v1{path}"), Vec::new()),
            },
            (
                ProviderType::Bedrock,
                UpstreamOp::OpenAi {
                    path: "/chat/completions",
                    ..
                },
            ) => ("/openai/v1/chat/completions".to_owned(), Vec::new()),
            (ProviderType::Bedrock, UpstreamOp::OpenAi { .. }) => return Err(BuildError::Unsupported),
            (_, UpstreamOp::OpenAi { path, .. }) => (path.to_owned(), Vec::new()),
            (ProviderType::Cohere, UpstreamOp::CohereChat) => ("/v2/chat".to_owned(), Vec::new()),
            (k, UpstreamOp::Messages) if k.anthropic_api() => ("/v1/messages".to_owned(), Vec::new()),
            (k, UpstreamOp::CountTokens) if k.anthropic_api() => {
                ("/v1/messages/count_tokens".to_owned(), Vec::new())
            }
            (ProviderType::Bedrock, UpstreamOp::Messages | UpstreamOp::Invoke) => {
                let action = if stream {
                    "invoke-with-response-stream"
                } else {
                    "invoke"
                };
                (format!("/model/{model}/{action}"), Vec::new())
            }
            (ProviderType::Bedrock, UpstreamOp::Converse) => {
                let action = if stream { "converse-stream" } else { "converse" };
                (format!("/model/{model}/{action}"), Vec::new())
            }
            (ProviderType::Bedrock, UpstreamOp::CountTokens) => {
                (format!("/model/{model}/count-tokens"), Vec::new())
            }
            (ProviderType::Vertex, UpstreamOp::Messages) => {
                let action = if stream { "streamRawPredict" } else { "rawPredict" };
                (
                    format!("{}:{action}", self.vertex_model_path("anthropic", &model)),
                    Vec::new(),
                )
            }
            (ProviderType::Vertex, UpstreamOp::CountTokens) => (
                format!(
                    "{}:rawPredict",
                    self.vertex_model_path("anthropic", "count-tokens")
                ),
                Vec::new(),
            ),
            (ProviderType::Vertex, UpstreamOp::GeminiGenerate) if stream => (
                format!(
                    "{}:streamGenerateContent",
                    self.vertex_model_path("google", &model)
                ),
                sse(),
            ),
            (ProviderType::Vertex, UpstreamOp::GeminiGenerate) => (
                format!("{}:generateContent", self.vertex_model_path("google", &model)),
                Vec::new(),
            ),
            (ProviderType::Vertex, UpstreamOp::GeminiCountTokens) => (
                format!("{}:countTokens", self.vertex_model_path("google", &model)),
                Vec::new(),
            ),
            (ProviderType::Gemini, UpstreamOp::GeminiGenerate) if stream => (
                format!(
                    "/{}/{}:streamGenerateContent",
                    p.gemini_version,
                    self.gemini_model()
                ),
                sse(),
            ),
            (ProviderType::Gemini, UpstreamOp::GeminiGenerate) => (
                format!("/{}/{}:generateContent", p.gemini_version, self.gemini_model()),
                Vec::new(),
            ),
            (ProviderType::Gemini, UpstreamOp::GeminiCountTokens) => (
                format!("/{}/{}:countTokens", p.gemini_version, self.gemini_model()),
                Vec::new(),
            ),
            (ProviderType::Gemini, UpstreamOp::GeminiEmbed { batch }) => {
                let action = if batch {
                    "batchEmbedContents"
                } else {
                    "embedContent"
                };
                (
                    format!("/{}/{}:{action}", p.gemini_version, self.gemini_model()),
                    Vec::new(),
                )
            }
            (ProviderType::Vertex, UpstreamOp::VertexPredict) => (
                format!("{}:predict", self.vertex_model_path("google", &model)),
                Vec::new(),
            ),
            (ProviderType::Cohere, UpstreamOp::CohereEmbed) => ("/v2/embed".to_owned(), Vec::new()),
            (ProviderType::Cohere, UpstreamOp::CohereRerank) => ("/v2/rerank".to_owned(), Vec::new()),
            (ProviderType::Bedrock, UpstreamOp::BedrockRerank) => ("/rerank".to_owned(), Vec::new()),
            (ProviderType::AzureOpenAi, UpstreamOp::Realtime) => match &p.azure_deployments {
                Some(v) => (
                    "/openai/realtime".to_owned(),
                    vec![("api-version", v.clone()), ("deployment", self.model.clone())],
                ),
                None => (
                    "/openai/v1/realtime".to_owned(),
                    vec![("model", self.model.clone())],
                ),
            },
            (k, UpstreamOp::Realtime) if k.openai_family() => {
                ("/realtime".to_owned(), vec![("model", self.model.clone())])
            }
            _ => return Err(BuildError::Unsupported),
        })
    }

    /// The `max_tokens` an Anthropic body gets when it has none.
    fn max_tokens(&self) -> u64 {
        self.capabilities
            .get("max_output_tokens")
            .0
            .as_u64()
            .filter(|n| *n > 0)
            .unwrap_or(self.default_max_tokens.max(1))
    }

    /// The body with the provider's model placement and envelope changes (in order: model, envelope,
    /// default `max_tokens`); the flags of the `anthropic-beta` header, and whether a platform table
    /// dropped client flags.
    fn shape(&self, call: &UpstreamCall<'_>) -> (Payload, Option<String>, bool) {
        let p = &self.provider;
        let client_betas: &[String] = call.anthropic.map_or(&[], |a| a.betas.as_slice());
        let mut e = BodyEdit::new(&call.body);
        let mut header = None;
        let mut dropped = false;
        match (p.kind, call.op) {
            (k, UpstreamOp::Messages | UpstreamOp::CountTokens) if k.anthropic_api() => {
                e.set("model", &Value::from(self.model.as_str()));
                e.remove("anthropic_version");
                if call.op == UpstreamOp::Messages && !e.has("max_tokens") {
                    e.set("max_tokens", &Value::from(self.max_tokens()));
                }
                if call.op == UpstreamOp::Messages && call.stream && !e.has("stream") {
                    e.set("stream", &Value::Bool(true));
                }
                let merged = merge_betas(client_betas, &p.anthropic.beta);
                header = (!merged.is_empty()).then(|| merged.join(","));
            }
            (ProviderType::Bedrock, UpstreamOp::Messages | UpstreamOp::CountTokens) => {
                e.remove("model");
                e.remove("stream");
                e.set("anthropic_version", &Value::from(BEDROCK_ANTHROPIC_VERSION));
                if !e.has("max_tokens") {
                    e.set("max_tokens", &Value::from(self.max_tokens()));
                }
                let body_betas: Vec<String> = e
                    .value("anthropic_beta")
                    .and_then(|v| v.as_array().cloned())
                    .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                    .unwrap_or_default();
                let mut client: Vec<String> = client_betas.to_vec();
                client.extend(body_betas);
                let (kept, d) = filter(Platform::Bedrock, &client);
                dropped = d;
                let merged = merge_betas(&kept, &p.anthropic.beta);
                if merged.is_empty() {
                    e.remove("anthropic_beta");
                } else {
                    e.set("anthropic_beta", &Value::from(merged));
                }
                if call.op == UpstreamOp::CountTokens {
                    let inner = e.build().contiguous();
                    let wrapped =
                        serde_json::json!({"input": {"invokeModel": {"body": STANDARD.encode(&inner)}}});
                    return (
                        Payload::from(serde_json::to_vec(&wrapped).unwrap_or_default()),
                        None,
                        dropped,
                    );
                }
            }
            (ProviderType::Vertex, UpstreamOp::Messages | UpstreamOp::CountTokens) => {
                if call.op == UpstreamOp::CountTokens {
                    e.set("model", &Value::from(self.model.as_str()));
                } else {
                    e.remove("model");
                    e.set("anthropic_version", &Value::from(VERTEX_ANTHROPIC_VERSION));
                    if !e.has("max_tokens") {
                        e.set("max_tokens", &Value::from(self.max_tokens()));
                    }
                }
                let (kept, d) = filter(Platform::Vertex, client_betas);
                dropped = d;
                let merged = merge_betas(&kept, &p.anthropic.beta);
                header = (!merged.is_empty()).then(|| merged.join(","));
            }
            (
                _,
                UpstreamOp::GeminiGenerate
                | UpstreamOp::GeminiCountTokens
                | UpstreamOp::Converse
                | UpstreamOp::Invoke,
            ) => e.remove("model"),
            (ProviderType::AzureOpenAi, UpstreamOp::OpenAi { .. }) if p.azure_deployments.is_some() => {
                e.remove("model");
            }
            _ => {}
        }
        (e.build(), header, dropped)
    }

    /// Builds the upstream request of one attempt; the credential is added by
    /// [`DeploymentRt::authorize`] once every header is final.
    pub fn build_request(&self, call: UpstreamCall<'_>) -> Result<Prepared, BuildError> {
        let p = &self.provider;
        if let UpstreamAuth::Unsupported(_) = p.auth {
            return Err(BuildError::Unsupported);
        }
        let (path, query) = self.path(call.op, call.stream)?;
        let query: Vec<(&str, &str)> = query.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let url = join(&self.base_for(call.op)?, &path, &query);
        let uri: Uri = url.parse().map_err(|_| BuildError::InvalidUrl)?;
        let (mut body, beta_header, dropped_betas) = self.shape(&call);
        let mut applied = Applied::default();
        let json = call
            .content_type
            .as_ref()
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.starts_with("application/json"));
        if json
            && (!p.params.is_empty() || !self.params.is_empty())
            && let Ok(mut v) = serde_json::from_slice::<Value>(&body.contiguous())
        {
            p.params.apply(&mut v, &mut applied);
            self.params.apply(&mut v, &mut applied);
            body = Payload::from(serde_json::to_vec(&v).unwrap_or_default());
        }
        let mut headers = HeaderMap::new();
        if let Some(ct) = call.content_type {
            headers.insert(CONTENT_TYPE, ct);
        }
        if let Ok(v) = HeaderValue::from_str(call.request_id.as_str()) {
            if p.kind.openai_family() {
                headers.insert(HeaderName::from_static(h::X_CLIENT_REQUEST_ID), v.clone());
            }
            headers.insert(HeaderName::from_static(h::X_REQUEST_ID), v);
        }
        if p.kind.anthropic_api() && matches!(call.op, UpstreamOp::Messages | UpstreamOp::CountTokens) {
            let version = call
                .anthropic
                .and_then(|a| a.version.clone())
                .unwrap_or_else(|| p.anthropic.version.clone());
            if let Ok(v) = HeaderValue::from_str(&version) {
                headers.insert(HeaderName::from_static("anthropic-version"), v);
            }
        }
        if let Some(v) = beta_header.and_then(|b| HeaderValue::from_str(&b).ok()) {
            headers.insert(HeaderName::from_static("anthropic-beta"), v);
        }
        for (name, value) in p
            .required
            .iter()
            .chain(p.headers.iter())
            .chain(self.headers.iter())
        {
            headers.insert(name.clone(), value.clone());
        }
        match (call.byok.filter(|_| self.byok.accepts()), &p.key_header, &p.auth) {
            (Some(client), Some((name, prefix)), _) => {
                let mut v = HeaderValue::from_str(&format!("{prefix}{client}"))
                    .map_err(|_| BuildError::InvalidKey)?;
                v.set_sensitive(true);
                headers.insert(name.clone(), v);
            }
            (_, _, UpstreamAuth::Header { name, value }) => {
                let chosen = call
                    .key
                    .and_then(|k| p.keys.iter().find(|pk| pk.name == k))
                    .map_or(value, |pk| &pk.value);
                headers.insert(name.clone(), chosen.clone());
            }
            _ => {}
        }
        Ok(Prepared {
            request: OutboundRequest {
                method: Method::POST,
                uri,
                headers,
                body,
                response_timeout: call.response_timeout,
                connect_timeout: call.connect_timeout,
                fresh_connection: false,
                trust: TrustClass::Operator,
                tls: p.tls.clone(),
                policy: Arc::clone(&p.egress),
                resend_when_closed: true,
            },
            dropped_betas,
            applied,
        })
    }

    /// Adds the credential that must be acquired (an access token) or computed over the final
    /// request (a SigV4 signature). `cred_tls` is the credential-acquisition client's TLS profile.
    pub async fn authorize(
        &self,
        req: &mut OutboundRequest,
        creds: &Credentials,
        cred_tls: &TlsProfile,
    ) -> Result<Authorized, CredError> {
        let policy = Arc::clone(&req.policy);
        let token = match &self.provider.auth {
            UpstreamAuth::Aws(configured) => {
                let region = self.region.as_deref().unwrap_or("us-east-1");
                let c = creds.aws(configured.as_ref(), region, cred_tls, &policy).await?;
                return Ok(sign_bedrock(req, &c, region));
            }
            UpstreamAuth::AwsAssumed { base, role } => {
                let region = self.region.as_deref().unwrap_or("us-east-1");
                let c = creds
                    .aws_assumed(base.as_ref(), role, region, cred_tls, &policy)
                    .await?;
                return Ok(sign_bedrock(req, &c, region));
            }
            auth => match auth.token_source() {
                Some(source) => source,
                None => return Ok(Authorized::Static),
            },
        };
        let value = creds.token(&token, cred_tls, &policy).await?;
        let mut v = HeaderValue::from_str(&format!("Bearer {value}")).map_err(|_| CredError::Malformed)?;
        v.set_sensitive(true);
        req.headers.insert(AUTHORIZATION, v);
        Ok(Authorized::Token {
            source: token,
            token: value,
        })
    }

    /// Whether the deployment's model is addressed by the URL path (the body carries no `model`).
    #[must_use]
    pub fn path_model(&self) -> bool {
        matches!(
            self.provider.kind,
            ProviderType::Bedrock | ProviderType::Vertex | ProviderType::Gemini
        ) || (self.provider.kind == ProviderType::AzureOpenAi && self.provider.azure_deployments.is_some())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use bytes::Bytes;

    use super::*;
    use crate::compile;

    fn table(doc: Value) -> crate::ProviderTable {
        compile(&doc).unwrap()
    }

    fn call<'a>(
        op: UpstreamOp<'a>,
        stream: bool,
        body: &'static [u8],
        rid: &'a RequestId,
    ) -> UpstreamCall<'a> {
        UpstreamCall {
            op,
            stream,
            body: Bytes::from_static(body).into(),
            content_type: Some(HeaderValue::from_static("application/json")),
            request_id: rid,
            response_timeout: Duration::from_secs(1),
            connect_timeout: None,
            anthropic: None,
            key: None,
            byok: None,
        }
    }

    #[test]
    fn openai_request() {
        let t = table(json!({
            "providers": {"p": {"type": "openai", "base_url": "http://127.0.0.1:9/v1/", "auth": {"api_key": "sk-up"},
                                "headers": {"x-escaped": "a$b"}}},
            "deployments": {"d": {"provider": "p", "model": "up-model", "headers": {"x-dep": "1"}}}
        }));
        let rid = RequestId::generate();
        let r = t.deployments["d"]
            .build_request(call(
                UpstreamOp::OpenAi {
                    class: "generate",
                    path: "/chat/completions",
                },
                false,
                b"{}",
                &rid,
            ))
            .unwrap()
            .request;
        assert_eq!(r.uri.to_string(), "http://127.0.0.1:9/v1/chat/completions");
        assert_eq!(r.headers["authorization"], "Bearer sk-up");
        assert_eq!(r.headers["x-escaped"], "a$b");
        assert_eq!(r.headers["x-dep"], "1");
        assert_eq!(r.headers["x-client-request-id"], rid.as_str());
    }

    #[test]
    fn a_stream_operation_asks_an_anthropic_upstream_to_stream() {
        let t = table(json!({
            "providers": {"a": {"type": "anthropic", "base_url": "http://h/a", "auth": {"api_key": "k"}}},
            "deployments": {"a": {"provider": "a", "model": "claude-up"}}
        }));
        let rid = RequestId::generate();
        let body = |stream, raw: &'static [u8]| -> Value {
            let r = t.deployments["a"]
                .build_request(call(UpstreamOp::Messages, stream, raw, &rid))
                .unwrap()
                .request;
            serde_json::from_slice(&r.body.contiguous()).unwrap()
        };
        let invoke = br#"{"anthropic_version":"bedrock-2023-05-31","max_tokens":5,"messages":[]}"#;
        let streamed = body(true, invoke);
        assert_eq!(streamed["stream"], true);
        assert_eq!(streamed["model"], "claude-up");
        assert!(streamed.get("anthropic_version").is_none());
        assert!(body(false, invoke).get("stream").is_none());
        assert_eq!(
            body(true, br#"{"stream":true,"max_tokens":5,"messages":[]}"#)["stream"],
            true
        );
    }

    #[test]
    fn native_paths() {
        let t = table(json!({
            "providers": {
                "b": {"type": "bedrock", "base_url": "http://h/b", "aws": {"region": "us-east-1"},
                      "auth": {"method": "api_key", "api_key": "k"}},
                "g": {"type": "gemini", "base_url": "http://h/g", "auth": {"api_key": "k"}},
                "v": {"type": "vertex", "base_url": "http://h/v", "auth": {"method": "api_key", "api_key": "k"}},
                "a": {"type": "anthropic", "base_url": "http://h/a", "auth": {"api_key": "k"}},
            },
            "deployments": {
                "b": {"provider": "b", "model": "anthropic.claude-v1:0"},
                "g": {"provider": "g", "model": "gemini-x"},
                "v": {"provider": "v", "model": "gemini-x"},
                "a": {"provider": "a", "model": "claude-up"},
            }
        }));
        let rid = RequestId::generate();
        let uri = |d: &str, op, stream| {
            t.deployments[d]
                .build_request(call(op, stream, b"{}", &rid))
                .unwrap()
                .request
                .uri
                .to_string()
        };
        assert_eq!(
            uri("b", UpstreamOp::Converse, true),
            "http://h/b/model/anthropic.claude-v1%3A0/converse-stream"
        );
        assert_eq!(
            uri("b", UpstreamOp::Messages, true),
            "http://h/b/model/anthropic.claude-v1%3A0/invoke-with-response-stream"
        );
        assert_eq!(
            uri("g", UpstreamOp::GeminiGenerate, true),
            "http://h/g/v1beta/models/gemini-x:streamGenerateContent?alt=sse"
        );
        assert_eq!(
            uri("v", UpstreamOp::GeminiGenerate, false),
            "http://h/v/v1/publishers/google/models/gemini-x:generateContent"
        );
        assert_eq!(
            uri("a", UpstreamOp::CountTokens, false),
            "http://h/a/v1/messages/count_tokens"
        );
    }

    #[test]
    fn anthropic_envelopes() {
        let t = table(json!({
            "providers": {
                "b": {"type": "bedrock", "aws": {"region": "us-east-1"}, "anthropic": {"beta": ["op"]}},
                "a": {"type": "anthropic", "auth": {"api_key": "k"}},
            },
            "deployments": {
                "b": {"provider": "b", "model": "anthropic.claude-v1:0", "bedrock": {"api": "invoke"}},
                "a": {"provider": "a", "model": "claude-up", "default_max_tokens": 100},
            }
        }));
        let rid = RequestId::generate();
        let body = |d: &str, op| {
            let r = t.deployments[d]
                .build_request(call(
                    op,
                    true,
                    br#"{"model":"m","stream":true,"messages":[]}"#,
                    &rid,
                ))
                .unwrap()
                .request;
            (
                serde_json::from_slice::<Value>(&r.body.contiguous()).unwrap(),
                r.headers,
            )
        };
        let (b, h) = body("b", UpstreamOp::Messages);
        assert_eq!(
            b,
            json!({"messages": [], "anthropic_version": "bedrock-2023-05-31", "max_tokens": 4096, "anthropic_beta": ["op"]})
        );
        assert!(h.get("anthropic-beta").is_none());
        let (a, h) = body("a", UpstreamOp::Messages);
        assert_eq!(
            a,
            json!({"model": "claude-up", "stream": true, "messages": [], "max_tokens": 100})
        );
        assert_eq!(h["anthropic-version"], "2023-06-01");
        let untouched = Bytes::from_static(br#"{"model": "claude-up", "max_tokens": 1}"#);
        let mut c = call(UpstreamOp::Messages, false, b"", &rid);
        c.body = untouched.clone().into();
        let r = t.deployments["a"].build_request(c).unwrap().request;
        assert_eq!(r.body, untouched.into());
    }

    #[test]
    fn embedding_rerank_and_realtime_paths() {
        let t = table(json!({
            "providers": {
                "g": {"type": "gemini", "base_url": "http://h/g", "auth": {"api_key": "k"}},
                "v": {"type": "vertex", "base_url": "http://h/v", "auth": {"method": "api_key", "api_key": "k"}},
                "c": {"type": "cohere", "base_url": "http://h/c", "auth": {"api_key": "k"}},
                "b": {"type": "bedrock", "aws": {"region": "eu-west-1"}, "auth": {"method": "api_key", "api_key": "k"}},
                "o": {"type": "openai", "base_url": "http://h/o/v1", "auth": {"api_key": "k"}},
                "z": {"type": "azure_openai", "base_url": "http://h/z", "auth": {"api_key": "k"}},
                "zd": {"type": "azure_openai", "base_url": "http://h/zd", "auth": {"api_key": "k"},
                       "azure": {"api_style": "deployments", "api_version": "2025-04-01"}},
            },
            "deployments": {
                "g": {"provider": "g", "model": "text-embedding-004"},
                "v": {"provider": "v", "model": "text-embedding-005"},
                "c": {"provider": "c", "model": "embed-v4.0"},
                "b": {"provider": "b", "model": "amazon.rerank-v1:0"},
                "o": {"provider": "o", "model": "gpt-rt"},
                "z": {"provider": "z", "model": "rt-dep"},
                "zd": {"provider": "zd", "model": "rt-dep"},
            }
        }));
        let rid = RequestId::generate();
        let uri = |d: &str, op| {
            t.deployments[d]
                .build_request(call(op, false, b"{}", &rid))
                .unwrap()
                .request
                .uri
                .to_string()
        };
        assert_eq!(
            uri("g", UpstreamOp::GeminiEmbed { batch: false }),
            "http://h/g/v1beta/models/text-embedding-004:embedContent"
        );
        assert_eq!(
            uri("g", UpstreamOp::GeminiEmbed { batch: true }),
            "http://h/g/v1beta/models/text-embedding-004:batchEmbedContents"
        );
        assert_eq!(
            uri("v", UpstreamOp::VertexPredict),
            "http://h/v/v1/publishers/google/models/text-embedding-005:predict"
        );
        assert_eq!(uri("c", UpstreamOp::CohereEmbed), "http://h/c/v2/embed");
        assert_eq!(uri("c", UpstreamOp::CohereRerank), "http://h/c/v2/rerank");
        assert_eq!(
            uri("b", UpstreamOp::BedrockRerank),
            "https://bedrock-agent-runtime.eu-west-1.amazonaws.com/rerank"
        );
        assert_eq!(
            uri("o", UpstreamOp::Realtime),
            "http://h/o/v1/realtime?model=gpt-rt"
        );
        assert_eq!(
            uri("z", UpstreamOp::Realtime),
            "http://h/z/openai/v1/realtime?model=rt-dep"
        );
        assert_eq!(
            uri("zd", UpstreamOp::Realtime),
            "http://h/zd/openai/realtime?api-version=2025-04-01&deployment=rt-dep"
        );
        assert_eq!(
            t.deployments["c"]
                .build_request(call(UpstreamOp::Realtime, false, b"{}", &rid))
                .err(),
            Some(BuildError::Unsupported)
        );
    }
}
