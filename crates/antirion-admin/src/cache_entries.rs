//! The cache's entries on the console: `GET /cache/entries` (filters, sort, keyset pages, totals),
//! `GET /cache/entries/{key}` (the cached response only for readers of content) and
//! `POST /cache/entries/{key}:report_false_positive`. Entries are scoped like request logs: a
//! caller sees those of the organizations, teams and projects where it holds `cache:admin`.

use antirion_cache::listing::{ListQuery, Listed, Position, SortKey};
use antirion_cache::meta::dim;
use antirion_core::UtcMicros;
use antirion_core::audit::Outcome;
use antirion_identity::rbac::Scope;
use antirion_limits::cost;
use antirion_limits::money::{Dec, Nusd};
use antirion_limits::usage::{Units, Usage};
use antirion_store::logs::ScopeRef;
use axum::response::Response;
use http::{HeaderName, HeaderValue, Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;
use crate::{logquery, logs, pagination};

const PERMISSION: &str = "cache:admin";
const CONTENT: &str = "logs:read_content";
const CURSOR_SCOPE: &str = "cache/entries";
const FILTERS: [&str; 7] = [
    "model",
    "route",
    "deployment",
    "organization",
    "team",
    "project",
    "key",
];

fn time(ms: i64) -> String {
    UtcMicros(ms.saturating_mul(1000)).to_rfc3339_micros()
}

/// What `hits` hits of `item` would have been billed: its usage at the current price of the
/// deployment that produced it, one request's unit charges each.
fn saved(call: &Call, item: &Listed) -> String {
    let snap = &call.snap;
    let priced = snap.providers.deployments.get(&item.deployment).and_then(|d| {
        let p = snap
            .pricing
            .price(d.provider.kind.as_str(), &d.base_model, &item.deployment)?;
        let usage = Usage {
            input: item.meta.input_tokens,
            output: item.meta.output_tokens,
            ..Usage::default()
        };
        let units = Units {
            requests: 1,
            ..Units::default()
        };
        cost::cost(&p, &usage, &units, None, Dec::ONE)
            .ok()
            .map(|c| c.billed)
    });
    let each = priced.map_or(0, |n| n.0);
    Nusd(each.saturating_mul(i128::from(item.hits))).to_string()
}

fn item(call: &Call, i: &Listed) -> Value {
    json!({
        "key": i.key,
        "mode": i.meta.mode.as_str(),
        "model": i.meta.model,
        "route": i.meta.route,
        "deployment": i.deployment,
        "scope": i.meta.scope,
        "size_bytes": i.size_bytes,
        "input_tokens": i.meta.input_tokens,
        "output_tokens": i.meta.output_tokens,
        "hits": i.hits,
        "created_at": time(i.created_ms),
        "expires_at": time(i.expires_ms),
        "last_hit_at": i.last_hit_ms.map(time),
        "similarity_threshold": i.meta.similarity_threshold,
        "saved_usd": saved(call, i),
    })
}

fn chain(i: &Listed) -> Vec<Scope> {
    vec![
        Scope::Gateway,
        Scope::Organization(i.meta.organization.clone()),
        Scope::Team(i.meta.team.clone()),
        Scope::Project(i.meta.project.clone()),
    ]
}

/// Whether the caller may see the entry: shared-namespace entries only at gateway scope.
fn visible(call: &Call, i: &Listed) -> bool {
    if i.meta.scope == "shared" {
        return call.require(PERMISSION, &[Scope::Gateway]).is_ok();
    }
    let c = chain(i);
    call.principal.sees(&c) && call.require(PERMISSION, &c).is_ok()
}

fn unavailable() -> AdminError {
    AdminError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "dependency_unavailable",
        "The cache backend is unavailable.",
    )
}

fn sort_of(call: &Call) -> Result<(SortKey, bool), AdminError> {
    let sort = match call.param("sort") {
        None => SortKey::LastHit,
        Some(s) => SortKey::parse(s).ok_or_else(|| {
            AdminError::invalid_parameter(
                "sort must be one of last_hit_at, created_at, expires_at, hits, size",
            )
        })?,
    };
    let descending = match call.param("order") {
        None | Some("desc") => true,
        Some("asc") => false,
        Some(_) => return Err(AdminError::invalid_parameter("order must be asc or desc")),
    };
    Ok((sort, descending))
}

/// `GET /cache/entries`.
pub(crate) async fn list(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&[
        "limit",
        "cursor",
        "model",
        "route",
        "deployment",
        "organization",
        "team",
        "project",
        "key",
        "mode",
        "q",
        "sort",
        "order",
        "include_total",
    ])?;
    logs::require_some(call, PERMISSION)?;
    let (sort, descending) = sort_of(call)?;
    let (limit, after) = pagination::page_params(call, CURSOR_SCOPE)?;
    let after = match after {
        None => None,
        Some(p) => Some(
            Position::decode(&p).ok_or_else(|| AdminError::invalid_parameter("the cursor is not valid"))?,
        ),
    };
    let mut all_of: Vec<String> = FILTERS
        .iter()
        .filter_map(|f| call.param(f).map(|v| dim(f, v)))
        .collect();
    match call.param("mode") {
        None => {}
        Some(m @ ("exact" | "semantic")) => all_of.push(dim("mode", m)),
        Some(_) => return Err(AdminError::invalid_parameter("mode must be exact or semantic")),
    }
    let prefix = call.param("q").filter(|q| !q.is_empty()).map(str::to_owned);
    if prefix
        .as_deref()
        .is_some_and(|q| !q.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(AdminError::invalid_parameter(
            "q is a prefix of a cache key: hexadecimal digits",
        ));
    }
    let any_of = logquery::scopes(&call.principal, PERMISSION).map(|scopes| {
        scopes
            .iter()
            .map(|s| match s {
                ScopeRef::Organization(n) => dim("organization", n),
                ScopeRef::Team(n) => dim("team", n),
                ScopeRef::Project(n) => dim("project", n),
            })
            .collect::<Vec<_>>()
    });
    let include_total = call.bool_param("include_total", false)?;
    let q = ListQuery {
        all_of,
        any_of,
        prefix: prefix.map(|p| p.to_ascii_lowercase()),
        sort,
        descending,
        after,
        limit,
        totals: include_total,
        now_ms: call.rt.now().0 / 1000,
    };
    let listing = match call.rt.state.cache.list(&q).await {
        Ok(l) => l,
        Err(e) => {
            tracing::debug!(error = %e, "the cache index could not be read");
            return Ok(call.json(
                StatusCode::OK,
                &json!({"items": [], "next_cursor": null, "enumerable": false}),
            ));
        }
    };
    let items: Vec<Value> = listing
        .items
        .iter()
        .filter(|i| visible(call, i))
        .map(|i| item(call, i))
        .collect();
    let next = listing
        .next
        .map(|p| pagination::encode(&call.rt.cursor_key, CURSOR_SCOPE, &p.encode()));
    let mut body = json!({"items": items, "next_cursor": next, "enumerable": true});
    if let Some(t) = listing.totals {
        body["total"] = json!(t.count);
        body["total_size_bytes"] = json!(t.bytes);
        body["total_hits"] = json!(t.hits);
    }
    Ok(call.json(StatusCode::OK, &body))
}

fn checked_key(key: &str) -> Result<(), AdminError> {
    if antirion_cache::key::well_formed(key) {
        Ok(())
    } else {
        Err(AdminError::invalid_parameter(
            "a cache key is 64 lowercase hexadecimal digits",
        ))
    }
}

/// The cached response: the body, or the stream's events when only those were stored.
fn response(entry: &antirion_cache::Entry) -> Value {
    let parse = |s: &str| serde_json::from_str::<Value>(s).unwrap_or_else(|_| Value::from(s));
    match (&entry.body, &entry.events) {
        (Some(b), _) => parse(b),
        (None, Some(events)) => Value::Array(events.iter().map(|e| parse(&e.data)).collect()),
        (None, None) => Value::Null,
    }
}

/// `GET /cache/entries/{key}`.
pub(crate) async fn get(call: &Call, key: &str) -> Result<Response, AdminError> {
    call.allow_params(&[])?;
    logs::require_some(call, PERMISSION)?;
    checked_key(key)?;
    let now_ms = call.rt.now().0 / 1000;
    let found = call
        .rt
        .state
        .cache
        .read(key, now_ms)
        .await
        .map_err(|_| unavailable())?;
    let Some((listed, Some(entry))) = found.filter(|(l, _)| visible(call, l)) else {
        return Err(AdminError::not_found());
    };
    let mut body = item(call, &listed);
    let content = call.require(CONTENT, &chain(&listed)).is_ok();
    body["response"] = if content { response(&entry) } else { Value::Null };
    let mut resp = call.json(StatusCode::OK, &body);
    if !content {
        resp.headers_mut().insert(
            HeaderName::from_static("x-antirion-fields-redacted"),
            HeaderValue::from_static("response"),
        );
    }
    Ok(resp)
}

/// `POST /cache/entries/{key}:report_false_positive`: records that a semantic hit served a wrong
/// answer and drops the entry, and the entry it was copied from.
pub(crate) async fn report_false_positive(call: &Call, key: &str) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&[])?;
    logs::require_some(call, PERMISSION)?;
    checked_key(key)?;
    let cache = &call.rt.state.cache;
    let now_ms = call.rt.now().0 / 1000;
    let found = cache.read(key, now_ms).await.map_err(|_| unavailable())?;
    let Some((listed, _)) = found.filter(|(l, _)| visible(call, l)) else {
        return Err(AdminError::not_found());
    };
    let (true, Some(group)) = (listed.meta.semantic(), listed.meta.semantic_group.clone()) else {
        return Err(AdminError::conflict(
            "conflict",
            "The entry did not come from the semantic cache.",
            Vec::new(),
        ));
    };
    for k in std::iter::once(key).chain(listed.meta.origin.as_deref()) {
        cache.delete(k, now_ms).await.map_err(|_| unavailable())?;
    }
    call.rt.semantic_report(&group);
    call.rt.flush_semantic_tallies().await;
    call.audit(
        "cache.report_false_positive",
        &format!("cache/entries/{key}"),
        Outcome::Success,
        StatusCode::NO_CONTENT,
    )
    .await;
    Ok(call.no_content())
}
