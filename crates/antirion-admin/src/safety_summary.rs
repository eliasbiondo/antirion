//! `GET /reports/guardrails/summary`: event counts over the events list's selection, per group of
//! up to two dimensions and per time bucket, with the matches (entity counts) of each group and how
//! many of its events share their fingerprint with another event of the selection.

use std::collections::{BTreeMap, HashMap};

use antirion_store::{SafetyEventRow, SafetyFilter};
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;
use crate::safety::{SELECTION, selection};
use crate::{analytics, logquery, pagination};

const SCOPE: &str = "reports/guardrails/summary";
const DIMENSIONS: [&str; 6] = [
    "category",
    "severity",
    "action",
    "guardrail",
    "entity",
    "review_state",
];
/// The most events one summary reads.
const MAX_EVENTS: usize = 200_000;
const BATCH: usize = 5_000;

/// The value of a dimension other than `entity`.
fn value_of(e: &SafetyEventRow, dim: &str) -> Option<String> {
    Some(match dim {
        "category" => e.category.clone(),
        "severity" => e.severity.clone(),
        "action" => e.action.clone(),
        "guardrail" => e.guardrail.clone(),
        "review_state" => e.review_state.clone(),
        _ => return None,
    })
}

/// The entity types of an event with their match counts.
fn entities_of(e: &SafetyEventRow) -> Vec<(String, u64)> {
    serde_json::from_str::<Value>(&e.entities)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|x| {
            Some((
                x.get("type")?.as_str()?.to_owned(),
                x.get("count").and_then(Value::as_u64).unwrap_or(0),
            ))
        })
        .collect()
}

fn group_by(call: &Call) -> Result<Vec<&'static str>, AdminError> {
    let mut out: Vec<&'static str> = Vec::new();
    for (_, v) in call.query.iter().filter(|(k, _)| k == "group_by") {
        for name in v.split(',').filter(|s| !s.is_empty()) {
            let d = DIMENSIONS
                .iter()
                .find(|d| **d == name)
                .ok_or_else(|| AdminError::invalid_parameter(format!("group_by does not accept {name:?}")))?;
            if !out.contains(d) {
                out.push(d);
            }
        }
    }
    if out.len() > 2 {
        return Err(AdminError::invalid_parameter(
            "group_by accepts at most 2 dimensions",
        ));
    }
    Ok(out)
}

/// Every event of the selection the caller may read.
async fn events(call: &Call, f: &SafetyFilter) -> Result<Vec<SafetyEventRow>, AdminError> {
    call.rt.read_safety().await;
    let mut out = Vec::new();
    let mut after = None;
    loop {
        let page = call
            .rt
            .store
            .list_safety_events(f, after, BATCH)
            .await
            .map_err(|_| AdminError::store_unavailable())?;
        let full = page.len() == BATCH;
        after = page.last().map(|e| (e.time_us, e.id.clone()));
        out.extend(page.into_iter().filter(|e| {
            let c = crate::safety::chain(e);
            call.principal.sees(&c) && call.require("logs:read", &c).is_ok()
        }));
        if out.len() > MAX_EVENTS {
            return Err(AdminError::invalid_parameter(
                "the selection holds more than 200000 events; narrow it",
            ));
        }
        if !full {
            return Ok(out);
        }
    }
}

#[derive(Default)]
struct Cell {
    events: u64,
    related: u64,
    matches: u64,
}

pub(crate) async fn summary(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    logquery::allow(call, &SELECTION, &["group_by", "interval", "limit", "cursor"])?;
    crate::logs::require_some(call, "logs:read")?;
    let (limit, after) = pagination::page_params(call, SCOPE)?;
    let offset = after
        .map(|a| {
            a.parse::<usize>()
                .map_err(|_| AdminError::invalid_parameter("the cursor is not valid"))
        })
        .transpose()?
        .unwrap_or(0);
    let dims = group_by(call)?;
    let interval = call
        .param("interval")
        .map(|v| {
            analytics::interval_us(v).ok_or_else(|| {
                AdminError::invalid_parameter("interval must be one of 1m, 5m, 15m, 1h, 6h, 1d, hour, day")
            })
        })
        .transpose()?;
    let mut f = selection(call)?;
    let starts = match interval {
        Some(step) => {
            let (from, to) = analytics::range(call)?;
            f.from_us = Some(from);
            f.to_us = Some(to);
            Some((analytics::starts(from, to, Some(step), None)?, step))
        }
        None => None,
    };
    let all = events(call, &f).await?;
    let mut seen: HashMap<&str, u64> = HashMap::new();
    for e in &all {
        *seen.entry(e.fingerprint.as_str()).or_default() += 1;
    }
    let mut cells: BTreeMap<(Option<i64>, Vec<Option<String>>), Cell> = BTreeMap::new();
    for e in &all {
        let start = match &starts {
            Some((list, step)) => {
                let Some(first) = list.first() else { continue };
                let i = (e.time_us - first).div_euclid(*step);
                let Some(s) = usize::try_from(i).ok().and_then(|i| list.get(i)) else {
                    continue;
                };
                Some(*s)
            }
            None => None,
        };
        let ents = entities_of(e);
        let mut keys: Vec<(Vec<Option<String>>, u64)> = vec![(Vec::new(), ents.iter().map(|(_, c)| c).sum())];
        for d in &dims {
            let mut next = Vec::new();
            for (k, m) in keys {
                if *d == "entity" {
                    if ents.is_empty() {
                        let mut k2 = k.clone();
                        k2.push(None);
                        next.push((k2, 0));
                    }
                    for (t, c) in &ents {
                        let mut k2 = k.clone();
                        k2.push(Some(t.clone()));
                        next.push((k2, *c));
                    }
                } else {
                    let mut k2 = k;
                    k2.push(value_of(e, d));
                    next.push((k2, m));
                }
            }
            keys = next;
        }
        let related = seen.get(e.fingerprint.as_str()).is_some_and(|n| *n > 1);
        for (k, m) in keys {
            let c = cells.entry((start, k)).or_default();
            c.events += 1;
            c.related += u64::from(related);
            c.matches += m;
        }
    }
    let rows: Vec<Value> = cells
        .into_iter()
        .skip(offset)
        .take(limit + 1)
        .map(|((start, values), c)| {
            let group: serde_json::Map<String, Value> = dims
                .iter()
                .zip(values)
                .map(|(d, v)| ((*d).to_owned(), json!(v)))
                .collect();
            json!({
                "group": group,
                "start": start.map(|t| antirion_core::UtcMicros(t).to_rfc3339()),
                "events": c.events,
                "related": c.related,
                "matches": c.matches,
            })
        })
        .collect();
    let more = rows.len() > limit;
    let items: Vec<Value> = rows.into_iter().take(limit).collect();
    let next_cursor =
        more.then(|| pagination::encode(&call.rt.cursor_key, SCOPE, &(offset + limit).to_string()));
    Ok(call.json(
        StatusCode::OK,
        &json!({"items": items, "next_cursor": next_cursor}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entities_are_read_with_their_counts() {
        let e = SafetyEventRow {
            entities: r#"[{"type":"email","count":2},{"type":"phone","count":1}]"#.into(),
            ..SafetyEventRow::default()
        };
        assert_eq!(
            entities_of(&e),
            [("email".to_owned(), 2), ("phone".to_owned(), 1)]
        );
        assert_eq!(value_of(&e, "entity"), None);
    }
}
