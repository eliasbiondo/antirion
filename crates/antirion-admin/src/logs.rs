//! The request log: `GET /logs` (the selection of `logquery`, sorted, with totals), `GET /logs/{id}`
//! and `GET /logs/schema`. A record is visible to principals with `logs:read` in its scope; records
//! outside the caller's scope are neither listed nor fetchable. Content, attempt error bodies and
//! guardrail scores and spans need `logs:read_content`.

use antirion_identity::rbac::Scope;
use antirion_runtime::reqlog::analytics::metadata_matches;
use antirion_runtime::reqlog::joined;
use antirion_store::logs::{Arg, ListedLog, LogOrder, SortKey, SortPos};
use axum::response::Response;
use http::{HeaderName, HeaderValue, Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;
use crate::logquery::{self, SELECTION, Selection};
use crate::pagination;

const SCOPE: &str = "logs";
const REDACTED_HEADER: &str = "x-antirion-fields-redacted";
/// The most records a total counts exactly.
const TOTAL_CAP: u64 = 100_000;
/// Records read per store call while metadata filters are applied.
const SCAN_BATCH: usize = 500;

pub(crate) fn scope_of(record: &Value) -> Vec<Scope> {
    let name = |field: &str| record.get(field).and_then(Value::as_str).map(str::to_owned);
    let mut chain = vec![Scope::Gateway];
    chain.extend(name("organization").map(Scope::Organization));
    chain.extend(name("team").map(Scope::Team));
    chain.extend(name("project").map(Scope::Project));
    chain
}

pub(crate) fn readable(call: &Call, record: &Value) -> bool {
    let chain = scope_of(record);
    call.principal.sees(&chain) && call.require("logs:read", &chain).is_ok()
}

/// The fields nulled for a reader without `logs:read_content`, as `x-antirion-fields-redacted`
/// names them.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Redacted {
    pub(crate) content: bool,
    pub(crate) scores: bool,
}

impl Redacted {
    pub(crate) fn add(&mut self, other: Redacted) {
        self.content |= other.content;
        self.scores |= other.scores;
    }

    pub(crate) fn header(self) -> Option<&'static str> {
        match (self.content, self.scores) {
            (false, _) => None,
            (true, false) => Some("content,error_body"),
            (true, true) => Some("content,error_body,guardrails.score,guardrails.spans"),
        }
    }
}

/// Nulls what a reader without `logs:read_content` may not see.
pub(crate) fn redact(r: &mut Value) -> Redacted {
    antirion_obs::sink::strip_content(r);
    let mut scores = false;
    if let Some(gs) = r.get_mut("guardrails").and_then(Value::as_array_mut) {
        for g in gs.iter_mut().filter_map(Value::as_object_mut) {
            for field in ["score", "spans"] {
                if g.contains_key(field) {
                    g.insert(field.into(), Value::Null);
                    scores = true;
                }
            }
        }
    }
    Redacted {
        content: true,
        scores,
    }
}

/// The record as `call` may see it, and what was nulled.
pub(crate) fn shown(call: &Call, record: &Value) -> (Value, Redacted) {
    let mut r = record.clone();
    if call.require("logs:read_content", &scope_of(record)).is_ok() {
        return (r, Redacted::default());
    }
    let red = redact(&mut r);
    (r, red)
}

fn redacted(call: &Call, status: StatusCode, body: &Value, red: Redacted) -> Response {
    let mut resp = call.json(status, body);
    if let Some(h) = red.header() {
        resp.headers_mut().insert(
            HeaderName::from_static(REDACTED_HEADER),
            HeaderValue::from_static(h),
        );
    }
    resp
}

/// Whether `q` may search stored prompts: the caller reads content wherever it reads records.
pub(crate) fn searches_content(call: &Call) -> bool {
    match (
        logquery::scopes(&call.principal, "logs:read"),
        logquery::scopes(&call.principal, "logs:read_content"),
    ) {
        (_, None) => true,
        (Some(read), Some(content)) => !read.is_empty() && read.iter().all(|s| content.contains(s)),
        (None, Some(_)) => false,
    }
}

/// Refuses a caller with no `logs:read` binding at all.
pub(crate) fn require_some(call: &Call, permission: &str) -> Result<(), AdminError> {
    if call.principal.bindings.iter().any(|b| b.role.allows(permission)) {
        return Ok(());
    }
    call.require(permission, &[Scope::Gateway])
}

/// The sort and order of `call`.
pub(crate) fn order_of(call: &Call) -> Result<(SortKey, bool), AdminError> {
    let sort = match call.param("sort") {
        None => SortKey::StartedAt,
        Some(s) => SortKey::parse(s).ok_or_else(|| {
            AdminError::invalid_parameter(
                "sort must be one of started_at, latency_ms, ttft_ms, cost_usd, input_tokens, output_tokens, status_code",
            )
        })?,
    };
    let desc = match call.param("order") {
        None | Some("desc") => true,
        Some("asc") => false,
        Some(_) => return Err(AdminError::invalid_parameter("order must be asc or desc")),
    };
    Ok((sort, desc))
}

fn encode_pos(sort: SortKey, desc: bool, p: &SortPos) -> String {
    let value = match &p.value {
        Arg::Int(i) => format!("i{i}"),
        Arg::Real(r) => format!("r{}", r.to_bits()),
        Arg::Text(t) => format!("t{t}"),
    };
    format!(
        "{}\u{1f}{}\u{1f}{value}\u{1f}{}\u{1f}{}",
        sort.name(),
        desc,
        p.started_us,
        p.id
    )
}

fn decode_pos(sort: SortKey, desc: bool, s: &str) -> Option<SortPos> {
    let mut parts = s.splitn(5, '\u{1f}');
    if parts.next()? != sort.name() || parts.next()? != desc.to_string() {
        return None;
    }
    let value = parts.next()?;
    let value = match value.split_at_checked(1)? {
        ("i", v) => Arg::Int(v.parse().ok()?),
        ("r", v) => Arg::Real(f64::from_bits(v.parse().ok()?)),
        _ => return None,
    };
    Some(SortPos {
        value,
        started_us: parts.next()?.parse().ok()?,
        id: parts.next()?.to_owned(),
    })
}

/// Up to `limit` matching records from `order`'s position, with the metadata filters applied.
pub(crate) async fn page(
    call: &Call,
    sel: &Selection,
    mut order: LogOrder,
    limit: usize,
) -> Result<Vec<ListedLog>, AdminError> {
    let mut out = Vec::new();
    loop {
        order.limit = if sel.metadata.is_empty() {
            limit + 1 - out.len()
        } else {
            SCAN_BATCH
        };
        let batch = call
            .rt
            .list_logs(sel.filter.clone(), order.clone())
            .await
            .map_err(|_| AdminError::store_unavailable())?;
        let full = batch.len() == order.limit;
        order.after = batch.last().map(|l| l.pos.clone());
        for l in batch {
            let keep = sel.metadata.is_empty()
                || serde_json::from_str::<Value>(&l.log.record)
                    .is_ok_and(|r| metadata_matches(&r, &sel.metadata));
            if keep {
                out.push(l);
                if out.len() > limit {
                    return Ok(out);
                }
            }
        }
        if !full {
            return Ok(out);
        }
    }
}

async fn total(call: &Call, sel: &Selection) -> Result<(u64, bool), AdminError> {
    if sel.metadata.is_empty() {
        let n = call
            .rt
            .count_logs(sel.filter.clone(), TOTAL_CAP)
            .await
            .map_err(|_| AdminError::store_unavailable())?;
        return Ok((n.min(TOTAL_CAP), n <= TOTAL_CAP));
    }
    let order = LogOrder {
        sort: SortKey::StartedAt,
        desc: true,
        after: None,
        limit: SCAN_BATCH,
    };
    let cap = usize::try_from(TOTAL_CAP).unwrap_or(usize::MAX);
    let n = page(call, sel, order, cap).await?.len();
    let n = u64::try_from(n).unwrap_or(u64::MAX);
    Ok((n.min(TOTAL_CAP), n <= TOTAL_CAP))
}

pub(crate) async fn list(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    logquery::allow(
        call,
        SELECTION,
        &["limit", "cursor", "sort", "order", "include_total"],
    )?;
    require_some(call, "logs:read")?;
    let (limit, after) = pagination::page_params(call, SCOPE)?;
    let (sort, desc) = order_of(call)?;
    let after = after
        .map(|a| {
            decode_pos(sort, desc, &a).ok_or_else(|| AdminError::invalid_parameter("the cursor is not valid"))
        })
        .transpose()?;
    let include_total = call.bool_param("include_total", false)?;
    let sel = logquery::parse(call, "logs:read", searches_content(call))?;
    let order = LogOrder {
        sort,
        desc,
        after,
        limit,
    };
    let mut rows = page(call, &sel, order, limit).await?;
    let more = rows.len() > limit;
    rows.truncate(limit);
    let mut any_redacted = Redacted::default();
    let items: Vec<Value> = rows
        .iter()
        .map(|l| joined(&l.log))
        .filter(|r| readable(call, r))
        .map(|r| {
            let (v, red) = shown(call, &r);
            any_redacted.add(red);
            v
        })
        .collect();
    let next_cursor = rows
        .last()
        .filter(|_| more)
        .map(|l| pagination::encode(&call.rt.cursor_key, SCOPE, &encode_pos(sort, desc, &l.pos)));
    let mut body = json!({"items": items, "next_cursor": next_cursor});
    if include_total {
        let (n, exact) = total(call, &sel).await?;
        body["total"] = json!(n);
        body["total_exact"] = json!(exact);
    }
    Ok(redacted(call, StatusCode::OK, &body, any_redacted))
}

pub(crate) async fn get(call: &Call, id: &str) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&[])?;
    let record = call
        .rt
        .stored_log(id)
        .await
        .map_err(|_| AdminError::store_unavailable())?
        .ok_or_else(AdminError::not_found)?;
    let chain = scope_of(&record);
    if !call.principal.sees(&chain) {
        return Err(AdminError::not_found());
    }
    call.require("logs:read", &chain)?;
    let (mut body, red) = shown(call, &record);
    crate::logfeedback::attach(call, id, &mut body).await?;
    if !red.content && !record.get("content").is_none_or(Value::is_null) {
        call.audit_read("logs.read_content", &format!("logs/{id}")).await;
    }
    Ok(redacted(call, StatusCode::OK, &body, red))
}

/// The JSON Schema of a record.
pub(crate) fn schema(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&[])?;
    require_some(call, "logs:read")?;
    Ok(call.json(StatusCode::OK, &antirion_obs::record::schema()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_positions_round_trip_for_their_sort_only() {
        let p = SortPos {
            value: Arg::Real(12.5),
            started_us: 1_790_000_000_000_000,
            id: "01926f5e-7c1a".into(),
        };
        let s = encode_pos(SortKey::LatencyMs, true, &p);
        assert_eq!(decode_pos(SortKey::LatencyMs, true, &s), Some(p.clone()));
        assert_eq!(decode_pos(SortKey::LatencyMs, false, &s), None);
        assert_eq!(decode_pos(SortKey::CostUsd, true, &s), None);
        assert_eq!(decode_pos(SortKey::StartedAt, true, "x"), None);
    }

    #[test]
    fn scores_and_spans_are_nulled_without_content() {
        let mut r = serde_json::json!({"content": {"request": 1}, "attempts": [],
            "guardrails": [{"name": "g", "score": 0.9, "spans": [{"start": 0, "end": 3}]}, {"name": "h"}]});
        assert_eq!(
            redact(&mut r).header(),
            Some("content,error_body,guardrails.score,guardrails.spans")
        );
        assert!(r["content"].is_null());
        assert!(r["guardrails"][0]["score"].is_null() && r["guardrails"][0]["spans"].is_null());
        assert!(r["guardrails"][1].get("score").is_none());
    }
}
