//! The request corpus: one entry per request shape of the scenario matrix, each with the request sent
//! through the gateway and the equivalent request sent straight to the mock.

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::prng::SplitMix64;

/// One HTTP request of the corpus.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Template {
    pub path: String,
    pub headers: Vec<(String, String)>,
    /// The exact body text.
    pub body: String,
}

/// A corpus entry: the request through the gateway and its direct-to-mock counterpart.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub entry: String,
    pub seed: u64,
    pub gateway: Template,
    pub direct: Template,
}

/// The model name the gateway profiles route to the OpenAI-dialect deployment.
pub const CHAT_MODEL: &str = "bench-chat";
/// The model name routed to the Anthropic Messages deployment.
pub const MESSAGES_MODEL: &str = "bench-messages";
/// The embeddings model.
pub const EMBED_MODEL: &str = "bench-embed";
/// The upstream model id of every deployment.
pub const UPSTREAM_MODEL: &str = "bench-upstream";
/// The MCP tool every bench server exposes, and its name through the gateway's namespacing.
pub const MCP_TOOL: &str = "echo";
pub const MCP_GATEWAY_TOOL: &str = "a__echo";
/// The legacy protocol version the generator initializes with, and the stateless one.
pub const MCP_LEGACY_VERSION: &str = "2025-06-18";
pub const MCP_MODERN_VERSION: &str = "2026-07-28";

/// Every entry name that is committed under `corpus/data/`.
pub const COMMITTED: &[&str] = &[
    "chat_small",
    "chat_large",
    "chat_stream",
    "messages",
    "messages_stream",
    "embeddings",
    "mcp_call_legacy",
    "mcp_call_modern",
];

const JSON: (&str, &str) = ("content-type", "application/json");

fn header(pair: (&str, &str)) -> (String, String) {
    (pair.0.to_owned(), pair.1.to_owned())
}

/// Lowercase words separated by spaces, exactly `len` bytes; JSON-safe without escaping.
fn filler(rng: &mut SplitMix64, len: usize) -> String {
    let mut out = String::with_capacity(len);
    while out.len() < len {
        let word = 2 + rng.below(9);
        for _ in 0..word {
            if out.len() == len {
                break;
            }
            let c = b'a' + u8::try_from(rng.below(26)).unwrap_or(0);
            out.push(char::from(c));
        }
        if out.len() < len {
            out.push(' ');
        }
    }
    out
}

/// A JSON body whose `"content"` string is padded so the whole body is exactly `size` bytes.
fn sized(rng: &mut SplitMix64, size: usize, shape: impl Fn(&str) -> String) -> String {
    let empty = shape("").len();
    shape(&filler(rng, size.saturating_sub(empty)))
}

fn chat_body(rng: &mut SplitMix64, model: &str, size: usize, stream: bool) -> String {
    let opts = if stream {
        r#","stream":true,"stream_options":{"include_usage":true}"#
    } else {
        r#","stream":false"#
    };
    sized(rng, size, |content| {
        format!(
            r#"{{"model":"{model}","max_tokens":256{opts},"messages":[{{"role":"user","content":"{content}"}}]}}"#
        )
    })
}

fn messages_body(content: &str, stream: bool) -> String {
    let opt = if stream { r#","stream":true"# } else { "" };
    format!(
        r#"{{"model":"{UPSTREAM_MODEL}","max_tokens":256{opt},"messages":[{{"role":"user","content":"{content}"}}]}}"#
    )
}

fn content_of(body: &str) -> &str {
    let start = body.find(r#""content":""#).map_or(0, |i| i + 11);
    let end = body.rfind(r#""}]"#).unwrap_or(body.len());
    body.get(start..end).unwrap_or_default()
}

fn chat(seed: u64, entry: &str, size: usize, stream: bool) -> Entry {
    let mut rng = SplitMix64::named(seed, entry);
    let body = chat_body(&mut rng, CHAT_MODEL, size, stream);
    let direct = body.replacen(CHAT_MODEL, UPSTREAM_MODEL, 1);
    let path = "/v1/chat/completions".to_owned();
    Entry {
        entry: entry.to_owned(),
        seed,
        gateway: Template {
            path: path.clone(),
            headers: vec![header(JSON)],
            body,
        },
        direct: Template {
            path,
            headers: vec![
                header(JSON),
                header(("authorization", "Bearer sk-bench-upstream")),
            ],
            body: direct,
        },
    }
}

fn messages(seed: u64, entry: &str, stream: bool) -> Entry {
    let mut rng = SplitMix64::named(seed, entry);
    let body = chat_body(&mut rng, MESSAGES_MODEL, 1024, stream);
    let direct = messages_body(content_of(&body), stream);
    Entry {
        entry: entry.to_owned(),
        seed,
        gateway: Template {
            path: "/v1/chat/completions".to_owned(),
            headers: vec![header(JSON)],
            body,
        },
        direct: Template {
            path: "/v1/messages".to_owned(),
            headers: vec![
                header(JSON),
                header(("x-api-key", "sk-bench-upstream")),
                header(("anthropic-version", "2023-06-01")),
            ],
            body: direct,
        },
    }
}

fn embeddings(seed: u64) -> Entry {
    let mut rng = SplitMix64::named(seed, "embeddings");
    let inputs: Vec<String> = (0..16).map(|_| filler(&mut rng, 256)).collect();
    let body = |model: &str| json!({"model": model, "input": inputs}).to_string();
    Entry {
        entry: "embeddings".to_owned(),
        seed,
        gateway: Template {
            path: "/v1/embeddings".to_owned(),
            headers: vec![header(JSON)],
            body: body(EMBED_MODEL),
        },
        direct: Template {
            path: "/v1/embeddings".to_owned(),
            headers: vec![
                header(JSON),
                header(("authorization", "Bearer sk-bench-upstream")),
            ],
            body: body(UPSTREAM_MODEL),
        },
    }
}

fn mcp_call(seed: u64, entry: &str, modern: bool) -> Entry {
    let mut rng = SplitMix64::named(seed, entry);
    let text = filler(&mut rng, 512);
    let body = |tool: &str| {
        let mut params = json!({"name": tool, "arguments": {"text": text}});
        if modern {
            params["_meta"] = json!({
                "io.modelcontextprotocol/protocolVersion": MCP_MODERN_VERSION,
                "io.modelcontextprotocol/clientCapabilities": {},
                "io.modelcontextprotocol/clientInfo": {"name": "antirion-bench", "version": "1.0.0"},
            });
        }
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": params}).to_string()
    };
    let headers = |tool: &str| {
        let mut h = vec![
            header(JSON),
            header(("accept", "application/json, text/event-stream")),
        ];
        if modern {
            h.push(header(("mcp-protocol-version", MCP_MODERN_VERSION)));
            h.push(header(("mcp-method", "tools/call")));
            h.push(("mcp-name".to_owned(), tool.to_owned()));
        } else {
            h.push(header(("mcp-protocol-version", MCP_LEGACY_VERSION)));
        }
        h
    };
    Entry {
        entry: entry.to_owned(),
        seed,
        gateway: Template {
            path: "/mcp/g".to_owned(),
            headers: headers(MCP_GATEWAY_TOOL),
            body: body(MCP_GATEWAY_TOOL),
        },
        direct: Template {
            path: "/mcp".to_owned(),
            headers: headers(MCP_TOOL),
            body: body(MCP_TOOL),
        },
    }
}

/// The entry named `name` under `seed`. `chat_body_<bytes>` is a non-streaming chat request of that
/// many bytes (the large-body cells), generated on demand rather than committed.
#[must_use]
pub fn generate(seed: u64, name: &str) -> Option<Entry> {
    Some(match name {
        "chat_small" => chat(seed, name, 1024, false),
        "chat_large" => chat(seed, name, 100 * 1024, false),
        "chat_stream" => chat(seed, name, 1024, true),
        "messages" => messages(seed, name, false),
        "messages_stream" => messages(seed, name, true),
        "embeddings" => embeddings(seed),
        "mcp_call_legacy" => mcp_call(seed, name, false),
        "mcp_call_modern" => mcp_call(seed, name, true),
        other => {
            let size: usize = other.strip_prefix("chat_body_")?.parse().ok()?;
            chat(seed, name, size, false)
        }
    })
}

/// An MCP `initialize` request for a legacy session.
#[must_use]
pub fn mcp_initialize() -> String {
    json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
        "protocolVersion": MCP_LEGACY_VERSION,
        "capabilities": {},
        "clientInfo": {"name": "antirion-bench", "version": "1.0.0"},
    }})
    .to_string()
}

/// The `notifications/initialized` message that completes a legacy session's handshake.
#[must_use]
pub fn mcp_initialized() -> String {
    json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_match_the_matrix() {
        let s1 = generate(1, "chat_small").unwrap();
        assert_eq!(s1.gateway.body.len(), 1024);
        assert_eq!(generate(1, "chat_large").unwrap().gateway.body.len(), 100 * 1024);
        assert_eq!(generate(1, "chat_stream").unwrap().gateway.body.len(), 1024);
        assert_eq!(generate(1, "messages").unwrap().gateway.body.len(), 1024);
        assert_eq!(
            generate(1, "chat_body_1048576").unwrap().gateway.body.len(),
            1 << 20
        );
        let emb = generate(1, "embeddings").unwrap();
        let v: serde_json::Value = serde_json::from_str(&emb.gateway.body).unwrap();
        assert_eq!(v["input"].as_array().unwrap().len(), 16);
        assert!(
            v["input"]
                .as_array()
                .unwrap()
                .iter()
                .all(|s| s.as_str().unwrap().len() == 256)
        );
    }

    #[test]
    fn bodies_are_json_and_direct_matches() {
        for name in COMMITTED {
            let e = generate(3, name).unwrap();
            let g: serde_json::Value = serde_json::from_str(&e.gateway.body).unwrap();
            let d: serde_json::Value = serde_json::from_str(&e.direct.body).unwrap();
            assert!(g.is_object() && d.is_object(), "{name}");
        }
        let m = generate(3, "messages_stream").unwrap();
        let g: serde_json::Value = serde_json::from_str(&m.gateway.body).unwrap();
        let d: serde_json::Value = serde_json::from_str(&m.direct.body).unwrap();
        assert_eq!(g["messages"][0]["content"], d["messages"][0]["content"]);
        assert_eq!(d["stream"], true);
    }

    #[test]
    fn unknown_names_are_none() {
        assert!(generate(1, "nope").is_none());
        assert!(generate(1, "chat_body_x").is_none());
    }
}
