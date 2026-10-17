//! The per-item embedding cache: each element of an embeddings request's `input` array has its own
//! entry, keyed like the whole request with `input` replaced by that element. Hits are kept, only
//! the elements that missed go upstream, and the answer is put back together in input order.

use antirion_cache::{Entry, Status};
use futures_util::future::join_all;
use http::Response;
use http_body_util::BodyExt;
use serde_json::{Value, json};

use super::{CacheSlot, Inputs, Lookup, record, replay, store};
use crate::ctx::{Body, Ctx, full};

/// The largest answer merged.
const MAX_ANSWER: usize = 64 << 20;

/// What the lookup of an item-cached request found.
#[derive(Debug, Clone)]
pub(crate) struct Items {
    /// The key of each element, in input order.
    keys: Vec<String>,
    /// The cached `data` item of each element that hit.
    hits: Vec<Option<Value>>,
    /// The request body as the client sent it.
    body: Value,
}

impl Items {
    /// Whether every element hit.
    pub(crate) fn all_hit(&self) -> bool {
        self.hits.iter().all(Option::is_some)
    }

    /// Whether some elements hit and others go upstream.
    pub(crate) fn partial(&self) -> bool {
        self.hits.iter().any(Option::is_some) && self.hits.iter().any(Option::is_none)
    }

    /// The body that asks the upstream for the elements that missed only.
    pub(crate) fn reduced(&self) -> Value {
        let mut body = self.body.clone();
        if let (Some(o), Some(all)) = (
            body.as_object_mut(),
            self.body.get("input").and_then(Value::as_array),
        ) {
            let missing: Vec<Value> = all
                .iter()
                .zip(&self.hits)
                .filter(|(_, h)| h.is_none())
                .map(|(v, _)| v.clone())
                .collect();
            o.insert("input".into(), Value::Array(missing));
        }
        body
    }

    fn missing(&self) -> Vec<usize> {
        (0..self.hits.len())
            .filter(|i| self.hits.get(*i).is_some_and(Option::is_none))
            .collect()
    }
}

/// The elements of an embeddings request that is cached item by item.
pub(super) fn elements(body: &Value) -> Option<&Vec<Value>> {
    body.get("input")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
}

/// Looks every element up; `None` when a lookup failed (the caller reports the failure).
pub(super) async fn lookup(
    cx: &Ctx,
    doc_of: impl Fn(&Value) -> String,
    scope: &str,
    body: &Value,
    read: bool,
) -> Option<Items> {
    let all = elements(body)?;
    let keys: Vec<String> = all
        .iter()
        .map(|item| {
            let mut one = body.clone();
            if let Some(o) = one.as_object_mut() {
                o.insert("input".into(), item.clone());
            }
            doc_of(&one)
        })
        .collect();
    let now_ms = cx.rt.now().0 / 1000;
    let hits = if read {
        let cache = &cx.rt.state.cache;
        let found = join_all(keys.iter().map(|k| cache.lookup(scope, k, now_ms))).await;
        let mut hits = Vec::with_capacity(found.len());
        for f in found {
            match f {
                Ok(e) => hits.push(e.and_then(|e| item_of(&e))),
                Err(e) => {
                    tracing::debug!(error = %e, "cache lookup failed");
                    return None;
                }
            }
        }
        hits
    } else {
        vec![None; keys.len()]
    };
    Some(Items {
        keys,
        hits,
        body: body.clone(),
    })
}

fn item_of(e: &Entry) -> Option<Value> {
    serde_json::from_str::<Value>(e.body.as_deref()?).ok()
}

/// Every element hit: the answer built from the cache, as a hit.
pub(super) fn answer(cx: &Ctx, inp: &Inputs<'_>, items: &Items, key: &str) -> Option<Lookup> {
    let data: Vec<Value> = items
        .hits
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let h = h.as_ref()?;
            Some(json!({"object": "embedding", "index": i, "embedding": h.get("embedding")?}))
        })
        .collect::<Option<_>>()?;
    let model = items
        .hits
        .iter()
        .flatten()
        .find_map(|h| h.get("model").and_then(Value::as_str))
        .unwrap_or(inp.model)
        .to_owned();
    let body = json!({"object": "list", "data": data, "model": model,
                      "usage": {"prompt_tokens": 0, "total_tokens": 0}});
    let first = items.hits.iter().flatten().next()?;
    let now_ms = cx.rt.now().0 / 1000;
    let entry = Entry {
        written_at_ms: first
            .get("written_at_ms")
            .and_then(Value::as_i64)
            .unwrap_or(now_ms),
        ttl_ms: 0,
        content_type: "application/json".to_owned(),
        body: Some(body.to_string()),
        events: None,
        provider: text(first, "provider"),
        deployment: text(first, "deployment"),
        model: text(first, "upstream_model"),
        upstream_status: 200,
        upstream_dialect: None,
        usage: Some(json!({"prompt_tokens": 0, "total_tokens": 0})),
        meta: None,
    };
    let resp = replay::respond(cx, inp, &entry, key, now_ms, None)?;
    cx.rt.state.cache.count(true);
    record(cx, Status::Hit, Some(key));
    Some(Lookup::Hit(resp))
}

fn text(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or_default().to_owned()
}

/// Puts an upstream answer for the elements that missed back together with the hits, in input
/// order, and stores each computed element. Anything but a 200 JSON answer of the right length
/// passes through unchanged.
pub(crate) async fn finish(
    cx: &Ctx,
    slot: Option<&CacheSlot>,
    items: &Items,
    resp: Response<Body>,
) -> Response<Body> {
    if resp.status() != http::StatusCode::OK {
        return resp;
    }
    let (mut parts, body) = resp.into_parts();
    let Ok(collected) = body.collect().await else {
        return Response::from_parts(parts, crate::ctx::empty());
    };
    let bytes = collected.to_bytes();
    let parsed = (bytes.len() <= MAX_ANSWER)
        .then(|| serde_json::from_slice::<Value>(&bytes).ok())
        .flatten();
    let missing = items.missing();
    let now_ms = cx.rt.now().0 / 1000;
    let merged = parsed.and_then(|v| merge(items, &missing, &parts.headers, v, now_ms));
    let out = match merged {
        Some((m, stored)) => {
            parts.headers.remove(http::header::CONTENT_LENGTH);
            if let Some(slot) = slot.filter(|s| s.store_after_fallback || served_by_first(cx)) {
                for (key, item) in stored {
                    write(cx, slot, &key, &item, now_ms).await;
                }
            }
            bytes::Bytes::from(m)
        }
        None => bytes,
    };
    Response::from_parts(parts, full(out))
}

/// The answer came from the deployment the request first targeted: elements a fallback computed
/// are stored only when the policy stores answers after a fallback, as whole entries are.
fn served_by_first(cx: &Ctx) -> bool {
    cx.log
        .read(|r| {
            same_deployment(
                r.attempts.first().map(|a| a.deployment.as_str()),
                r.serving_attempt().map(|a| a.deployment.as_str()),
            )
        })
        .unwrap_or(true)
}

fn same_deployment(first: Option<&str>, served: Option<&str>) -> bool {
    match (first, served) {
        (Some(f), Some(s)) => f == s,
        _ => true,
    }
}

/// One element the upstream computed: its key and what is stored under it.
type Computed = (String, Value);

/// The merged body, and each computed element with its key.
fn merge(
    items: &Items,
    missing: &[usize],
    headers: &http::HeaderMap,
    mut v: Value,
    now_ms: i64,
) -> Option<(Vec<u8>, Vec<Computed>)> {
    let computed = v.get("data")?.as_array()?.clone();
    if computed.len() != missing.len() {
        return None;
    }
    let mut ordered = computed.clone();
    ordered.sort_by_key(|d| d.get("index").and_then(Value::as_u64).unwrap_or(0));
    let model = v
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let header = |n: &str| {
        headers
            .get(n)
            .and_then(|h| h.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    let (provider, deployment, upstream_model) = (
        header(antirion_core::headers::PROVIDER),
        header(antirion_core::headers::DEPLOYMENT),
        header(antirion_core::headers::MODEL),
    );
    let mut data: Vec<Value> = Vec::with_capacity(items.hits.len());
    let mut stored = Vec::new();
    let mut fresh = ordered.iter().zip(missing);
    for (i, hit) in items.hits.iter().enumerate() {
        let embedding = match hit {
            Some(h) => h.get("embedding")?.clone(),
            None => {
                let (d, at) = fresh.next()?;
                if *at != i {
                    return None;
                }
                let e = d.get("embedding")?.clone();
                if let Some(k) = items.keys.get(i) {
                    stored.push((
                        k.clone(),
                        json!({"embedding": e, "model": model, "provider": provider,
                               "deployment": deployment, "upstream_model": upstream_model,
                               "written_at_ms": now_ms}),
                    ));
                }
                e
            }
        };
        data.push(json!({"object": "embedding", "index": i, "embedding": embedding}));
    }
    if let Some(o) = v.as_object_mut() {
        o.insert("data".into(), Value::Array(data));
    }
    Some((serde_json::to_vec(&v).ok()?, stored))
}

/// Stores one element on the response cache's backend.
async fn write(cx: &Ctx, slot: &CacheSlot, key: &str, item: &Value, now_ms: i64) {
    let text = item.to_string();
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > slot.max_entry_size {
        store::skipped(&cx.rt);
        return;
    }
    let entry = Entry {
        written_at_ms: now_ms,
        ttl_ms: u64::try_from(slot.ttl.as_millis()).unwrap_or(u64::MAX),
        content_type: "application/json".to_owned(),
        body: Some(text),
        events: None,
        provider: item
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        deployment: item
            .get("deployment")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        model: item
            .get("upstream_model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        upstream_status: 200,
        upstream_dialect: None,
        usage: None,
        meta: None,
    };
    let mut one = slot.clone();
    key.clone_into(&mut one.key);
    one.semantic = None;
    one.flight = None;
    store::write(&cx.rt, &one, entry).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(hits: Vec<Option<Value>>) -> Items {
        let n = hits.len();
        Items {
            keys: (0..n).map(|i| format!("k{i}")).collect(),
            hits,
            body: json!({"model": "m", "input": (0..n).map(|i| format!("t{i}")).collect::<Vec<_>>()}),
        }
    }

    #[test]
    fn elements_a_fallback_computed_are_not_the_first_deployments() {
        assert!(same_deployment(Some("a"), Some("a")));
        assert!(!same_deployment(Some("a"), Some("b")));
        assert!(same_deployment(None, None));
    }

    #[test]
    fn the_answer_is_merged_in_input_order() {
        let it = items(vec![None, Some(json!({"embedding": [1.0]})), None]);
        assert!(it.partial() && !it.all_hit());
        assert_eq!(it.reduced()["input"], json!(["t0", "t2"]));
        let upstream = json!({"object": "list", "model": "up", "usage": {"prompt_tokens": 2, "total_tokens": 2},
            "data": [{"object": "embedding", "index": 1, "embedding": [2.0]},
                     {"object": "embedding", "index": 0, "embedding": [0.0]}]});
        let (body, stored) = merge(&it, &it.missing(), &http::HeaderMap::new(), upstream, 7).unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        let vectors: Vec<&Value> = v["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| &d["embedding"])
            .collect();
        assert_eq!(vectors, [&json!([0.0]), &json!([1.0]), &json!([2.0])]);
        assert_eq!(v["usage"]["prompt_tokens"], 2);
        let keys: Vec<&str> = stored.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["k0", "k2"]);
        assert_eq!(stored[1].1["embedding"], json!([2.0]));
    }

    #[test]
    fn an_answer_of_the_wrong_length_is_not_merged() {
        let it = items(vec![None, None]);
        let upstream = json!({"data": [{"index": 0, "embedding": [0.0]}]});
        assert!(merge(&it, &it.missing(), &http::HeaderMap::new(), upstream, 7).is_none());
    }
}
