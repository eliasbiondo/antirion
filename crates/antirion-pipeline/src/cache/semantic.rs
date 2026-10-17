//! The semantic layer of a lookup, after an exact miss: the query is embedded through the
//! embedding Model (an auxiliary `cache_embedding` line of the request), then compared with the
//! vectors of its partition. A failure or timeout fails open, as an `error` lookup.

use std::sync::Arc;

use antirion_cache::key::KeyDoc;
use antirion_cache::semantic::{self, Query};
use antirion_cache::vectors::{self, Candidate, Probe};
use antirion_cache::{CachePolicy, Directives, Entry, SemanticPolicy};
use antirion_limits::ledger::Purpose;

use crate::ctx::Ctx;
use crate::internal::{Answer, Asker, Question, ask};

/// What a stored response adds to the semantic store.
#[derive(Clone, Debug)]
pub(crate) struct SemanticWrite {
    pub(crate) partition: String,
    pub(crate) vector: Arc<[f32]>,
    pub(crate) polarity: Vec<String>,
}

/// The semantic layer's answer.
pub(super) enum Semantic {
    /// The entry under `key` answers with `similarity`.
    Hit {
        entry: Arc<Entry>,
        similarity: f64,
        key: String,
    },
    Miss(SemanticWrite),
    Failed,
}

/// The query of a request the semantic layer serves.
pub(super) fn query<'p>(
    policy: &'p CachePolicy,
    op: &str,
    body: &serde_json::Value,
) -> Option<(&'p SemanticPolicy, Query)> {
    let sem = policy.semantic.as_ref()?;
    let q = semantic::eligible(op, body, sem.max_text_chars)?;
    Some((sem, q))
}

/// The partition key: the exact key's document with the last user text emptied, as version `s1`.
pub(super) fn partition(doc: &KeyDoc, op: &str, q: &Query, policy: &CachePolicy) -> String {
    let mut d = doc.clone();
    d.body = antirion_cache::key::normalise(op, &q.partition_body, &policy.ignore_fields);
    d.partition_key()
}

/// Embeds the query and searches its partition, within `semantic_store.lookup_timeout`.
pub(super) async fn lookup(
    cx: &Ctx,
    policy: &CachePolicy,
    d: &Directives,
    sem: &SemanticPolicy,
    q: &Query,
    partition: String,
    now_ms: i64,
) -> Semantic {
    let cache = &cx.rt.state.cache;
    let timeout = cache.semantic_timeout();
    let texts = [q.text.clone()];
    let asker = Asker {
        purpose: Purpose::CacheEmbedding,
        line: 1,
    };
    let call = ask(cx, &sem.embedding_model, Question::Embed(&texts), timeout, asker);
    let vector = match tokio::time::timeout(timeout, call).await {
        Ok(Ok(Answer::Vectors(v))) => v.into_iter().next().and_then(|v| vectors::normalised(&v)),
        Ok(Ok(Answer::Text(_)) | Err(_)) | Err(_) => None,
    };
    let Some(vector) = vector else {
        tracing::debug!("the semantic cache could not embed the query; continuing without it");
        return Semantic::Failed;
    };
    let polarity = sem.polarity.as_ref().map(|l| l.tokens(&q.text));
    let write = SemanticWrite {
        partition,
        vector,
        polarity: polarity.clone().unwrap_or_default(),
    };
    let probe = Probe {
        vector: &write.vector,
        polarity: polarity.as_deref(),
        threshold: d.threshold.unwrap_or(policy.threshold),
        now_ms,
        written_after_ms: d
            .max_age
            .map(|n| now_ms.saturating_sub(i64::try_from(n.saturating_mul(1000)).unwrap_or(i64::MAX))),
    };
    let Some(Candidate {
        scope,
        key,
        similarity,
    }) = cache.vectors.search(&write.partition, &probe)
    else {
        return Semantic::Miss(write);
    };
    match tokio::time::timeout(timeout, cache.lookup(&scope, &key, now_ms)).await {
        Ok(Ok(Some(entry))) => Semantic::Hit {
            entry,
            similarity,
            key,
        },
        Ok(Ok(None)) => {
            cache.vectors.delete_key(&key);
            Semantic::Miss(write)
        }
        Ok(Err(_)) | Err(_) => Semantic::Failed,
    }
}
