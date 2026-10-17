//! The response cache: `DELETE /cache/entries/{key}`, `POST /cache:invalidate` (one level's epoch,
//! or the cutoff of a finer level) and `GET /runtime/cache` (backend health, the node's counts and
//! the semantic statistics).

use antirion_cache::Cache;
use antirion_cache::invalidate::Target;
use antirion_config::scalar::parse_duration;
use antirion_config::{ConfigError, DetailCode, DocPath};
use antirion_core::audit::Outcome;
use antirion_identity::rbac::Scope;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;

const PERMISSION: &str = "cache:admin";

fn guard(call: &Call, method: Method, allow: &'static str) -> Result<(), AdminError> {
    if call.method != method {
        return Err(AdminError::method_not_allowed(allow));
    }
    call.allow_params(&[])?;
    call.require(PERMISSION, &[Scope::Gateway])
}

/// `DELETE /cache/entries/{key}`.
pub(crate) async fn delete_entry(call: &Call, key: &str) -> Result<Response, AdminError> {
    guard(call, Method::DELETE, "DELETE")?;
    if !antirion_cache::key::well_formed(key) {
        return Err(AdminError::invalid_parameter(
            "a cache key is 64 lowercase hexadecimal digits",
        ));
    }
    let now_ms = call.rt.now().0 / 1000;
    let existed = call
        .rt
        .state
        .cache
        .delete(key, now_ms)
        .await
        .map_err(|_| cache_unavailable())?;
    if !existed {
        return Err(AdminError::not_found());
    }
    call.audit(
        "cache.delete",
        &format!("cache/entries/{key}"),
        Outcome::Success,
        StatusCode::NO_CONTENT,
    )
    .await;
    Ok(call.no_content())
}

fn cache_unavailable() -> AdminError {
    AdminError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "dependency_unavailable",
        "The cache backend is unavailable.",
    )
}

/// Whether the level's resource exists (the default tenants always do).
fn exists(call: &Call, level: &str, name: &str) -> bool {
    let collection = match level {
        "organization" => "organizations",
        "team" => "teams",
        "project" => "projects",
        "route" => "routes",
        "model" => "models",
        "key" => "keys",
        _ => return true,
    };
    matches!(level, "organization" | "team" | "project") && name == antirion_config::model::DEFAULT_TENANT
        || call
            .snap
            .config
            .doc
            .get(collection)
            .and_then(|c| c.get(name))
            .is_some()
}

const LEVELS: &str = "level must be gateway, organization, team, project, route, model, key, end_user, tag, \
                      metadata or older_than";

/// An invalidation request: the level, its name, and what it selects with its cutoff.
struct Request {
    level: &'static str,
    name: Option<String>,
    target: Target,
    cutoff_ms: i64,
}

fn request(call: &Call, now_ms: i64) -> Result<Request, AdminError> {
    let body = call.json_body()?;
    let invalid = |field: &str, code: DetailCode, message: &str| {
        AdminError::validation(vec![ConfigError::new(&DocPath::root().key(field), code, message)])
    };
    let level: &'static str = match body.get("level").and_then(Value::as_str) {
        Some("gateway") => "gateway",
        Some("organization") => "organization",
        Some("team") => "team",
        Some("project") => "project",
        Some("route") => "route",
        Some("model") => "model",
        Some("key") => "key",
        Some("end_user") => "end_user",
        Some("tag") => "tag",
        Some("metadata") => "metadata",
        Some("older_than") => "older_than",
        _ => return Err(invalid("level", DetailCode::InvalidEnum, LEVELS)),
    };
    if level == "gateway" {
        return Ok(Request {
            level,
            name: None,
            target: Target::All,
            cutoff_ms: now_ms,
        });
    }
    if level == "older_than" {
        let d = match body.get("duration") {
            None => {
                return Err(invalid(
                    "duration",
                    DetailCode::Required,
                    "older_than needs a duration",
                ));
            }
            Some(v) => v
                .as_str()
                .and_then(parse_duration)
                .filter(|d| !d.is_zero())
                .ok_or_else(|| {
                    invalid(
                        "duration",
                        DetailCode::InvalidFormat,
                        "duration must be a duration above 0s",
                    )
                })?,
        };
        let ms = i64::try_from(d.as_millis()).unwrap_or(i64::MAX);
        return Ok(Request {
            level,
            name: None,
            target: Target::OlderThan,
            cutoff_ms: now_ms.saturating_sub(ms),
        });
    }
    let Some(name) = body.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()) else {
        return Err(invalid(
            "name",
            DetailCode::Required,
            "name is required for this level",
        ));
    };
    if level == "metadata" && !name.split_once('=').is_some_and(|(k, _)| !k.is_empty()) {
        return Err(invalid(
            "name",
            DetailCode::InvalidFormat,
            "a metadata name is <key>=<value>",
        ));
    }
    if !exists(call, level, name) {
        return Err(AdminError::not_found());
    }
    let target = match level {
        "organization" | "team" | "project" | "route" => Cache::level_target(level, name),
        _ => Target::dim(level, name),
    };
    Ok(Request {
        level,
        name: Some(name.to_owned()),
        target,
        cutoff_ms: now_ms,
    })
}

/// `POST /cache:invalidate`: the levels in the key bump their epoch; the finer ones and `older_than`
/// set a cutoff every node applies. Either way the response does not wait for the entries to be
/// removed from the backend, which a tracked task does.
pub(crate) async fn invalidate(call: &Call) -> Result<Response, AdminError> {
    guard(call, Method::POST, "POST")?;
    let now_ms = call.rt.now().0 / 1000;
    let r = request(call, now_ms)?;
    if r.level == "gateway"
        && let Some(held) = Box::pin(crate::change_control::second_person(call, "cache_flush_all")).await?
    {
        return Ok(held);
    }
    let cache = &call.rt.state.cache;
    let epoch = cache.epochs.bump(r.level, r.name.as_deref().unwrap_or_default());
    if let Some(key) = r.target.cutoff_key() {
        cache.epochs.raise(&key, u64::try_from(r.cutoff_ms).unwrap_or(0));
    }
    call.rt.persist_cache_epochs().await;
    let rt = std::sync::Arc::clone(&call.rt);
    let (target, cutoff) = (r.target.clone(), r.cutoff_ms);
    call.rt.tasks.spawn(async move {
        if let Err(e) = rt.state.cache.remove(&target, cutoff).await {
            tracing::debug!(error = %e, "invalidated cache entries could not all be removed");
        }
    });
    let resource = match (&r.name, r.level) {
        (Some(n), l) => format!("cache/{l}/{n}"),
        (None, "older_than") => format!("cache/older_than/{}", r.cutoff_ms),
        (None, _) => "cache/gateway".to_owned(),
    };
    call.audit("cache.invalidate", &resource, Outcome::Success, StatusCode::OK)
        .await;
    Ok(call.json(
        StatusCode::OK,
        &json!({"level": r.level, "name": r.name, "epoch": epoch}),
    ))
}

/// `GET /runtime/cache`.
pub(crate) async fn status(call: &Call) -> Result<Response, AdminError> {
    guard(call, Method::GET, "GET")?;
    let h = call.rt.state.cache.health(call.rt.now().0 / 1000).await;
    if !h.healthy {
        return Ok(call.json(
            StatusCode::SERVICE_UNAVAILABLE,
            &json!({"backend": h.backend, "healthy": false}),
        ));
    }
    let semantic: Vec<Value> = call
        .rt
        .semantic_stats()
        .await
        .into_iter()
        .map(|s| {
            json!({"scope": s.scope, "embedding_model": s.embedding_model, "threshold": s.threshold,
                   "entries": s.entries, "hits": s.hits,
                   "mean_similarity_of_hits": s.mean_similarity_of_hits.map(|m| (m * 10_000.0).round() / 10_000.0),
                   "false_positive_reports": s.false_positive_reports,
                   "false_positive_rate": s.false_positive_rate.map(|r| (r * 10_000.0).round() / 10_000.0)})
        })
        .collect();
    Ok(call.json(
        StatusCode::OK,
        &json!({"backend": h.backend, "healthy": true, "entries": h.entries, "bytes": h.bytes,
                "hits": h.hits, "misses": h.misses, "semantic": semantic}),
    ))
}
