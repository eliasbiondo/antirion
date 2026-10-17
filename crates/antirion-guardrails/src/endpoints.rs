//! Segments of the endpoints whose bodies are not conversations: embeddings and rerank inputs,
//! image prompts (all `user`), and the text of a transcription answer (`content`).

use serde_json::Value;

use crate::def::{Origin, SegKind};
use crate::segments::{Dialect, Loc, Segment};

fn seg(kind: SegKind, origin: Origin, ptr: String, text: &str) -> Segment {
    Segment {
        kind,
        origin,
        loc: Loc::Ptr(ptr),
        text: text.to_owned(),
        message: None,
    }
}

/// A string, or each string of an array, under `ptr`.
fn strings(v: Option<&Value>, ptr: &str, out: &mut Vec<Segment>) {
    match v {
        Some(Value::String(s)) => out.push(seg(SegKind::User, Origin::Client, ptr.to_owned(), s)),
        Some(Value::Array(items)) => {
            for (i, item) in items.iter().enumerate() {
                match item {
                    Value::String(s) => out.push(seg(SegKind::User, Origin::Client, format!("{ptr}/{i}"), s)),
                    Value::Object(o) => {
                        if let Some(Value::String(s)) = o.get("text") {
                            out.push(seg(SegKind::User, Origin::Client, format!("{ptr}/{i}/text"), s));
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

/// The input segments of an embeddings, image generation, rerank or transcription request.
#[must_use]
pub fn input(body: &Value, d: Dialect) -> Vec<Segment> {
    let mut out = Vec::new();
    match d {
        Dialect::Embeddings => strings(body.get("input"), "/input", &mut out),
        Dialect::Images => strings(body.get("prompt"), "/prompt", &mut out),
        Dialect::Rerank => {
            strings(body.get("query"), "/query", &mut out);
            strings(body.get("documents"), "/documents", &mut out);
        }
        _ => {}
    }
    out
}

/// The output segments of a transcription answer: its `text`.
#[must_use]
pub fn output(body: &Value, d: Dialect) -> Vec<Segment> {
    match (d, body.get("text")) {
        (Dialect::Transcription, Some(Value::String(s))) => {
            vec![seg(SegKind::Content, Origin::Model, "/text".to_owned(), s)]
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn inputs_and_outputs() {
        let paths = |segs: Vec<Segment>| segs.into_iter().map(|s| s.path()).collect::<Vec<_>>();
        assert_eq!(
            paths(input(&json!({"input": ["a", [1, 2], "b"]}), Dialect::Embeddings)),
            ["/input/0", "/input/2"]
        );
        assert_eq!(
            paths(input(&json!({"input": "a"}), Dialect::Embeddings)),
            ["/input"]
        );
        assert_eq!(
            paths(input(&json!({"prompt": "cat"}), Dialect::Images)),
            ["/prompt"]
        );
        assert_eq!(
            paths(input(
                &json!({"query": "q", "documents": ["d", {"text": "e"}]}),
                Dialect::Rerank
            )),
            ["/query", "/documents/0", "/documents/1/text"]
        );
        assert_eq!(
            paths(output(&json!({"text": "hello"}), Dialect::Transcription)),
            ["/text"]
        );
        assert!(input(&json!({}), Dialect::Transcription).is_empty());
    }
}
