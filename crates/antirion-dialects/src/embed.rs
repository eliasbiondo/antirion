//! Embeddings requests translated from the OpenAI shape to other providers' embedding APIs, and their
//! answers back to the OpenAI list.
//!
//! Each input text becomes one instance of the provider's request. Token-id inputs have no
//! equivalent outside the OpenAI API and are refused. The answer is always
//! `{object: "list", data: [{object: "embedding", index, embedding}], model, usage}`, with
//! `encoding_format: "base64"` honoured as the base64 of each vector's little-endian `f32` values.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Value, json};

/// A provider embedding API.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EmbedApi {
    /// Gemini `embedContent` (one text) and `batchEmbedContents` (several).
    Gemini,
    /// Vertex AI `predict` on a Google embedding model.
    VertexPredict,
    /// Amazon Titan text embeddings on Bedrock InvokeModel: one text per call.
    Titan,
    /// Cohere embedding models on Bedrock InvokeModel.
    BedrockCohere,
    /// Cohere's v2 `embed`.
    Cohere,
}

/// Why an embeddings request cannot be sent to a provider API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Untranslatable {
    /// The client field that has no equivalent.
    pub param: String,
}

/// An embeddings request as the provider codecs read it.
#[derive(Clone, Debug, PartialEq)]
pub struct EmbedRequest {
    pub texts: Vec<String>,
    /// `input` was a single string (one instance, sent with the single-text call where there is one).
    pub single: bool,
    pub dimensions: Option<u64>,
    /// The Cohere-style `input_type` extension field.
    pub input_type: Option<String>,
    /// The Gemini-style `task_type` extension field.
    pub task_type: Option<String>,
    pub title: Option<String>,
    pub base64: bool,
}

/// Reads an OpenAI embeddings body. Integer (token) inputs are refused.
pub fn decode_request(body: &Value) -> Result<EmbedRequest, Untranslatable> {
    let refuse = |param: &str| Untranslatable {
        param: param.to_owned(),
    };
    let (texts, single) = match body.get("input") {
        Some(Value::String(s)) => (vec![s.clone()], true),
        Some(Value::Array(items)) => {
            let mut texts = Vec::with_capacity(items.len());
            for (i, item) in items.iter().enumerate() {
                match item {
                    Value::String(s) => texts.push(s.clone()),
                    _ => return Err(refuse(&format!("input[{i}]"))),
                }
            }
            (texts, false)
        }
        _ => return Err(refuse("input")),
    };
    let text = |k: &str| body.get(k).and_then(Value::as_str).map(str::to_owned);
    Ok(EmbedRequest {
        texts,
        single,
        dimensions: body.get("dimensions").and_then(Value::as_u64),
        input_type: text("input_type"),
        task_type: text("task_type"),
        title: text("title"),
        base64: body.get("encoding_format").and_then(Value::as_str) == Some("base64"),
    })
}

/// The Gemini task type of a request: its `task_type`, else its `input_type` mapped by meaning.
fn gemini_task(r: &EmbedRequest) -> Option<String> {
    if let Some(t) = &r.task_type {
        return Some(t.clone());
    }
    let mapped = match r.input_type.as_deref()? {
        "search_query" | "query" => "RETRIEVAL_QUERY",
        "search_document" | "document" => "RETRIEVAL_DOCUMENT",
        "classification" => "CLASSIFICATION",
        "clustering" => "CLUSTERING",
        other => return Some(other.to_owned()),
    };
    Some(mapped.to_owned())
}

/// The Cohere input type of a request: its `input_type`, else its `task_type` mapped by meaning,
/// else `search_document`, which Cohere's embedding models require.
fn cohere_input_type(r: &EmbedRequest) -> String {
    if let Some(t) = &r.input_type {
        return t.clone();
    }
    match r.task_type.as_deref() {
        Some("RETRIEVAL_QUERY" | "QUESTION_ANSWERING" | "CODE_RETRIEVAL_QUERY") => "search_query",
        Some("CLASSIFICATION") => "classification",
        Some("CLUSTERING") => "clustering",
        _ => "search_document",
    }
    .to_owned()
}

/// The upstream bodies of a request, one per upstream call (several only for Titan, which embeds
/// one text per call). `model` is the deployment's upstream model id.
#[must_use]
pub fn encode(api: EmbedApi, r: &EmbedRequest, model: &str) -> Vec<Value> {
    match api {
        EmbedApi::Gemini => {
            let one = |text: &str, with_model: bool| {
                let mut o = Map::new();
                if with_model {
                    o.insert("model".into(), Value::from(gemini_model(model)));
                }
                o.insert("content".into(), json!({"parts": [{"text": text}]}));
                if let Some(t) = gemini_task(r) {
                    o.insert("taskType".into(), Value::from(t));
                }
                if let Some(t) = &r.title {
                    o.insert("title".into(), Value::from(t.as_str()));
                }
                if let Some(d) = r.dimensions {
                    o.insert("outputDimensionality".into(), Value::from(d));
                }
                Value::Object(o)
            };
            if r.single {
                vec![one(r.texts.first().map_or("", String::as_str), false)]
            } else {
                let requests: Vec<Value> = r.texts.iter().map(|t| one(t, true)).collect();
                vec![json!({ "requests": requests })]
            }
        }
        EmbedApi::VertexPredict => {
            let instances: Vec<Value> = r
                .texts
                .iter()
                .map(|t| {
                    let mut o = Map::new();
                    o.insert("content".into(), Value::from(t.as_str()));
                    if let Some(k) = gemini_task(r) {
                        o.insert("task_type".into(), Value::from(k));
                    }
                    if let Some(title) = &r.title {
                        o.insert("title".into(), Value::from(title.as_str()));
                    }
                    Value::Object(o)
                })
                .collect();
            let mut body = Map::new();
            body.insert("instances".into(), Value::from(instances));
            if let Some(d) = r.dimensions {
                body.insert("parameters".into(), json!({ "outputDimensionality": d }));
            }
            vec![Value::Object(body)]
        }
        EmbedApi::Titan => r
            .texts
            .iter()
            .map(|t| {
                let mut o = Map::new();
                o.insert("inputText".into(), Value::from(t.as_str()));
                if let Some(d) = r.dimensions {
                    o.insert("dimensions".into(), Value::from(d));
                }
                Value::Object(o)
            })
            .collect(),
        EmbedApi::BedrockCohere => {
            let mut o = Map::new();
            o.insert("texts".into(), Value::from(r.texts.clone()));
            o.insert("input_type".into(), Value::from(cohere_input_type(r)));
            if let Some(d) = r.dimensions {
                o.insert("output_dimension".into(), Value::from(d));
            }
            vec![Value::Object(o)]
        }
        EmbedApi::Cohere => {
            let mut o = Map::new();
            o.insert("model".into(), Value::from(model));
            o.insert("texts".into(), Value::from(r.texts.clone()));
            o.insert("input_type".into(), Value::from(cohere_input_type(r)));
            o.insert("embedding_types".into(), json!(["float"]));
            if let Some(d) = r.dimensions {
                o.insert("output_dimension".into(), Value::from(d));
            }
            vec![Value::Object(o)]
        }
    }
}

/// Gemini's `model` member inside a batch request: `models/<id>` unless the id already names its
/// collection.
fn gemini_model(model: &str) -> String {
    if model.starts_with("models/") || model.starts_with("tunedModels/") {
        model.to_owned()
    } else {
        format!("models/{model}")
    }
}

/// The vectors and the reported input tokens of the provider's answers, in input order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Embedded {
    pub vectors: Vec<Vec<f64>>,
    /// `None` when the provider reports no token count.
    pub tokens: Option<u64>,
}

fn numbers(v: &Value) -> Option<Vec<f64>> {
    v.as_array()?.iter().map(Value::as_f64).collect()
}

/// Reads the provider answers (one per call, in call order). `None` when an answer lacks its
/// vectors or holds a different number of them than texts were sent.
#[must_use]
pub fn decode_answers(api: EmbedApi, answers: &[Value], expected: usize) -> Option<Embedded> {
    let mut out = Embedded::default();
    let mut add_tokens = |n: Option<u64>| {
        if let Some(n) = n {
            out.tokens = Some(out.tokens.unwrap_or(0) + n);
        }
    };
    let mut vectors = Vec::with_capacity(expected);
    for a in answers {
        match api {
            EmbedApi::Gemini => {
                if let Some(one) = a.pointer("/embedding/values") {
                    vectors.push(numbers(one)?);
                } else {
                    for e in a.get("embeddings")?.as_array()? {
                        vectors.push(numbers(e.get("values")?)?);
                    }
                }
                add_tokens(
                    a.pointer("/usageMetadata/promptTokenCount")
                        .and_then(Value::as_u64),
                );
            }
            EmbedApi::VertexPredict => {
                for p in a.get("predictions")?.as_array()? {
                    let e = p.get("embeddings")?;
                    vectors.push(numbers(e.get("values")?)?);
                    add_tokens(
                        e.pointer("/statistics/token_count")
                            .and_then(Value::as_f64)
                            .map(|n| n.max(0.0).round() as u64),
                    );
                }
            }
            EmbedApi::Titan => {
                vectors.push(numbers(a.get("embedding")?)?);
                add_tokens(a.get("inputTextTokenCount").and_then(Value::as_u64));
            }
            EmbedApi::BedrockCohere | EmbedApi::Cohere => {
                let list = match a.get("embeddings")? {
                    Value::Array(l) => l,
                    Value::Object(o) => o.get("float")?.as_array()?,
                    _ => return None,
                };
                for e in list {
                    vectors.push(numbers(e)?);
                }
                add_tokens(
                    a.pointer("/meta/billed_units/input_tokens")
                        .and_then(Value::as_u64),
                );
            }
        }
    }
    if vectors.len() != expected {
        return None;
    }
    out.vectors = vectors;
    Some(out)
}

/// The base64 of a vector's little-endian `f32` values.
#[must_use]
pub fn base64_vector(v: &[f64]) -> String {
    let mut bytes = Vec::with_capacity(v.len() * 4);
    for x in v {
        #[allow(clippy::cast_possible_truncation)]
        bytes.extend_from_slice(&(*x as f32).to_le_bytes());
    }
    STANDARD.encode(bytes)
}

/// The OpenAI list the client receives. `prompt_tokens` is 0 when the provider reported none.
#[must_use]
pub fn client_body(e: &Embedded, model: &str, base64: bool) -> Value {
    let data: Vec<Value> = e
        .vectors
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let embedding = if base64 {
                Value::from(base64_vector(v))
            } else {
                Value::from(v.clone())
            };
            json!({"object": "embedding", "index": i, "embedding": embedding})
        })
        .collect();
    let tokens = e.tokens.unwrap_or(0);
    json!({
        "object": "list",
        "data": data,
        "model": model,
        "usage": {"prompt_tokens": tokens, "total_tokens": tokens},
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(body: Value) -> EmbedRequest {
        decode_request(&body).unwrap()
    }

    #[test]
    fn inputs_and_refusals() {
        let r = req(json!({"input": "a", "dimensions": 8, "encoding_format": "base64"}));
        assert_eq!(
            (r.texts.clone(), r.single, r.dimensions, r.base64),
            (vec!["a".to_owned()], true, Some(8), true)
        );
        assert_eq!(req(json!({"input": ["a", "b"]})).texts.len(), 2);
        assert_eq!(
            decode_request(&json!({"input": [1, 2]})),
            Err(Untranslatable {
                param: "input[0]".into()
            })
        );
        assert_eq!(
            decode_request(&json!({"input": [[1]]})).unwrap_err().param,
            "input[0]"
        );
    }

    #[test]
    fn bodies_per_api() {
        let r =
            req(json!({"input": ["a", "b"], "dimensions": 4, "task_type": "RETRIEVAL_QUERY", "title": "t"}));
        let g = encode(EmbedApi::Gemini, &r, "text-embedding-004");
        assert_eq!(g.len(), 1);
        assert_eq!(g[0]["requests"][1]["model"], "models/text-embedding-004");
        assert_eq!(g[0]["requests"][0]["outputDimensionality"], 4);
        assert_eq!(g[0]["requests"][0]["taskType"], "RETRIEVAL_QUERY");
        let one = encode(EmbedApi::Gemini, &req(json!({"input": "x"})), "m");
        assert_eq!(one[0], json!({"content": {"parts": [{"text": "x"}]}}));
        let v = encode(EmbedApi::VertexPredict, &r, "m");
        assert_eq!(v[0]["instances"][1]["content"], "b");
        assert_eq!(v[0]["parameters"]["outputDimensionality"], 4);
        assert_eq!(encode(EmbedApi::Titan, &r, "m").len(), 2);
        let c = encode(EmbedApi::Cohere, &r, "embed-v4.0");
        assert_eq!(c[0]["input_type"], "search_query");
        assert_eq!(c[0]["output_dimension"], 4);
        let b = encode(
            EmbedApi::BedrockCohere,
            &req(json!({"input": "x", "input_type": "clustering"})),
            "m",
        );
        assert_eq!(b[0]["input_type"], "clustering");
        assert_eq!(
            gemini_task(&req(json!({"input": "x", "input_type": "search_document"}))).as_deref(),
            Some("RETRIEVAL_DOCUMENT")
        );
    }

    #[test]
    fn answers_back_to_the_list() {
        let g = decode_answers(
            EmbedApi::Gemini,
            &[json!({"embeddings": [{"values": [0.5]}, {"values": [1.0]}]})],
            2,
        )
        .unwrap();
        assert_eq!((g.vectors.len(), g.tokens), (2, None));
        assert!(decode_answers(EmbedApi::Gemini, &[json!({"embeddings": []})], 1).is_none());
        let v = decode_answers(
            EmbedApi::VertexPredict,
            &[json!({"predictions": [{"embeddings": {"values": [0.1], "statistics": {"token_count": 3.0}}}]})],
            1,
        )
        .unwrap();
        assert_eq!(v.tokens, Some(3));
        let t = decode_answers(
            EmbedApi::Titan,
            &[
                json!({"embedding": [0.1], "inputTextTokenCount": 2}),
                json!({"embedding": [0.2], "inputTextTokenCount": 5}),
            ],
            2,
        )
        .unwrap();
        assert_eq!(t.tokens, Some(7));
        let c = decode_answers(
            EmbedApi::Cohere,
            &[json!({"embeddings": {"float": [[1.0, 2.0]]}, "meta": {"billed_units": {"input_tokens": 4}}})],
            1,
        )
        .unwrap();
        assert_eq!(c.tokens, Some(4));
        let body = client_body(&c, "m", true);
        assert_eq!(body["data"][0]["embedding"], base64_vector(&[1.0, 2.0]));
        assert_eq!(body["usage"]["prompt_tokens"], 4);
        let decoded = STANDARD
            .decode(body["data"][0]["embedding"].as_str().unwrap())
            .unwrap();
        assert_eq!(decoded, [1.0f32.to_le_bytes(), 2.0f32.to_le_bytes()].concat());
    }
}
