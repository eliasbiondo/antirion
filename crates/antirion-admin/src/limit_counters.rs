//! `GET /runtime/limits`: the state of the rate and concurrency limit counters the caller may read,
//! filtered by `level`, `owner`, `name` and `partition`, each read without changing it.

use std::time::Duration;

use antirion_config::scalar::format_duration;
use antirion_identity::rbac::Scope;
use antirion_limits::applicable::named_limit;
use antirion_limits::decl::{Algorithm, LimitUnit};
use antirion_limits::level::Level;
use antirion_limits::plan::reading;
use antirion_limits::policy::DeclKind;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::budgets::{chain, parse_partition, project_of, show_partition};
use crate::call::Call;
use crate::error::AdminError;
use crate::pagination;

const SCOPE: &str = "limits";

pub(crate) async fn list(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&["level", "owner", "name", "partition", "limit", "cursor"])?;
    let (limit, after) = pagination::page_params(call, SCOPE)?;
    let level = match call.param("level") {
        Some(l) => Some(Level::parse(l).ok_or_else(|| AdminError::invalid_parameter("unknown level"))?),
        None => None,
    };
    let wanted = call.param("partition").map(parse_partition).transpose()?;
    let policies = &call.snap.limits;
    let now = call.rt.now();
    let mut items = Vec::new();
    for d in policies.all() {
        let DeclKind::Limit(decl) = &d.kind else {
            continue;
        };
        if level.is_some_and(|l| l != d.level)
            || call.param("owner").is_some_and(|o| o != d.owner)
            || call.param("name").is_some_and(|n| n != d.name)
        {
            continue;
        }
        let scopes = chain(call, d.level, &d.owner);
        if !call.principal.sees(&scopes) || call.require("policies:read", &scopes).is_err() {
            continue;
        }
        let organization = scopes
            .iter()
            .find_map(|s| match s {
                Scope::Organization(o) => Some(o.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let partitions = match &wanted {
            Some(p) => vec![p.clone()],
            None if decl.partition.attrs.is_empty() => vec![Vec::new()],
            None => call.rt.accounts.partitions_of(d.level, &d.owner, &d.name),
        };
        let project = project_of(call, d.level, &d.owner);
        for p in partitions {
            let Some(a) = named_limit(
                policies,
                d.level,
                &d.owner,
                &d.name,
                &p,
                project.as_deref(),
                &organization,
            ) else {
                continue;
            };
            let hold = reading(&a, now);
            let view = call
                .rt
                .accounts
                .read(std::slice::from_ref(&hold))
                .await
                .map_err(|_| AdminError::store_unavailable())?;
            let Some(v) = view.first() else { continue };
            let remaining = v.remaining.min(a.quota);
            let per = (decl.per_us > 0).then(|| format_duration(Duration::from_micros(decl.per_us)));
            let resets_at = (decl.per_us > 0 && v.full_in_us > 0).then(|| {
                now.saturating_add(Duration::from_micros(v.full_in_us))
                    .to_rfc3339_seconds()
            });
            items.push(json!({
                "level": d.level.as_str(),
                "owner": d.owner,
                "name": d.name,
                "partition": show_partition(&a.partition),
                "unit": match decl.unit {
                    LimitUnit::Requests { .. } => "requests",
                    LimitUnit::Tokens(_) => "tokens",
                    LimitUnit::Concurrency => "concurrency",
                },
                "algorithm": match decl.algorithm {
                    Algorithm::Gcra => "gcra",
                    Algorithm::SlidingWindow { .. } => "sliding_window",
                },
                "limit": a.quota,
                "per": per,
                "used": a.quota.saturating_sub(remaining),
                "reserved": v.reserved,
                "remaining": remaining,
                "resets_at": resets_at,
            }));
        }
    }
    let start = after.and_then(|a| a.parse::<usize>().ok()).unwrap_or(0);
    let page: Vec<Value> = items.iter().skip(start).take(limit).cloned().collect();
    let next = (start + page.len() < items.len())
        .then(|| pagination::encode(&call.rt.cursor_key, SCOPE, &(start + page.len()).to_string()));
    Ok(call.json(StatusCode::OK, &json!({"items": page, "next_cursor": next})))
}
