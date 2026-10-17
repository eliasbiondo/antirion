//! What the admission estimate reads from a request body: its text, messages, tool definitions,
//! media parts, explicit output caps and choice count, in every dialect.

use antirion_dialects::{Dialect, Endpoint};
use antirion_limits::estimate::{CapOutOfRange, MediaCounts, validate_caps};
use serde_json::Value;

/// Members whose string values are prompt text.
const TEXT_KEYS: &[&str] = &["content", "text", "input", "prompt", "system", "instructions"];

/// The output cap fields of an endpoint's dialect, as paths, and its choice count field.
fn cap_fields(
    endpoint: &Endpoint,
) -> (
    &'static [&'static [&'static str]],
    Option<&'static [&'static str]>,
) {
    match endpoint.dialect {
        Dialect::OpenAiChat => (&[&["max_tokens"], &["max_completion_tokens"]], Some(&["n"])),
        Dialect::OpenAiCompletions => (&[&["max_tokens"]], Some(&["n"])),
        Dialect::OpenAiResponses => (&[&["max_output_tokens"]], None),
        Dialect::AnthropicMessages => (&[&["max_tokens"]], None),
        Dialect::Gemini => (
            &[&["generationConfig", "maxOutputTokens"]],
            Some(&["generationConfig", "candidateCount"]),
        ),
        Dialect::BedrockConverse => (&[&["inferenceConfig", "maxTokens"]], None),
        Dialect::BedrockInvoke => (&[&["max_tokens"], &["max_tokens_to_sample"]], None),
        _ => (&[], None),
    }
}

fn at<'v>(body: &'v Value, path: &[&str]) -> Option<&'v Value> {
    path.iter().try_fold(body, |v, k| v.get(*k))
}

/// The largest explicit output cap of the request, after checking every cap and the choice count
/// against their ranges.
pub(crate) fn output_cap(
    endpoint: &Endpoint,
    body: &Value,
    ceiling: u64,
) -> Result<Option<u64>, CapOutOfRange> {
    let (caps, choices) = cap_fields(endpoint);
    let named: Vec<(String, &Value)> = caps
        .iter()
        .filter_map(|p| at(body, p).map(|v| (p.join("."), v)))
        .collect();
    let refs: Vec<(&str, &Value)> = named.iter().map(|(n, v)| (n.as_str(), *v)).collect();
    let choice = choices.and_then(|p| at(body, p).map(|v| (p.join("."), v)));
    validate_caps(&refs, choice.as_ref().map(|(n, v)| (n.as_str(), *v)), ceiling)
}

/// The output cap the upstream will apply: the smallest explicit cap of the client's dialect
/// (Chat's `max_tokens` and `max_completion_tokens` together: the smaller wins).
pub(crate) fn requested_output(endpoint: &Endpoint, body: &Value) -> Option<u64> {
    cap_fields(endpoint)
        .0
        .iter()
        .filter_map(|p| at(body, p).and_then(Value::as_u64))
        .min()
}

/// Caps every output-cap field of the body at `cap`, adding the dialect's main field when the body
/// has none: `(changed, lowered)`, whether the body changed and whether a value the client sent was
/// lowered.
pub(crate) fn clamp(endpoint: &Endpoint, body: &mut Value, cap: u64) -> (bool, bool) {
    let fields = cap_fields(endpoint).0;
    let mut present = false;
    let mut lowered = false;
    for path in fields {
        let Some(slot) = at_mut(body, path) else {
            continue;
        };
        present = true;
        if slot.as_u64().is_some_and(|v| v > cap) {
            *slot = Value::from(cap);
            lowered = true;
        }
    }
    if present {
        return (lowered, lowered);
    }
    let Some((last, parents)) = fields.first().and_then(|p| p.split_last()) else {
        return (false, false);
    };
    let mut cur = body;
    for key in parents {
        let Some(obj) = cur.as_object_mut() else {
            return (false, false);
        };
        cur = obj
            .entry((*key).to_owned())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
    }
    match cur.as_object_mut() {
        Some(obj) => {
            obj.insert((*last).to_owned(), Value::from(cap));
            (true, false)
        }
        None => (false, false),
    }
}

fn at_mut<'v>(body: &'v mut Value, path: &[&str]) -> Option<&'v mut Value> {
    path.iter().try_fold(body, |v, k| v.get_mut(*k))
}

/// The number of choices requested (`n`, `candidateCount`).
pub(crate) fn choices(endpoint: &Endpoint, body: &Value) -> u64 {
    cap_fields(endpoint)
        .1
        .and_then(|p| at(body, p))
        .and_then(Value::as_u64)
        .unwrap_or(1)
}

/// Whether the endpoint generates output tokens.
pub(crate) fn generates(endpoint: &Endpoint) -> bool {
    !matches!(
        endpoint.dialect,
        Dialect::OpenAiEmbeddings
            | Dialect::OpenAiModerations
            | Dialect::Rerank
            | Dialect::OpenAiFiles
            | Dialect::OpenAiBatches
    ) && !endpoint.id.contains("count_tokens")
        && !matches!(endpoint.action, Some("input_tokens" | "compact"))
        && !endpoint.id.to_ascii_lowercase().contains("embed")
        && !endpoint.id.to_ascii_lowercase().contains("counttokens")
}

/// The facts of a body the estimator reads.
#[derive(Debug, Default)]
pub(crate) struct BodyFacts<'a> {
    /// The prompt texts, borrowed from the body.
    pub(crate) texts: Vec<&'a str>,
    pub(crate) messages: u64,
    pub(crate) tools: Option<String>,
    /// Bytes of inline media payloads (excluded from the upper-bound estimate).
    pub(crate) media_bytes: u64,
    pub(crate) media: MediaCounts,
    pub(crate) cache_markers: bool,
    pub(crate) audio_in: bool,
    pub(crate) image_in: bool,
    pub(crate) video_in: bool,
    /// The base64 payloads of inline images, and whether each asked for `detail: low`.
    pub(crate) inline_images: Vec<(&'a str, bool)>,
}

fn is_inline_media(s: &str) -> bool {
    s.starts_with("data:")
}

/// The base64 payload of an inline image part: a `data:` URL, an Anthropic `source.data`, or the
/// `data` of a Gemini `inlineData` part.
fn inline_payload(m: &serde_json::Map<String, Value>) -> Option<&str> {
    let url = match m.get("image_url") {
        Some(Value::String(s)) => Some(s.as_str()),
        Some(Value::Object(o)) => o.get("url").and_then(Value::as_str),
        _ => m.get("url").and_then(Value::as_str),
    };
    if let Some(u) = url {
        return u.strip_prefix("data:")?.split_once(";base64,").map(|(_, d)| d);
    }
    m.get("source")
        .and_then(|s| s.get("data"))
        .or_else(|| m.get("data"))
        .and_then(Value::as_str)
}

fn low_detail(m: &serde_json::Map<String, Value>) -> bool {
    let detail = m
        .get("image_url")
        .and_then(|u| u.get("detail"))
        .or_else(|| m.get("detail"))
        .and_then(Value::as_str);
    detail == Some("low")
}

fn walk<'a>(v: &'a Value, key: Option<&str>, f: &mut BodyFacts<'a>, in_image: bool) {
    match v {
        Value::String(s) if is_inline_media(s) => f.media_bytes += s.len() as u64,
        Value::String(s) if key == Some("data") => f.media_bytes += s.len() as u64,
        Value::String(s) if key.is_some_and(|k| TEXT_KEYS.contains(&k)) => f.texts.push(s.as_str()),
        Value::Array(items) => {
            for i in items {
                walk(i, key, f, in_image);
            }
        }
        Value::Object(m) => {
            if m.contains_key("cache_control") || m.contains_key("cachePoint") {
                f.cache_markers = true;
            }
            let kind = m.get("type").and_then(Value::as_str).unwrap_or_default();
            let mime = m
                .get("mimeType")
                .or_else(|| m.get("mime_type"))
                .or_else(|| m.get("media_type"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let image = matches!(kind, "image_url" | "image" | "input_image") || mime.starts_with("image/");
            if image {
                if !in_image {
                    f.media.images += 1;
                    f.image_in = true;
                    if let Some(payload) = inline_payload(m) {
                        f.inline_images.push((payload, low_detail(m)));
                    }
                }
            } else if matches!(kind, "input_audio" | "audio") || mime.starts_with("audio/") {
                f.media.audio += 1;
                f.audio_in = true;
            } else if matches!(kind, "video" | "input_video") || mime.starts_with("video/") {
                f.media.video += 1;
                f.video_in = true;
            }
            for (k, child) in m {
                if k == "tools" {
                    continue;
                }
                walk(child, Some(k.as_str()), f, in_image || image);
            }
        }
        _ => {}
    }
}

/// The base64 characters decoded to find an image's dimensions: its header is near the start.
const HEADER_B64: usize = 128 * 1024;

fn header_dimensions(payload: &str) -> Option<(u32, u32)> {
    use base64::Engine;
    let end = payload.len().min(HEADER_B64) / 4 * 4;
    let prefix = payload.get(..end)?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(prefix).ok()?;
    antirion_limits::imagetokens::dimensions(&bytes)
}

/// The input tokens of the inline images whose header gives their dimensions: for each, the
/// largest over the candidates' formulas, a candidate without one counting `default`. Returns the
/// tokens and how many images they cover; the others keep `default`.
pub(crate) fn image_tokens(
    images: &[(&str, bool)],
    formulas: &[Option<antirion_limits::imagetokens::ImageFormula>],
    default: u64,
) -> (u64, u64) {
    if formulas.iter().all(Option::is_none) {
        return (0, 0);
    }
    let mut tokens = 0u64;
    let mut counted = 0u64;
    for (payload, low) in images {
        let Some((w, h)) = header_dimensions(payload) else {
            continue;
        };
        let worst = formulas
            .iter()
            .map(|f| f.map_or(default, |f| f.tokens(w, h, *low)))
            .max()
            .unwrap_or(default);
        tokens = tokens.saturating_add(worst);
        counted += 1;
    }
    (tokens, counted)
}

/// Reads the estimator's facts from a JSON request body.
pub(crate) fn body_facts(body: &Value) -> BodyFacts<'_> {
    let mut f = BodyFacts::default();
    walk(body, None, &mut f, false);
    f.messages = ["messages", "contents", "input"]
        .iter()
        .find_map(|k| body.get(*k).and_then(Value::as_array))
        .map_or(0, |a| a.len() as u64);
    f.tools = body
        .get("tools")
        .filter(|t| t.as_array().is_some_and(|a| !a.is_empty()))
        .map(Value::to_string);
    f
}

#[cfg(test)]
mod tests {
    use antirion_dialects::Kind;
    use serde_json::json;

    use super::*;

    fn ep(id: &'static str, dialect: Dialect) -> Endpoint {
        Endpoint {
            id,
            family: "chat",
            dialect,
            kind: Kind::Inference,
            upstream_path: None,
            path_param: None,
            action: None,
        }
    }

    #[test]
    fn caps_in_every_dialect() {
        let chat = ep("chat_completions", Dialect::OpenAiChat);
        let ceiling = (1 << 30) - 1;
        assert_eq!(
            output_cap(
                &chat,
                &json!({"max_tokens": 100, "max_completion_tokens": 300}),
                ceiling
            ),
            Ok(Some(300))
        );
        assert_eq!(
            output_cap(&chat, &json!({"max_tokens": -1}), ceiling)
                .unwrap_err()
                .field,
            "max_tokens"
        );
        assert_eq!(
            output_cap(&chat, &json!({"max_tokens": 1, "n": 129}), ceiling)
                .unwrap_err()
                .field,
            "n"
        );
        let gemini = ep("generateContent", Dialect::Gemini);
        let huge = json!({"generationConfig": {"maxOutputTokens": 18_446_744_073_686_646_784u64}});
        assert_eq!(
            output_cap(&gemini, &huge, ceiling).unwrap_err().field,
            "generationConfig.maxOutputTokens"
        );
        assert_eq!(choices(&chat, &json!({"n": 2})), 2);
        assert!(!generates(&ep("embeddings", Dialect::OpenAiEmbeddings)));
        assert!(generates(&chat));
    }

    #[test]
    fn clamping_lowers_or_adds_the_cap() {
        let chat = ep("chat_completions", Dialect::OpenAiChat);
        let mut body = json!({"max_tokens": 8000, "max_completion_tokens": 50});
        assert_eq!(clamp(&chat, &mut body, 1000), (true, true));
        assert_eq!(body, json!({"max_tokens": 1000, "max_completion_tokens": 50}));
        let mut none = json!({"messages": []});
        assert_eq!(clamp(&chat, &mut none, 1000), (true, false));
        assert_eq!(none["max_tokens"], 1000);
        let mut small = json!({"max_tokens": 10});
        assert_eq!(clamp(&chat, &mut small, 1000), (false, false));
        let gemini = ep("generateContent", Dialect::Gemini);
        let mut g = json!({"contents": []});
        assert_eq!(clamp(&gemini, &mut g, 7), (true, false));
        assert_eq!(g["generationConfig"]["maxOutputTokens"], 7);
    }

    #[test]
    fn inline_images_are_counted_once_with_their_payload() {
        let body = json!({"messages": [{"role": "user", "content": [
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "QUJD"}},
            {"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,REVG", "detail": "low"}},
            {"type": "image_url", "image_url": {"url": "https://example.com/a.png"}}]}]});
        let f = body_facts(&body);
        assert_eq!(f.media.images, 3);
        assert_eq!(f.inline_images, vec![("QUJD", false), ("REVG", true)]);
    }

    #[test]
    fn image_tokens_from_headers() {
        use antirion_limits::imagetokens::ImageFormula;
        use base64::Engine;
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend_from_slice(&512u32.to_be_bytes());
        png.extend_from_slice(&512u32.to_be_bytes());
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
        let tiles = [Some(ImageFormula::OpenAiTiles)];
        assert_eq!(image_tokens(&[(b64.as_str(), false)], &tiles, 1600), (255, 1));
        assert_eq!(image_tokens(&[("AAAA", false)], &tiles, 1600), (0, 0));
        assert_eq!(image_tokens(&[(b64.as_str(), false)], &[None], 1600), (0, 0));
        let mixed = [Some(ImageFormula::OpenAiTiles), None];
        assert_eq!(image_tokens(&[(b64.as_str(), false)], &mixed, 1600), (1600, 1));
    }

    #[test]
    fn texts_media_and_tools() {
        let body = json!({"model": "m", "messages": [
            {"role": "system", "content": "be brief"},
            {"role": "user", "content": [{"type": "text", "text": "hello"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}]}],
            "tools": [{"type": "function", "function": {"name": "f"}}]});
        let f = body_facts(&body);
        assert_eq!(f.texts, vec!["be brief", "hello"]);
        assert_eq!(f.messages, 2);
        assert_eq!(f.media.images, 1);
        assert!(f.image_in && !f.cache_markers);
        assert_eq!(f.media_bytes, "data:image/png;base64,AAAA".len() as u64);
        assert!(f.tools.is_some());
    }
}
