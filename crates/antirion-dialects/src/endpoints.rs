//! The data-plane endpoint catalog: which (method, path) is which endpoint, decided from the method
//! and the normalised path only, and which error envelope a path gets.

use antirion_core::Envelope;
use http::Method;

/// A client dialect.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Dialect {
    OpenAiChat,
    OpenAiResponses,
    OpenAiCompletions,
    OpenAiEmbeddings,
    OpenAiImages,
    OpenAiAudio,
    OpenAiModerations,
    OpenAiFiles,
    OpenAiBatches,
    OpenAiRealtime,
    /// Cohere-style rerank.
    Rerank,
    AnthropicMessages,
    Gemini,
    BedrockConverse,
    BedrockInvoke,
    Mcp,
}

/// What an endpoint does with its request.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A model call forwarded to an upstream.
    Inference,
    /// `GET /v1/models` and `GET /v1/models/{id}`, answered locally.
    ModelsList,
    /// A stored Responses object (`GET`/`DELETE /v1/responses/{id}`).
    StoredResponse,
    /// An MCP gateway endpoint.
    Mcp,
    /// An A2A agent: its JSON-RPC endpoint (`id` `a2a`) or its card (`id` `a2a_card`).
    A2a,
    /// RFC 9728 metadata of an MCP gateway, served without authentication; `path_param` is the
    /// resource path after the well-known prefix.
    McpMetadata,
    /// An explicit passthrough mount; `path_param` is the mount's name.
    Passthrough,
    /// `GET /antirion/v1/limits`: the caller's own limits and budgets, answered locally.
    Limits,
    /// The Files API: an upload (`POST /v1/files`), the project's list, or one file by its gateway
    /// id in `path_param`, with `action` `content` for its bytes.
    Files,
    /// The Batches API: a create or the list, or one batch by its gateway id in `path_param`, with
    /// `action` `cancel`.
    Batches,
    /// The Realtime WebSocket endpoint.
    Realtime,
    /// Responses events over a WebSocket (`GET /v1/responses` with an upgrade).
    ResponsesSocket,
    /// `POST /v1/feedback`: feedback on one of the caller's request-log records.
    Feedback,
}

/// An endpoint matched at `accept`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// The endpoint id used in config and logs (`chat_completions`).
    pub id: &'static str,
    /// The endpoint family of access blocks (`chat`).
    pub family: &'static str,
    pub dialect: Dialect,
    pub kind: Kind,
    /// The operation path appended to an OpenAI-family provider's base URL.
    pub upstream_path: Option<&'static str>,
    /// A model or object id carried in the path (for OpenAI inference endpoints, the model of a
    /// legacy Azure or engine path).
    pub path_param: Option<String>,
    /// The action of a native path (`streamGenerateContent`, `converse-stream`, …), or of a
    /// Responses operation on the collection (`input_tokens`, `compact`).
    pub action: Option<&'static str>,
}

impl Endpoint {
    /// Whether the endpoint counts input tokens (Anthropic `count_tokens`, Gemini `countTokens`,
    /// Responses `input_tokens`).
    #[must_use]
    pub fn counts_tokens(&self) -> bool {
        self.id == "count_tokens" || matches!(self.action, Some("countTokens" | "input_tokens"))
    }

    /// Whether the path itself asks for a stream (Gemini and Bedrock native routes).
    #[must_use]
    pub fn path_streams(&self) -> bool {
        matches!(
            self.action,
            Some("streamGenerateContent" | "converse-stream" | "invoke-with-response-stream")
        )
    }
}

/// The result of matching a request line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Matched {
    Found(Endpoint),
    /// The path is known but not for this method; the allowed methods.
    MethodNotAllowed(&'static [Method]),
    /// A known endpoint this build does not serve.
    NotSupported,
    NotFound,
    /// A dot segment, backslash, control character or encoded slash.
    InvalidPath,
}

const POST: &[Method] = &[Method::POST];
const GET: &[Method] = &[Method::GET];
const GET_HEAD: &[Method] = &[Method::GET, Method::HEAD];
const GET_DELETE: &[Method] = &[Method::GET, Method::DELETE];
const MCP_METHODS: &[Method] = &[Method::GET, Method::POST, Method::DELETE];
const GET_POST: &[Method] = &[Method::GET, Method::POST];

fn inference(
    id: &'static str,
    family: &'static str,
    dialect: Dialect,
    upstream: Option<&'static str>,
) -> Endpoint {
    Endpoint {
        id,
        family,
        dialect,
        kind: Kind::Inference,
        upstream_path: upstream,
        path_param: None,
        action: None,
    }
}

/// Normalises a path: collapses repeated `/`, strips one trailing `/` and one provider mount prefix,
/// and maps the aliases without `/v1`. `None` when the path must be rejected.
#[must_use]
pub fn normalise(path: &str) -> Option<String> {
    if canonical(path) {
        Some(path.to_owned())
    } else {
        normalised(path)
    }
}

fn normalised(path: &str) -> Option<String> {
    if path.contains(['\\', '\0']) || path.bytes().any(|b| b.is_ascii_control()) {
        return None;
    }
    let mut segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.iter().any(|s| dot_segment(s) || encoded_control(s)) {
        return None;
    }
    if matches!(
        segments.first(),
        Some(&("openai" | "anthropic" | "gemini" | "bedrock"))
    ) {
        segments.remove(0);
    }
    let bedrock_model = segments.first() == Some(&"model");
    let encoded_slash = |s: &str| {
        let lower = s.to_ascii_lowercase();
        lower.contains("%2f") || lower.contains("%5c")
    };
    let model_id = |i: usize| bedrock_model && i == 1;
    if segments
        .iter()
        .enumerate()
        .any(|(i, s)| !model_id(i) && encoded_slash(s))
    {
        return None;
    }
    if matches!(
        segments.first(),
        Some(&("chat" | "embeddings" | "models" | "responses" | "completions"))
    ) {
        segments.insert(0, "v1");
    }
    Some(format!("/{}", segments.join("/")))
}

/// A path `normalise` returns unchanged: printable ASCII without `%` or `\\`, no empty, `.` or
/// `..` segment, no trailing `/`, and no mount prefix or alias as its first segment.
fn canonical(path: &str) -> bool {
    let Some(rest) = path.strip_prefix('/') else {
        return false;
    };
    if rest.is_empty()
        || rest.ends_with('/')
        || !rest
            .bytes()
            .all(|b| b.is_ascii_graphic() && !matches!(b, b'%' | b'\\'))
    {
        return false;
    }
    let first = rest.split('/').next().unwrap_or_default();
    !matches!(
        first,
        "openai"
            | "anthropic"
            | "gemini"
            | "bedrock"
            | "chat"
            | "embeddings"
            | "models"
            | "responses"
            | "completions"
    ) && rest.split('/').all(|s| !s.is_empty() && s != "." && s != "..")
}

/// A segment that is `.` or `..`, literally or percent-encoded.
fn dot_segment(s: &str) -> bool {
    let decoded = s.replace("%2e", ".").replace("%2E", ".");
    decoded == "." || decoded == ".."
}

/// A segment carrying a percent-encoded control character.
fn encoded_control(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.windows(3).any(|w| {
        w.first() == Some(&b'%')
            && std::str::from_utf8(w.get(1..).unwrap_or_default())
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .is_some_and(|b| b < 0x20 || b == 0x7f)
    })
}

/// The error envelope of a raw path, decided from the path alone.
#[must_use]
pub fn envelope_for_path(raw: &str) -> Envelope {
    let norm = normalise(raw).unwrap_or_else(|| raw.to_owned());
    if norm.starts_with("/v1/messages") || raw.starts_with("/anthropic/") {
        Envelope::Anthropic
    } else if norm.starts_with("/v1beta/")
        || raw.starts_with("/gemini/")
        || [
            ":generateContent",
            ":streamGenerateContent",
            ":countTokens",
            ":embedContent",
            ":batchEmbedContents",
        ]
        .iter()
        .any(|m| raw.contains(m))
    {
        Envelope::Gemini
    } else if raw.starts_with("/model/") || raw.starts_with("/bedrock/") {
        Envelope::Bedrock
    } else if norm == "/mcp" || norm.starts_with("/mcp/") || norm.starts_with("/a2a/") {
        Envelope::JsonRpc
    } else {
        Envelope::OpenAi
    }
}

/// The endpoint of the passthrough mount `name`.
#[must_use]
pub fn passthrough(name: &str) -> Endpoint {
    Endpoint {
        id: "passthrough",
        family: "passthrough",
        dialect: Dialect::OpenAiChat,
        kind: Kind::Passthrough,
        upstream_path: None,
        path_param: Some(name.to_owned()),
        action: None,
    }
}

/// The legacy Azure OpenAI and engine paths: `/openai/deployments/{d}/chat/completions`,
/// `…/completions`, `…/embeddings`, `/engines/{m}/chat/completions`, `/engines/{m}/embeddings` and
/// `/v1/engines/{m}/embeddings`, as the catalog path they stand for and the model they name.
#[must_use]
pub fn legacy_path(raw_path: &str) -> Option<(&'static str, String)> {
    normalise(raw_path)?;
    let segs: Vec<&str> = raw_path.strip_prefix('/')?.split('/').collect();
    let (model, op, azure) = match segs.as_slice() {
        ["openai", "deployments", d, op @ ..] => (*d, op, true),
        ["v1", "engines", m, op @ ..] if op == ["embeddings"] => (*m, op, false),
        ["engines", m, op @ ..] => (*m, op, false),
        _ => return None,
    };
    let path = match op {
        ["chat", "completions"] => "/v1/chat/completions",
        ["completions"] if azure => "/v1/completions",
        ["embeddings"] => "/v1/embeddings",
        _ => return None,
    };
    let model = percent_decode(model);
    if model.is_empty() || model.contains('/') {
        return None;
    }
    Some((path, model))
}

/// The Responses WebSocket endpoint: `GET /v1/responses` with a WebSocket upgrade.
#[must_use]
pub fn responses_socket() -> Endpoint {
    Endpoint {
        id: "responses_websocket",
        family: "responses",
        dialect: Dialect::OpenAiResponses,
        kind: Kind::ResponsesSocket,
        upstream_path: None,
        path_param: None,
        action: None,
    }
}

/// Matches a request against the endpoint catalog.
#[must_use]
pub fn match_endpoint(method: &Method, raw_path: &str) -> Matched {
    let Some(path) = normalise(raw_path) else {
        return Matched::InvalidPath;
    };
    let segs: Vec<&str> = path.split('/').skip(1).collect();
    let (allowed, endpoint): (&'static [Method], Option<Endpoint>) = match segs.as_slice() {
        ["v1", "chat", "completions"] => (
            POST,
            Some(inference(
                "chat_completions",
                "chat",
                Dialect::OpenAiChat,
                Some("/chat/completions"),
            )),
        ),
        ["v1", "completions"] => (
            POST,
            Some(inference(
                "completions",
                "chat",
                Dialect::OpenAiCompletions,
                Some("/completions"),
            )),
        ),
        ["v1", "responses"] => (
            POST,
            Some(inference(
                "responses",
                "responses",
                Dialect::OpenAiResponses,
                Some("/responses"),
            )),
        ),
        ["v1", "responses", op @ ("input_tokens" | "compact")] => {
            let (action, path) = if *op == "compact" {
                ("compact", "/responses/compact")
            } else {
                ("input_tokens", "/responses/input_tokens")
            };
            (
                POST,
                Some(Endpoint {
                    action: Some(action),
                    ..inference("responses", "responses", Dialect::OpenAiResponses, Some(path))
                }),
            )
        }
        ["v1", "responses", id] => (
            GET_DELETE,
            Some(Endpoint {
                id: "responses",
                family: "responses",
                dialect: Dialect::OpenAiResponses,
                kind: Kind::StoredResponse,
                upstream_path: None,
                path_param: Some((*id).to_owned()),
                action: None,
            }),
        ),
        ["v1", "embeddings"] => (
            POST,
            Some(inference(
                "embeddings",
                "embeddings",
                Dialect::OpenAiEmbeddings,
                Some("/embeddings"),
            )),
        ),
        ["v1", "images", "generations"] => (
            POST,
            Some(inference(
                "images",
                "images",
                Dialect::OpenAiImages,
                Some("/images/generations"),
            )),
        ),
        ["v1", "audio", op @ ("speech" | "transcriptions" | "translations")] => {
            let upstream = match *op {
                "speech" => "/audio/speech",
                "transcriptions" => "/audio/transcriptions",
                _ => "/audio/translations",
            };
            (
                POST,
                Some(inference("audio", "audio", Dialect::OpenAiAudio, Some(upstream))),
            )
        }
        ["v1", "messages"] => (
            POST,
            Some(inference(
                "messages",
                "messages",
                Dialect::AnthropicMessages,
                None,
            )),
        ),
        ["v1", "messages", "count_tokens"] => (
            POST,
            Some(inference(
                "count_tokens",
                "messages",
                Dialect::AnthropicMessages,
                None,
            )),
        ),
        ["v1beta" | "v1", "models", "models", last] | ["v1beta" | "v1", "models", last]
            if gemini_action(last).is_some() =>
        {
            let (model, action) = gemini_action(last).unwrap_or_default();
            let id = if matches!(action, "generateContent" | "streamGenerateContent") {
                "gemini_generate"
            } else {
                "gemini"
            };
            let mut e = inference(id, "gemini", Dialect::Gemini, None);
            e.path_param = Some(model.to_owned());
            e.action = Some(action);
            (POST, Some(e))
        }
        ["v1" | "v1beta", "models"] => (GET_HEAD, Some(models_endpoint(None))),
        ["v1", "models", rest @ ..] => (GET_HEAD, Some(models_endpoint(Some(&rest.join("/"))))),
        ["v1", "moderations"] => (
            POST,
            Some(inference(
                "moderations",
                "moderations",
                Dialect::OpenAiModerations,
                Some("/moderations"),
            )),
        ),
        [
            "model",
            model,
            action @ ("converse" | "converse-stream" | "invoke" | "invoke-with-response-stream"),
        ] => {
            let (id, dialect) = if action.starts_with("converse") {
                ("bedrock_converse", Dialect::BedrockConverse)
            } else {
                ("bedrock_invoke", Dialect::BedrockInvoke)
            };
            let mut e = inference(id, "bedrock", dialect, None);
            e.path_param = Some(percent_decode(model));
            e.action = Some(match *action {
                "converse" => "converse",
                "converse-stream" => "converse-stream",
                "invoke" => "invoke",
                _ => "invoke-with-response-stream",
            });
            (POST, Some(e))
        }
        ["antirion", "v1", "limits"] => (
            GET_HEAD,
            Some(Endpoint {
                id: "limits",
                family: "models",
                dialect: Dialect::OpenAiChat,
                kind: Kind::Limits,
                upstream_path: None,
                path_param: None,
                action: None,
            }),
        ),
        ["v1", "feedback"] => (
            POST,
            Some(Endpoint {
                id: "feedback",
                family: "feedback",
                dialect: Dialect::OpenAiChat,
                kind: Kind::Feedback,
                upstream_path: None,
                path_param: None,
                action: None,
            }),
        ),
        ["a2a", agent] => (POST, Some(a2a("a2a", agent))),
        ["a2a", agent, ".well-known", "agent-card.json" | "agent.json"] => {
            (GET, Some(a2a("a2a_card", agent)))
        }
        [".well-known", "oauth-protected-resource", "mcp", rest @ ..] if rest.len() <= 1 => (
            GET,
            Some(Endpoint {
                id: "mcp_metadata",
                family: "mcp",
                dialect: Dialect::Mcp,
                kind: Kind::McpMetadata,
                upstream_path: None,
                path_param: Some(format!(
                    "/mcp{}",
                    rest.iter().map(|r| format!("/{r}")).collect::<String>()
                )),
                action: None,
            }),
        ),
        [".well-known", "oauth-authorization-server", rest @ ..] if rest.len() <= 2 => (
            GET,
            Some(Endpoint {
                id: "mcp_authorization_server",
                family: "mcp",
                dialect: Dialect::Mcp,
                kind: Kind::McpMetadata,
                upstream_path: None,
                path_param: Some(rest.iter().map(|r| format!("/{r}")).collect::<String>()),
                action: None,
            }),
        ),
        ["mcp", gateway, "register"] => (
            POST,
            Some(Endpoint {
                id: "mcp_register",
                family: "mcp",
                dialect: Dialect::Mcp,
                kind: Kind::McpMetadata,
                upstream_path: None,
                path_param: Some((*gateway).to_owned()),
                action: None,
            }),
        ),
        ["mcp", gateway, which @ ("sse" | "message")] => (
            if *which == "sse" { GET } else { POST },
            Some(Endpoint {
                id: if *which == "sse" { "mcp_sse" } else { "mcp_message" },
                family: "mcp",
                dialect: Dialect::Mcp,
                kind: Kind::Mcp,
                upstream_path: None,
                path_param: Some((*gateway).to_owned()),
                action: None,
            }),
        ),
        ["mcp"] | ["mcp", _] => (
            MCP_METHODS,
            Some(Endpoint {
                id: "mcp",
                family: "mcp",
                dialect: Dialect::Mcp,
                kind: Kind::Mcp,
                upstream_path: None,
                path_param: segs.get(1).map(|s| (*s).to_owned()),
                action: None,
            }),
        ),
        ["v1", "images", op @ ("edits" | "variations")] => (
            POST,
            Some(inference(
                "images",
                "images",
                Dialect::OpenAiImages,
                Some(if *op == "edits" {
                    "/images/edits"
                } else {
                    "/images/variations"
                }),
            )),
        ),
        ["v1" | "v2", "rerank"] => (
            POST,
            Some(inference("rerank", "rerank", Dialect::Rerank, Some("/rerank"))),
        ),
        ["v1", "files"] => (GET_POST, Some(resource(Kind::Files, None, None))),
        ["v1", "files", id] => (GET_DELETE, Some(resource(Kind::Files, Some(id), None))),
        ["v1", "files", id, "content"] => (GET, Some(resource(Kind::Files, Some(id), Some("content")))),
        ["v1", "batches"] => (GET_POST, Some(resource(Kind::Batches, None, None))),
        ["v1", "batches", id] => (GET, Some(resource(Kind::Batches, Some(id), None))),
        ["v1", "batches", id, "cancel"] => (POST, Some(resource(Kind::Batches, Some(id), Some("cancel")))),
        ["v1", "realtime"] => (
            GET,
            Some(Endpoint {
                id: "realtime",
                family: "realtime",
                dialect: Dialect::OpenAiRealtime,
                kind: Kind::Realtime,
                upstream_path: Some("/realtime"),
                path_param: None,
                action: None,
            }),
        ),
        [
            "v1",
            "files" | "assistants" | "threads" | "vector_stores" | "fine_tuning" | "uploads" | "evals"
            | "containers" | "batches",
            ..,
        ]
        | ["v1", "responses", _, ..]
        | ["v1", "messages", "batches", ..] => return Matched::NotSupported,
        _ => return Matched::NotFound,
    };
    match endpoint {
        Some(e) if allowed.contains(method) => Matched::Found(e),
        _ => Matched::MethodNotAllowed(allowed),
    }
}

/// Decodes `%XX` escapes once; invalid escapes are kept as they are.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%'
            && let Some(hex) = bytes.get(i + 1..i + 3)
            && let Ok(hex) = std::str::from_utf8(hex)
            && let Ok(v) = u8::from_str_radix(hex, 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A Files or Batches endpoint: the collection, or one object by its gateway id.
fn resource(kind: Kind, id: Option<&&str>, action: Option<&'static str>) -> Endpoint {
    let (name, dialect, upstream) = if kind == Kind::Files {
        ("files", Dialect::OpenAiFiles, "/files")
    } else {
        ("batches", Dialect::OpenAiBatches, "/batches")
    };
    Endpoint {
        id: name,
        family: name,
        dialect,
        kind,
        upstream_path: Some(upstream),
        path_param: id.map(|s| (*s).to_owned()),
        action,
    }
}

fn a2a(id: &'static str, agent: &str) -> Endpoint {
    Endpoint {
        id,
        family: "a2a",
        dialect: Dialect::Mcp,
        kind: Kind::A2a,
        upstream_path: None,
        path_param: Some(agent.to_owned()),
        action: None,
    }
}

fn models_endpoint(id: Option<&str>) -> Endpoint {
    Endpoint {
        id: "models",
        family: "models",
        dialect: Dialect::OpenAiChat,
        kind: Kind::ModelsList,
        upstream_path: None,
        path_param: id.map(str::to_owned),
        action: None,
    }
}

fn gemini_action(segment: &str) -> Option<(&str, &'static str)> {
    let (model, action) = segment.rsplit_once(':')?;
    let action = match action {
        "generateContent" => "generateContent",
        "streamGenerateContent" => "streamGenerateContent",
        "countTokens" => "countTokens",
        "embedContent" => "embedContent",
        "batchEmbedContents" => "batchEmbedContents",
        _ => return None,
    };
    Some((model, action))
}

#[cfg(test)]
mod legacy_tests {
    use super::legacy_path;

    #[test]
    fn legacy_paths_name_the_endpoint_and_the_model() {
        let ok = [
            (
                "/openai/deployments/gpt-x/chat/completions",
                "/v1/chat/completions",
                "gpt-x",
            ),
            (
                "/openai/deployments/gpt-x/completions",
                "/v1/completions",
                "gpt-x",
            ),
            ("/openai/deployments/gpt-x/embeddings", "/v1/embeddings", "gpt-x"),
            ("/engines/gpt-x/chat/completions", "/v1/chat/completions", "gpt-x"),
            ("/engines/gpt-x/embeddings", "/v1/embeddings", "gpt-x"),
            ("/v1/engines/gpt-x/embeddings", "/v1/embeddings", "gpt-x"),
        ];
        for (raw, path, model) in ok {
            assert_eq!(legacy_path(raw), Some((path, model.to_owned())), "{raw}");
        }
        for raw in [
            "/v1/engines/gpt-x/chat/completions",
            "/engines/gpt-x/completions",
            "/openai/deployments/gpt-x/images/generations",
            "/openai/deployments//chat/completions",
            "/v1/chat/completions",
        ] {
            assert_eq!(legacy_path(raw), None, "{raw}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Matched {
        fn map_kind(&self) -> Option<Kind> {
            match self {
                Matched::Found(e) => Some(e.kind),
                _ => None,
            }
        }
    }

    fn found(method: Method, path: &str) -> Endpoint {
        match match_endpoint(&method, path) {
            Matched::Found(e) => e,
            other => panic!("{path}: {other:?}"),
        }
    }

    #[test]
    fn normalisation() {
        assert_eq!(
            normalise("//v1//chat/completions/").as_deref(),
            Some("/v1/chat/completions")
        );
        assert_eq!(
            normalise("/anthropic/v1/messages").as_deref(),
            Some("/v1/messages")
        );
        assert_eq!(
            normalise("/chat/completions").as_deref(),
            Some("/v1/chat/completions")
        );
        assert_eq!(normalise("/v1/../admin"), None);
        for p in [
            "/v1/chat/completions",
            "/v1/messages",
            "/v1beta/models/gemini-pro:generateContent",
            "/model/m/converse",
            "/v1/a.b/c..d/.e",
            "/mcp",
            "/-/readyz",
        ] {
            assert!(canonical(p), "{p}");
            assert_eq!(normalised(p).as_deref(), Some(p));
        }
        for p in [
            "/",
            "/v1/",
            "//v1",
            "/v1//x",
            "/chat/completions",
            "/openai/v1/x",
            "/v1/./x",
            "/v1/x%41",
            "/v1/a b",
            "v1/x",
        ] {
            assert!(!canonical(p), "{p}");
        }
        assert_eq!(normalise("/v1/%2e%2E/admin"), None);
        assert_eq!(normalise("/v1/chat%0d%0a/completions"), None);
        assert_eq!(normalise("/v1/files%2Fx"), None);
        assert!(normalise("/model/arn%3Ax%2Fy/converse").is_some());
        assert_eq!(normalise("/model/m/converse%2Fx"), None);
    }

    #[test]
    fn catalog() {
        assert_eq!(found(Method::POST, "/v1/chat/completions").id, "chat_completions");
        assert_eq!(found(Method::GET, "/v1/models").kind, Kind::ModelsList);
        let g = found(Method::POST, "/v1beta/models/gemini-x:generateContent");
        assert_eq!(
            (g.id, g.path_param.as_deref()),
            ("gemini_generate", Some("gemini-x"))
        );
        assert_eq!(found(Method::POST, "/model/m/converse").id, "bedrock_converse");
        assert_eq!(
            found(Method::POST, "/model/arn%3Aa%2Fb/converse")
                .path_param
                .as_deref(),
            Some("arn:a/b")
        );
        assert_eq!(found(Method::POST, "/mcp/x").path_param.as_deref(), Some("x"));
        assert_eq!(found(Method::GET, "/mcp/x/sse").id, "mcp_sse");
        assert_eq!(found(Method::POST, "/mcp/x/message").id, "mcp_message");
        assert_eq!(found(Method::POST, "/mcp/x/register").kind, Kind::McpMetadata);
        assert_eq!(found(Method::POST, "/v1/feedback").kind, Kind::Feedback);
        assert_eq!(
            found(Method::GET, "/.well-known/oauth-authorization-server/mcp/g")
                .path_param
                .as_deref(),
            Some("/mcp/g")
        );
        assert_eq!(found(Method::POST, "/a2a/w").id, "a2a");
        assert_eq!(found(Method::GET, "/a2a/w/.well-known/agent.json").id, "a2a_card");
        assert_eq!(
            found(Method::GET, "/.well-known/oauth-protected-resource/mcp/g")
                .path_param
                .as_deref(),
            Some("/mcp/g")
        );
        assert_eq!(
            match_endpoint(&Method::GET, "/.well-known/oauth-protected-resource"),
            Matched::NotFound
        );
        assert_eq!(
            match_endpoint(&Method::GET, "/v1/chat/completions"),
            Matched::MethodNotAllowed(POST)
        );
        assert_eq!(match_endpoint(&Method::GET, "/health"), Matched::NotFound);
        assert_eq!(
            match_endpoint(&Method::GET, "/admin/v1/status"),
            Matched::NotFound
        );
        assert_eq!(found(Method::POST, "/v1/moderations").id, "moderations");
        assert_eq!(
            found(Method::GET, "/v1/models/openai/gpt-x")
                .path_param
                .as_deref(),
            Some("openai/gpt-x")
        );
        assert_eq!(
            match_endpoint(&Method::POST, "/v1/files").map_kind(),
            Some(Kind::Files)
        );
        let content = found(Method::GET, "/v1/files/file-ant_x/content");
        assert_eq!(
            (content.path_param.as_deref(), content.action),
            (Some("file-ant_x"), Some("content"))
        );
        assert_eq!(found(Method::POST, "/v1/batches/b/cancel").action, Some("cancel"));
        assert_eq!(
            match_endpoint(&Method::DELETE, "/v1/batches/b"),
            Matched::MethodNotAllowed(GET)
        );
        assert_eq!(found(Method::POST, "/v2/rerank").dialect, Dialect::Rerank);
        assert_eq!(
            found(Method::POST, "/v1/images/variations").upstream_path,
            Some("/images/variations")
        );
        assert_eq!(found(Method::GET, "/v1/realtime").kind, Kind::Realtime);
        assert_eq!(
            match_endpoint(&Method::POST, "/v1/files/a/b/c"),
            Matched::NotSupported
        );
    }

    #[test]
    fn envelopes() {
        assert_eq!(envelope_for_path("/v1/messages"), Envelope::Anthropic);
        assert_eq!(envelope_for_path("/anthropic/v1/whatever"), Envelope::Anthropic);
        assert_eq!(
            envelope_for_path("/v1/models/x:generateContent"),
            Envelope::Gemini
        );
        assert_eq!(envelope_for_path("/model/x/no-such-action"), Envelope::Bedrock);
        assert_eq!(envelope_for_path("/mcp/x"), Envelope::JsonRpc);
        assert_eq!(
            envelope_for_path("/a2a/x/.well-known/agent.json"),
            Envelope::JsonRpc
        );
        assert_eq!(envelope_for_path("/v1/does-not-exist"), Envelope::OpenAi);
    }
}
