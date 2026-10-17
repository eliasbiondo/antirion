//! Completions requests served by Chat deployments: the request is turned into a Chat Completions
//! request (its single string prompt as one user message) and the answer, complete or streamed,
//! back into text completions.

use std::pin::Pin;
use std::task::{Context, Poll};

use antirion_core::Stage;
use antirion_dialects::{Endpoint, Matched, match_endpoint};
use antirion_error::reason::hygiene;
use antirion_error::{GatewayError, Message};
use bytes::Bytes;
use http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use http::{Method, Response};
use http_body::Frame;
use http_body_util::BodyExt;
use serde_json::{Map, Value, json};

use crate::ctx::{Body, BoxError};
use crate::parse::Parsed;

/// Whether the effective `translation` policy asks for it.
pub(crate) fn enabled(translation: &Value) -> bool {
    translation.get("completions_to_chat") == Some(&Value::Bool(true))
}

/// The Chat Completions endpoint the request is served as.
pub(crate) fn chat_endpoint() -> Option<Endpoint> {
    match match_endpoint(&Method::POST, "/v1/chat/completions") {
        Matched::Found(e) => Some(e),
        _ => None,
    }
}

fn unsupported(param: &'static str, message: &'static str) -> GatewayError {
    GatewayError::with_reason(
        hygiene::TRANSLATION_UNSUPPORTED,
        Stage::Resolve,
        Message::Fixed(message),
    )
    .param(param)
}

/// The Chat request of a Completions body; the members a Chat deployment cannot honour are
/// refused.
pub(crate) fn to_chat(parsed: &Parsed) -> Result<Parsed, GatewayError> {
    let Some(Value::Object(body)) = parsed.doc() else {
        return Err(unsupported("prompt", "The request body must be a JSON object."));
    };
    let used =
        |name: &str, ok: &dyn Fn(&Value) -> bool| body.get(name).is_some_and(|v| !v.is_null() && !ok(v));
    if used("suffix", &|v| v.as_str() == Some("")) {
        return Err(unsupported(
            "suffix",
            "suffix cannot be served by a Chat deployment.",
        ));
    }
    if used("echo", &|v| v == &Value::Bool(false)) {
        return Err(unsupported("echo", "echo cannot be served by a Chat deployment."));
    }
    if used("best_of", &|v| v.as_u64() == Some(1)) {
        return Err(unsupported(
            "best_of",
            "best_of cannot be served by a Chat deployment.",
        ));
    }
    if used("logprobs", &|_| false) {
        return Err(unsupported(
            "logprobs",
            "logprobs cannot be served by a Chat deployment.",
        ));
    }
    let Some(prompt) = body.get("prompt").and_then(Value::as_str) else {
        return Err(unsupported(
            "prompt",
            "Only a single string prompt can be served by a Chat deployment.",
        ));
    };
    let mut chat = Map::new();
    for (k, v) in body {
        match k.as_str() {
            "prompt" => {
                chat.insert("messages".into(), json!([{"role": "user", "content": prompt}]));
            }
            "suffix" | "echo" | "best_of" | "logprobs" => {}
            _ => {
                chat.insert(k.clone(), v.clone());
            }
        }
    }
    let v = Value::Object(chat);
    let raw = Bytes::from(
        serde_json::to_vec(&v).map_err(|_| unsupported("prompt", "The request body cannot be read."))?,
    );
    let skim = antirion_json::skim(&raw, &antirion_json::JsonLimits::default())
        .map_err(|_| unsupported("prompt", "The request body cannot be read."))?;
    Ok(Parsed::json(raw, skim, Some(v)))
}

/// The members a text completion keeps of a Chat answer or chunk.
fn base(chat: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for k in ["id", "created", "model", "system_fingerprint"] {
        if let Some(v) = chat.get(k) {
            out.insert(k.into(), v.clone());
        }
    }
    out.insert("object".into(), Value::from("text_completion"));
    out
}

fn choice(c: &Value, content: Option<&Value>) -> Value {
    json!({
        "text": content.and_then(Value::as_str).unwrap_or_default(),
        "index": c.get("index").cloned().unwrap_or(Value::from(0)),
        "logprobs": null,
        "finish_reason": c.get("finish_reason").cloned().unwrap_or(Value::Null),
    })
}

/// A complete Chat answer as a text completion; anything else is left as it is.
fn completion(v: Value) -> Value {
    let Value::Object(chat) = &v else {
        return v;
    };
    let Some(choices) = chat.get("choices").and_then(Value::as_array) else {
        return v;
    };
    let mut out = base(chat);
    out.insert(
        "choices".into(),
        Value::from(
            choices
                .iter()
                .map(|c| choice(c, c.pointer("/message/content")))
                .collect::<Vec<_>>(),
        ),
    );
    if let Some(u) = chat.get("usage") {
        out.insert("usage".into(), u.clone());
    }
    Value::Object(out)
}

/// A streamed Chat chunk as a text completion chunk.
fn chunk(v: Value) -> Value {
    let Value::Object(chat) = &v else {
        return v;
    };
    if chat.get("object").and_then(Value::as_str) != Some("chat.completion.chunk") {
        return v;
    }
    let mut out = base(chat);
    let choices = chat
        .get("choices")
        .and_then(Value::as_array)
        .map(|cs| {
            cs.iter()
                .map(|c| choice(c, c.pointer("/delta/content")))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    out.insert("choices".into(), Value::from(choices));
    if let Some(u) = chat.get("usage") {
        out.insert("usage".into(), u.clone());
    }
    Value::Object(out)
}

/// One SSE event with its `data` lines turned into a text completion chunk.
fn event(block: &str) -> String {
    let mut out = String::with_capacity(block.len());
    for line in block.split('\n') {
        match line.strip_prefix("data:").map(str::trim_start) {
            Some(data) if data != "[DONE]" => match serde_json::from_str::<Value>(data) {
                Ok(v) => {
                    out.push_str("data: ");
                    out.push_str(&chunk(v).to_string());
                }
                Err(_) => out.push_str(line),
            },
            _ => out.push_str(line),
        }
        out.push('\n');
    }
    out
}

/// A stream whose events are rewritten as they complete; trailers pass through.
struct Chunks {
    inner: Body,
    pending: Vec<u8>,
    /// Trailers that arrived while an incomplete event was still buffered.
    trailers: Option<http::HeaderMap>,
}

impl Chunks {
    /// The complete events buffered so far, rewritten. The gateway's own streams end each event
    /// with a blank line, and the cut is made on bytes so a character split across frames waits.
    fn ready(&mut self) -> Option<Bytes> {
        let end = self.pending.windows(2).rposition(|w| w == b"\n\n")? + 2;
        let done: Vec<u8> = self.pending.drain(..end).collect();
        let text = String::from_utf8_lossy(&done);
        let out: String = text
            .split_inclusive("\n\n")
            .map(|b| event(b.trim_end_matches('\n')) + "\n")
            .collect();
        Some(Bytes::from(out))
    }
}

impl http_body::Body for Chunks {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        if let Some(t) = self.trailers.take() {
            return Poll::Ready(Some(Ok(Frame::trailers(t))));
        }
        loop {
            match Pin::new(&mut self.inner).poll_frame(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(frame))) => match frame.into_data() {
                    Ok(data) => {
                        self.pending.extend_from_slice(&data);
                        if let Some(out) = self.ready() {
                            return Poll::Ready(Some(Ok(Frame::data(out))));
                        }
                    }
                    Err(frame) => {
                        if self.pending.is_empty() {
                            return Poll::Ready(Some(Ok(frame)));
                        }
                        self.trailers = frame.into_trailers().ok();
                        let rest = std::mem::take(&mut self.pending);
                        return Poll::Ready(Some(Ok(Frame::data(Bytes::from(rest)))));
                    }
                },
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Some(Err(e))),
                Poll::Ready(None) => {
                    if self.pending.is_empty() {
                        return Poll::Ready(None);
                    }
                    let rest = std::mem::take(&mut self.pending);
                    return Poll::Ready(Some(Ok(Frame::data(Bytes::from(rest)))));
                }
            }
        }
    }
}

/// The client's answer to a Completions request served as Chat.
pub(crate) async fn respond(resp: Response<Body>) -> Response<Body> {
    let (mut parts, body) = resp.into_parts();
    let ct = parts
        .headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if ct.starts_with("text/event-stream") {
        let body = Chunks {
            inner: body,
            pending: Vec::new(),
            trailers: None,
        };
        return Response::from_parts(parts, body.boxed());
    }
    if !parts.status.is_success() || !ct.starts_with("application/json") {
        return Response::from_parts(parts, body);
    }
    let bytes = match body.collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => return Response::from_parts(parts, crate::ctx::empty()),
    };
    let out = match serde_json::from_slice::<Value>(&bytes) {
        Ok(v) => Bytes::from(serde_json::to_vec(&completion(v)).unwrap_or_default()),
        Err(_) => bytes,
    };
    parts.headers.remove(CONTENT_LENGTH);
    Response::from_parts(parts, crate::ctx::full(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(v: &Value) -> Parsed {
        let raw = Bytes::from(serde_json::to_vec(v).unwrap());
        let skim = antirion_json::skim(&raw, &antirion_json::JsonLimits::default()).unwrap();
        Parsed::json(raw, skim, Some(v.clone()))
    }

    #[test]
    fn a_prompt_becomes_one_user_message() {
        let p =
            parsed(&json!({"model": "m", "prompt": "Say hi", "max_tokens": 5, "echo": false, "best_of": 1}));
        let chat = to_chat(&p).unwrap();
        assert_eq!(
            chat.doc().unwrap(),
            &json!({"model": "m", "messages": [{"role": "user", "content": "Say hi"}], "max_tokens": 5})
        );
        for (body, param) in [
            (json!({"prompt": "x", "suffix": "s"}), "suffix"),
            (json!({"prompt": "x", "echo": true}), "echo"),
            (json!({"prompt": "x", "best_of": 2}), "best_of"),
            (json!({"prompt": "x", "logprobs": 1}), "logprobs"),
            (json!({"prompt": ["a", "b"]}), "prompt"),
        ] {
            let Err(e) = to_chat(&parsed(&body)) else {
                panic!("{param} is refused");
            };
            assert_eq!(e.param.as_deref(), Some(param));
        }
    }

    #[test]
    fn answers_become_text_completions() {
        let c = completion(json!({
            "id": "c1", "object": "chat.completion", "created": 7, "model": "m",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "hi"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4}
        }));
        assert_eq!(c["object"], "text_completion");
        assert_eq!(c["choices"][0]["text"], "hi");
        assert_eq!(c["choices"][0]["logprobs"], Value::Null);
        assert_eq!(c["usage"]["total_tokens"], 4);
        let e = event(
            "data: {\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"h\"},\"finish_reason\":null}]}",
        );
        assert!(e.starts_with("data: {"));
        let v: Value = serde_json::from_str(e.trim().trim_start_matches("data: ")).unwrap();
        assert_eq!(v["object"], "text_completion");
        assert_eq!(v["choices"][0]["text"], "h");
        assert_eq!(event("data: [DONE]"), "data: [DONE]\n");
    }

    #[tokio::test]
    async fn streams_are_rewritten_across_frames() {
        let chunk = "data: {\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ab\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n";
        let (a, b) = chunk.split_at(40);
        let frames = futures_util::stream::iter([
            Ok::<_, BoxError>(Frame::data(Bytes::from(a.to_owned()))),
            Ok(Frame::data(Bytes::from(b.to_owned()))),
        ]);
        let mut resp = Response::new(http_body_util::StreamBody::new(frames).boxed());
        resp.headers_mut()
            .insert(CONTENT_TYPE, http::HeaderValue::from_static("text/event-stream"));
        let out = respond(resp)
            .await
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let text = String::from_utf8(out.to_vec()).unwrap();
        assert!(text.contains("\"object\":\"text_completion\""), "{text}");
        assert!(text.contains("\"text\":\"ab\""), "{text}");
        assert!(text.ends_with("data: [DONE]\n\n"), "{text}");
    }
}
