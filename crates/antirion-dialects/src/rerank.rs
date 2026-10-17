//! Rerank requests in the Cohere shape translated to each provider's rerank API, and their answers
//! back to `{id, results: [{index, relevance_score, document?}], meta?}` sorted by score.
//!
//! The client's documents are kept: a result's `document` is built from the request, never from the
//! provider's echo, so every upstream returns the same shape.

use serde_json::{Map, Value, json};

/// A provider rerank API.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RerankApi {
    /// Cohere's v2 `rerank`.
    Cohere,
    /// The `/rerank` of OpenAI-compatible servers: `top_n` and `results`.
    Compatible,
    /// The `/rerank` that takes `top_k` and answers `data`.
    TopK,
    /// Bedrock's agent runtime `Rerank`.
    Bedrock,
}

/// A rerank request as the codecs read it (the body passed validation).
#[derive(Clone, Debug, PartialEq)]
pub struct RerankRequest {
    pub query: String,
    pub documents: Vec<Value>,
    pub top_n: Option<u64>,
    pub return_documents: bool,
    pub max_tokens_per_doc: Option<u64>,
}

#[must_use]
pub fn decode_request(body: &Value) -> RerankRequest {
    RerankRequest {
        query: body
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        documents: body
            .get("documents")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        top_n: body.get("top_n").and_then(Value::as_u64),
        return_documents: body.get("return_documents").and_then(Value::as_bool) == Some(true),
        max_tokens_per_doc: body.get("max_tokens_per_doc").and_then(Value::as_u64),
    }
}

/// The text a provider ranks for a document: a string as it is, an object's string `text`, any
/// other object as its compact JSON.
#[must_use]
pub fn document_text(d: &Value) -> String {
    match d {
        Value::String(s) => s.clone(),
        Value::Object(o) => match o.get("text") {
            Some(Value::String(t)) => t.clone(),
            _ => serde_json::to_string(d).unwrap_or_default(),
        },
        other => other.to_string(),
    }
}

/// The Bedrock model ARN of a model id: an ARN as it is, else the foundation model in `region`.
fn model_arn(model: &str, region: &str) -> String {
    if model.starts_with("arn:") {
        model.to_owned()
    } else {
        format!("arn:aws:bedrock:{region}::foundation-model/{model}")
    }
}

/// The upstream body. `model` is the deployment's upstream id; `region` is Bedrock's.
#[must_use]
pub fn encode(api: RerankApi, r: &RerankRequest, model: &str, region: &str) -> Value {
    let texts: Vec<Value> = r
        .documents
        .iter()
        .map(|d| Value::from(document_text(d)))
        .collect();
    match api {
        RerankApi::Cohere => {
            let mut o = Map::new();
            o.insert("model".into(), Value::from(model));
            o.insert("query".into(), Value::from(r.query.as_str()));
            o.insert("documents".into(), Value::from(texts));
            if let Some(n) = r.top_n {
                o.insert("top_n".into(), Value::from(n));
            }
            if let Some(n) = r.max_tokens_per_doc {
                o.insert("max_tokens_per_doc".into(), Value::from(n));
            }
            Value::Object(o)
        }
        RerankApi::Compatible | RerankApi::TopK => {
            let mut o = Map::new();
            o.insert("model".into(), Value::from(model));
            o.insert("query".into(), Value::from(r.query.as_str()));
            o.insert("documents".into(), Value::from(texts));
            if let Some(n) = r.top_n {
                let k = if api == RerankApi::TopK { "top_k" } else { "top_n" };
                o.insert(k.into(), Value::from(n));
            }
            Value::Object(o)
        }
        RerankApi::Bedrock => {
            let sources: Vec<Value> = texts
                .iter()
                .map(|t| {
                    json!({"type": "INLINE", "inlineDocumentSource": {"type": "TEXT", "textDocument": {"text": t}}})
                })
                .collect();
            let mut model_cfg = Map::new();
            model_cfg.insert(
                "modelConfiguration".into(),
                json!({"modelArn": model_arn(model, region)}),
            );
            model_cfg.insert(
                "numberOfResults".into(),
                Value::from(r.top_n.unwrap_or(r.documents.len() as u64)),
            );
            json!({
                "queries": [{"type": "TEXT", "textQuery": {"text": r.query}}],
                "sources": sources,
                "rerankingConfiguration": {
                    "type": "BEDROCK_RERANKING_MODEL",
                    "bedrockRerankingConfiguration": Value::Object(model_cfg),
                },
            })
        }
    }
}

/// One scored document.
#[derive(Clone, Debug, PartialEq)]
pub struct Scored {
    pub index: usize,
    pub score: f64,
}

/// What a provider answered: the scores and the units it reports.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ranked {
    pub results: Vec<Scored>,
    /// Input tokens, when the provider reports them.
    pub tokens: Option<u64>,
    /// The provider's `id`, when it gives one.
    pub id: Option<String>,
    /// Cohere's `meta`, relayed as it is.
    pub meta: Option<Value>,
}

/// Reads a provider answer. `None` when it has no results list, or a result names no valid index.
#[must_use]
pub fn decode_answer(api: RerankApi, v: &Value, documents: usize) -> Option<Ranked> {
    let (list, score_key) = match api {
        RerankApi::Bedrock => (v.get("results")?.as_array()?, "relevanceScore"),
        _ => match v.get("results").or_else(|| v.get("data")) {
            Some(Value::Array(l)) => (l, "relevance_score"),
            _ => return None,
        },
    };
    let mut results = Vec::with_capacity(list.len());
    for item in list {
        let index = usize::try_from(item.get("index")?.as_u64()?).ok()?;
        if index >= documents {
            return None;
        }
        let score = item.get(score_key)?.as_f64()?;
        results.push(Scored { index, score });
    }
    let tokens = v
        .pointer("/usage/total_tokens")
        .or_else(|| v.pointer("/meta/billed_units/input_tokens"))
        .and_then(Value::as_u64);
    Some(Ranked {
        results,
        tokens,
        id: v.get("id").and_then(Value::as_str).map(str::to_owned),
        meta: v.get("meta").filter(|m| m.is_object()).cloned(),
    })
}

/// The client's answer: results sorted by score (highest first, ties by index), cut to `top_n`,
/// with each document when `return_documents` asked for them.
#[must_use]
pub fn client_body(r: &RerankRequest, mut ranked: Ranked, id: &str) -> Value {
    ranked.results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.index.cmp(&b.index))
    });
    if let Some(n) = r.top_n.and_then(|n| usize::try_from(n).ok()) {
        ranked.results.truncate(n);
    }
    let results: Vec<Value> = ranked
        .results
        .iter()
        .map(|s| {
            let mut o = Map::new();
            o.insert("index".into(), Value::from(s.index));
            o.insert("relevance_score".into(), Value::from(s.score));
            if r.return_documents {
                let doc = match r.documents.get(s.index) {
                    Some(Value::String(t)) => json!({ "text": t }),
                    Some(other) => other.clone(),
                    None => Value::Null,
                };
                o.insert("document".into(), doc);
            }
            Value::Object(o)
        })
        .collect();
    let mut body = Map::new();
    body.insert(
        "id".into(),
        Value::from(ranked.id.unwrap_or_else(|| id.to_owned())),
    );
    body.insert("results".into(), Value::from(results));
    if let Some(m) = ranked.meta {
        body.insert("meta".into(), m);
    }
    Value::Object(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> RerankRequest {
        decode_request(&json!({
            "query": "q", "documents": ["a", {"text": "b", "id": 7}, {"title": "c"}], "top_n": 2,
            "return_documents": true
        }))
    }

    #[test]
    fn bodies() {
        let r = req();
        let c = encode(RerankApi::Cohere, &r, "rerank-v3.5", "us-east-1");
        assert_eq!(c["documents"], json!(["a", "b", "{\"title\":\"c\"}"]));
        assert_eq!(c["top_n"], 2);
        assert_eq!(encode(RerankApi::TopK, &r, "m", "")["top_k"], 2);
        let b = encode(RerankApi::Bedrock, &r, "amazon.rerank-v1:0", "us-west-2");
        assert_eq!(
            b.pointer("/rerankingConfiguration/bedrockRerankingConfiguration/modelConfiguration/modelArn"),
            Some(&json!(
                "arn:aws:bedrock:us-west-2::foundation-model/amazon.rerank-v1:0"
            ))
        );
        assert_eq!(
            b["sources"][1]["inlineDocumentSource"]["textDocument"]["text"],
            "b"
        );
    }

    #[test]
    fn answers_are_sorted_and_cut() {
        let r = req();
        let ranked = decode_answer(
            RerankApi::Compatible,
            &json!({"results": [{"index": 0, "relevance_score": 0.1}, {"index": 2, "relevance_score": 0.9}, {"index": 1, "relevance_score": 0.5}], "usage": {"total_tokens": 9}}),
            3,
        )
        .unwrap();
        assert_eq!(ranked.tokens, Some(9));
        let body = client_body(&r, ranked, "rr-1");
        assert_eq!(body["results"].as_array().unwrap().len(), 2);
        assert_eq!(body["results"][0]["index"], 2);
        assert_eq!(body["results"][0]["document"], json!({"title": "c"}));
        assert_eq!(body["results"][1]["document"], json!({"text": "b", "id": 7}));
        assert_eq!(body["id"], "rr-1");
        let topk = decode_answer(
            RerankApi::TopK,
            &json!({"data": [{"index": 1, "relevance_score": 0.3}]}),
            3,
        );
        assert_eq!(topk.unwrap().results[0].index, 1);
        let bedrock = decode_answer(
            RerankApi::Bedrock,
            &json!({"results": [{"index": 0, "relevanceScore": 0.7}]}),
            3,
        );
        assert_eq!(bedrock.unwrap().results[0].score, 0.7);
        assert!(
            decode_answer(
                RerankApi::Cohere,
                &json!({"results": [{"index": 5, "relevance_score": 1.0}]}),
                3
            )
            .is_none()
        );
        assert!(decode_answer(RerankApi::Cohere, &json!({}), 3).is_none());
    }
}
