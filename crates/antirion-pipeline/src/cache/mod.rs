//! Stage `cache_lookup` of the exact response cache: the request's directives and the policy, the
//! status of requests that are not looked up, the canonical key, and the backend lookup that fails
//! open. A hit is answered here (`replay`); storable responses are written by `store` and, for
//! streams, by the relay's `Tee`.

mod assemble;
mod facts;
pub(crate) mod items;
mod keydoc;
pub(crate) mod prompt;
mod replay;
mod semantic;
mod store;
mod tee;

use std::sync::Arc;
use std::time::Duration;

use antirion_cache::flights::{Flight, Join};
use antirion_cache::{CachePolicy, Directives, EntryMeta, Refusal, Status, reasons};
use antirion_core::{Stage, headers as h};
use antirion_dialects::Endpoint;
use antirion_error::{GatewayError, Message};
use antirion_identity::KeyPermission;
use antirion_obs::metrics::Fam;
use antirion_providers::DeploymentRt;
use antirion_runtime::identity::KeyRt;
use http::request::Parts;
use http::{HeaderName, HeaderValue, Response};
use serde_json::Value;

use crate::ctx::{Body, Ctx};

pub(crate) use store::store_response;
pub(crate) use tee::Tee;

/// The request header that carries the directives.
pub(crate) const DIRECTIVES: &str = "x-antirion-cache-control";
const STATUS: &str = "x-antirion-cache";
const KEY: &str = "x-antirion-cache-key";
const TYPE: &str = "x-antirion-cache-type";
const AGE: &str = "x-antirion-cache-age";
const SIMILARITY: &str = "x-antirion-cache-similarity";

/// Where a response may be stored, and for how long.
#[derive(Clone, Debug)]
pub(crate) struct CacheSlot {
    /// The digest of the key's tenant scope, part of the backend key.
    pub(crate) scope: String,
    pub(crate) key: String,
    pub(crate) ttl: Duration,
    pub(crate) max_entry_size: u64,
    pub(crate) store_after_fallback: bool,
    /// The vector that indexes the response in the semantic store.
    pub(crate) semantic: Option<semantic::SemanticWrite>,
    /// The leadership of identical concurrent misses, released once the response is written or
    /// found unstorable.
    pub(crate) flight: Option<Arc<Flight>>,
    /// What the entry records about the request.
    pub(crate) meta: Box<EntryMeta>,
}

/// What the lookup reads about the request.
pub(crate) struct Inputs<'a> {
    pub(crate) parts: &'a Parts,
    pub(crate) endpoint: &'a Endpoint,
    /// The JSON body as parsed, before extensions and input guardrails changed it.
    pub(crate) body: Option<&'a Value>,
    pub(crate) key: &'a KeyRt,
    pub(crate) end_user: Option<&'a str>,
    /// The Model the request resolved to.
    pub(crate) model: &'a str,
    /// The named route whose policies apply.
    pub(crate) policy_route: Option<&'a str>,
    /// The route the request resolved to.
    pub(crate) route: &'a str,
    /// Every deployment the route can reach.
    pub(crate) reachable: &'a [Arc<DeploymentRt>],
    /// The merged `policies.cache`.
    pub(crate) policy: &'a Value,
    /// The guardrails that apply to the request, as their stage's fingerprint input.
    pub(crate) guardrails: &'a [u8],
    /// The input stage replaced values with reversible tokens: the response it gets holds them
    /// again, so it is neither looked up nor stored.
    pub(crate) tokenised: bool,
    pub(crate) stream: bool,
    pub(crate) wants_usage: bool,
    /// The `policies.cache` member of every level, with the level's name, broadest first.
    pub(crate) levels: &'a [(&'a str, Option<&'a Value>)],
    pub(crate) tags: &'a [String],
    /// The request's `x-antirion-metadata` object.
    pub(crate) metadata: Option<&'a Value>,
}

pub(crate) enum Lookup {
    Hit(Response<Body>),
    /// Continue to the upstream, storing the response in the slot when it is storable.
    Miss(Option<CacheSlot>),
    /// An embeddings request cached item by item: only the elements that missed go upstream, and
    /// each computed element is stored under the slot's settings.
    Items(Option<CacheSlot>, Box<items::Items>),
}

/// The canonical operation name of a cacheable endpoint.
fn operation(endpoint: &Endpoint) -> Option<&'static str> {
    if endpoint.action.is_some() {
        return None;
    }
    Some(match endpoint.id {
        "chat_completions" => "openai.chat_completions",
        "completions" => "openai.completions",
        "responses" => "openai.responses",
        "messages" => "anthropic.messages",
        "embeddings" => "openai.embeddings",
        "rerank" => "rerank",
        _ => return None,
    })
}

/// Responses requests that depend on server state.
fn stateful(op: &str, body: &Value) -> bool {
    op == "openai.responses"
        && (body.get("previous_response_id").is_some_and(|v| !v.is_null())
            || body.get("conversation").is_some_and(|v| !v.is_null())
            || body.get("background").and_then(Value::as_bool) == Some(true))
}

fn directive_error(refusal: Refusal) -> GatewayError {
    let e = match refusal {
        Refusal::Invalid(name) => GatewayError::with_reason(
            reasons::INVALID_CACHE_DIRECTIVE,
            Stage::CacheLookup,
            Message::built(format!("The cache directive `{name}` is not valid.")),
        ),
        Refusal::NotOverridable(name) => GatewayError::with_reason(
            antirion_identity::reasons::OVERRIDE_NOT_ALLOWED,
            Stage::CacheLookup,
            Message::built(format!("The cache policy does not let requests set `{name}`.")),
        ),
        Refusal::NeedsRelax(name) => GatewayError::with_reason(
            antirion_identity::reasons::PERMISSION_REQUIRED,
            Stage::CacheLookup,
            Message::built(format!(
                "Relaxing the cache policy with `{name}` needs the `override:relax` permission."
            )),
        ),
        Refusal::TtlAboveMax => GatewayError::with_reason(
            reasons::CACHE_TTL_ABOVE_MAX,
            Stage::CacheLookup,
            Message::Fixed("The `ttl` cache directive is above the policy's max_ttl."),
        ),
    };
    e.param(DIRECTIVES)
}

/// The request's directives (several header lines read as one list).
fn directives(parts: &Parts) -> Result<Directives, GatewayError> {
    let values: Vec<&str> = parts
        .headers
        .get_all(DIRECTIVES)
        .iter()
        .map(|v| v.to_str().unwrap_or("\u{fffd}"))
        .collect();
    if values.is_empty() {
        return Ok(Directives::default());
    }
    Directives::parse(&values.join(",")).map_err(directive_error)
}

/// The cache fields locked at any level that applies to the request.
fn locked(cx: &Ctx, inp: &Inputs<'_>) -> Vec<String> {
    let doc = &cx.snap.config.doc;
    let at = |collection: &str, name: &str| doc.get(collection)?.get(name)?.pointer("/policies/cache/locked");
    let key = inp.key;
    let mut levels = vec![
        doc.pointer("/policies/cache/locked"),
        at("organizations", &key.organization),
        at("teams", &key.team),
        at("projects", &key.project),
        at("keys", &key.name),
        at("models", inp.model),
    ];
    if let Some(r) = inp.policy_route {
        levels.push(at("routes", r));
    }
    levels
        .into_iter()
        .flatten()
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

/// Records the stage's outcome on the request.
fn record(cx: &Ctx, status: Status, key: Option<&str>) {
    crate::observe::cache_lookup(cx, status.as_str(), key);
}

/// Stage `cache_lookup`, with its span when the cache applied to the request.
pub(crate) async fn lookup(cx: &Ctx, inp: &Inputs<'_>) -> Result<Lookup, GatewayError> {
    let started = std::time::Instant::now();
    let result = look(cx, inp).await;
    let status = cx.log.read(|r| r.cache.status).flatten();
    if let Some(status) = status.filter(|s| *s != Status::Disabled.as_str()) {
        crate::reqlog::span(
            cx,
            crate::reqlog::InternalSpan::new(
                "antirion.cache_lookup".to_owned(),
                started,
                std::time::Instant::now(),
            )
            .attr("antirion.cache.status", status),
        );
    }
    result
}

async fn look(cx: &Ctx, inp: &Inputs<'_>) -> Result<Lookup, GatewayError> {
    let d = directives(inp.parts)?;
    if !CachePolicy::enabled_in(inp.policy) {
        record(cx, Status::Disabled, None);
        return Ok(Lookup::Miss(None));
    }
    let policy = CachePolicy::from_value(inp.policy);
    if policy.opt_in && !d.opt_in {
        record(cx, Status::Disabled, None);
        return Ok(Lookup::Miss(None));
    }
    let can_relax = inp.key.permissions.has(KeyPermission::OverrideRelax);
    d.check(&policy, &locked(cx, inp), can_relax)
        .map_err(directive_error)?;
    let op = operation(inp.endpoint).filter(|op| policy.covers(op));
    let (Some(op), Some(body)) = (op, inp.body) else {
        record(cx, Status::Uncacheable, None);
        return Ok(Lookup::Miss(None));
    };
    if inp.tokenised || stateful(op, body) || policy.too_many_messages(op, body) {
        record(cx, Status::Uncacheable, None);
        return Ok(Lookup::Miss(None));
    }
    if d.bypasses() {
        record(cx, Status::Bypass, None);
        return Ok(Lookup::Miss(None));
    }
    let doc = keydoc::build(cx, inp, &policy, &d, op, body);
    let key = doc.key();
    let scope = keydoc::scope_digest(&doc.scope);
    facts::metadata_rules(cx, inp, &scope);
    let ttl = d.ttl.map_or(policy.ttl, Duration::from_secs);
    let meta = facts::meta(cx, inp, &policy, &scope);
    let mut slot = (!d.no_store && !ttl.is_zero() && !policy.read_only).then(|| CacheSlot {
        scope: scope.clone(),
        key: key.clone(),
        ttl,
        max_entry_size: policy.max_entry_size,
        store_after_fallback: policy.store_after_fallback,
        semantic: None,
        flight: None,
        meta: Box::new(meta.clone()),
    });
    if policy.read_only {
        cx.log.update(|r| r.cache.write = Some("skipped_read_only"));
    }
    if op == "openai.embeddings" && policy.per_item_embeddings && items::elements(body).is_some() {
        let doc_of = |b: &Value| keydoc::build(cx, inp, &policy, &d, op, b).key();
        let Some(found) = items::lookup(cx, doc_of, &scope, body, !d.no_cache).await else {
            return failed(cx, &policy, &key, None);
        };
        if !d.no_cache
            && found.all_hit()
            && let Some(hit) = items::answer(cx, inp, &found, &key)
        {
            return Ok(hit);
        }
        if d.no_cache {
            record(cx, Status::Refresh, Some(&key));
        } else {
            cx.rt.state.cache.count(false);
            record(cx, Status::Miss, Some(&key));
        }
        return Ok(Lookup::Items(slot, Box::new(found)));
    }
    if d.no_cache {
        record(cx, Status::Refresh, Some(&key));
        return Ok(Lookup::Miss(slot));
    }
    let cache = &cx.rt.state.cache;
    let now_ms = cx.rt.now().0 / 1000;
    let started = std::time::Instant::now();
    let found = cache.lookup(&scope, &key, now_ms).await;
    cx.rt.metrics.observe(
        Fam::CacheLookupDuration,
        &["exact"],
        started.elapsed().as_secs_f64(),
    );
    let entry = match found {
        Ok(entry) => entry,
        Err(e) => {
            tracing::debug!(error = %e, "cache lookup failed");
            return failed(cx, &policy, &key, slot);
        }
    };
    let young_enough = |written: i64| {
        d.max_age.is_none_or(|n| {
            u64::try_from(now_ms.saturating_sub(written)).unwrap_or(0) < n.saturating_mul(1000)
        })
    };
    if let Some(entry) = entry.filter(|e| young_enough(e.written_at_ms))
        && let Some(resp) = replay::respond(cx, inp, &entry, &key, now_ms, None)
    {
        cache.count(true);
        record(cx, Status::Hit, Some(&key));
        facts::count_hit(cx, &key).await;
        return Ok(Lookup::Hit(resp));
    }
    if let (Some(max_wait), Some(s)) = (policy.coalesce, slot.as_mut()) {
        match cache.flights.join(&format!("{scope}:{key}")) {
            Join::Lead(flight) => s.flight = Some(Arc::new(flight)),
            Join::Follow(follower) => {
                if follower.wait(max_wait).await
                    && let Ok(Some(entry)) = cache.lookup(&scope, &key, cx.rt.now().0 / 1000).await
                    && let Some(resp) = replay::respond(cx, inp, &entry, &key, cx.rt.now().0 / 1000, None)
                {
                    cache.count(true);
                    record(cx, Status::Hit, Some(&key));
                    facts::count_hit(cx, &key).await;
                    return Ok(Lookup::Hit(resp));
                }
            }
        }
    }
    if let Some((sem, q)) = semantic::query(&policy, op, body) {
        let started = std::time::Instant::now();
        let partition = semantic::partition(&doc, op, &q, &policy);
        let outcome = semantic::lookup(cx, &policy, &d, sem, &q, partition, now_ms).await;
        cx.rt.metrics.observe(
            Fam::CacheLookupDuration,
            &["semantic"],
            started.elapsed().as_secs_f64(),
        );
        match outcome {
            semantic::Semantic::Hit {
                entry,
                similarity,
                key: served,
            } => {
                if let Some(resp) = replay::respond(cx, inp, &entry, &key, now_ms, Some(similarity)) {
                    if let Some(s) = &slot {
                        let mut copy = s.clone();
                        copy.meta.origin = Some(served.clone());
                        store::write(&cx.rt, &copy, (*entry).clone()).await;
                    }
                    cache.count(true);
                    record(cx, Status::Hit, Some(&key));
                    cx.log.update(|r| r.cache.similarity = Some(similarity));
                    facts::count_hit(cx, &served).await;
                    let group = entry
                        .meta
                        .as_ref()
                        .and_then(|m| m.semantic_group.clone())
                        .or_else(|| meta.semantic_group.clone());
                    if let Some(group) = group {
                        cx.rt.semantic_hit(&group, similarity);
                    }
                    return Ok(Lookup::Hit(resp));
                }
            }
            semantic::Semantic::Miss(write) => {
                if let Some(s) = slot.as_mut() {
                    s.semantic = Some(write);
                }
            }
            semantic::Semantic::Failed => return failed(cx, &policy, &key, slot),
        }
    }
    cache.count(false);
    record(cx, Status::Miss, Some(&key));
    Ok(Lookup::Miss(slot))
}

/// A lookup the backend, the embedding call or the vector store failed, with `cache` degraded: a
/// miss that still writes, or under `fail_mode: closed` the refusal of the request.
fn failed(
    cx: &Ctx,
    policy: &CachePolicy,
    key: &str,
    slot: Option<CacheSlot>,
) -> Result<Lookup, GatewayError> {
    record(cx, Status::Error, Some(key));
    cx.log.update(|r| {
        if !r.degraded.iter().any(|(c, _)| *c == "cache") {
            r.degraded.push(("cache", Some("cache_unavailable")));
        }
    });
    if policy.fail_closed {
        drop(slot);
        let mut e = GatewayError::with_reason(
            reasons::CACHE_UNAVAILABLE,
            Stage::CacheLookup,
            Message::Fixed("The response cache could not be read, and the cache policy fails closed."),
        );
        e.retry_after = Some(Duration::from_secs(1));
        return Err(e);
    }
    Ok(Lookup::Miss(slot))
}

/// Adds the cache headers of a request that reached the lookup, unless the response has them.
pub(crate) fn decorate(cx: &Ctx, resp: &mut Response<Body>) {
    let Some((Some(status), key)) = cx.log.read(|r| (r.cache.status, r.cache.key.clone())) else {
        return;
    };
    let headers = resp.headers_mut();
    if headers.contains_key(STATUS) {
        return;
    }
    if let Ok(v) = HeaderValue::from_str(status) {
        headers.insert(HeaderName::from_static(STATUS), v);
    }
    if matches!(status, "hit" | "miss" | "refresh")
        && let Some(v) = key.and_then(|k| HeaderValue::from_str(&k).ok())
    {
        headers.insert(HeaderName::from_static(KEY), v);
    }
    if status == "error" {
        let value = match headers.get(h::DEGRADED).and_then(|v| v.to_str().ok()) {
            Some(existing) if !existing.is_empty() => format!("{existing}, cache"),
            _ => "cache".to_owned(),
        };
        if let Ok(v) = HeaderValue::from_str(&value) {
            headers.insert(HeaderName::from_static(h::DEGRADED), v);
        }
    }
}
