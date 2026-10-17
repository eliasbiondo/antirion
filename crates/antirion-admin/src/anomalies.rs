//! `GET /anomalies`, `GET /anomalies/{id}` and `:dismiss`: key anomaly flags, each visible to who
//! may read its key and dismissed by who may update it.

use antirion_identity::rbac::Scope;
use antirion_runtime::people::anomalies::flag_json;
use antirion_store::{AnomalyFilter, AnomalyRow};
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;
use crate::pagination;

const KINDS: [&str; 5] = ["spike", "network", "user_agent", "country", "exposed"];
const STATES: [&str; 2] = ["open", "dismissed"];
const MAX_REASON: usize = 500;

/// The scopes of a flag's key: its project's, as the key stood when flagged when the project is gone.
fn scopes(call: &Call, a: &AnomalyRow) -> Vec<Scope> {
    if call.snap.identity.projects.contains_key(&a.project) {
        call.snap.identity.project_scopes(&a.project)
    } else {
        vec![
            Scope::Gateway,
            Scope::Organization(a.organization.clone()),
            Scope::Team(a.team.clone()),
            Scope::Project(a.project.clone()),
        ]
    }
}

fn may(call: &Call, permission: &str, a: &AnomalyRow) -> bool {
    call.principal
        .allows_owned(permission, &scopes(call, a), a.owner.as_deref())
}

async fn list(call: &Call) -> Result<Response, AdminError> {
    call.allow_params(&["limit", "cursor", "key", "user", "state", "kind"])?;
    let (limit, after) = pagination::page_params(call, "anomalies")?;
    let state = call.param("state").map(str::to_owned);
    if state.as_deref().is_some_and(|s| !STATES.contains(&s)) {
        return Err(AdminError::invalid_parameter("state must be open or dismissed"));
    }
    let kinds: Vec<String> = call
        .query
        .iter()
        .filter(|(k, _)| k == "kind")
        .map(|(_, v)| v.clone())
        .collect();
    if let Some(bad) = kinds.iter().find(|k| !KINDS.contains(&k.as_str())) {
        return Err(AdminError::invalid_parameter(format!(
            "kind {bad:?} is not one of {}",
            KINDS.join(", ")
        )));
    }
    let mut filter = AnomalyFilter {
        key: call.param("key").map(str::to_owned),
        owner: call.param("user").map(str::to_owned),
        state,
        kinds,
        before: after,
        limit: u64::try_from(limit.saturating_add(1)).unwrap_or(u64::MAX),
    };
    let mut items: Vec<AnomalyRow> = Vec::new();
    let mut more = false;
    loop {
        let page = call
            .rt
            .store
            .anomalies(&filter)
            .await
            .map_err(|_| AdminError::store_unavailable())?;
        let exhausted = u64::try_from(page.len()).unwrap_or(u64::MAX) < filter.limit;
        filter.before = page.last().map(|a| a.id.clone());
        for a in page {
            if !may(call, "keys:read", &a) {
                continue;
            }
            if items.len() == limit {
                more = true;
                break;
            }
            items.push(a);
        }
        if more || exhausted || filter.before.is_none() {
            break;
        }
    }
    let next = more
        .then(|| {
            items
                .last()
                .map(|a| pagination::encode(&call.rt.cursor_key, "anomalies", &a.id))
        })
        .flatten();
    let body: Vec<Value> = items.iter().map(flag_json).collect();
    Ok(call.json(StatusCode::OK, &json!({"items": body, "next_cursor": next})))
}

/// A flag the caller can see with `permission`, else not found.
async fn visible(call: &Call, id: &str, permission: &str) -> Result<AnomalyRow, AdminError> {
    let a = call
        .rt
        .store
        .anomaly(id)
        .await
        .map_err(|_| AdminError::store_unavailable())?
        .ok_or_else(AdminError::not_found)?;
    let chain = scopes(call, &a);
    if !call.principal.sees(&chain) {
        return Err(AdminError::not_found());
    }
    call.require_owned(permission, &chain, a.owner.as_deref())
        .map_err(|e| e.with_resource(format!("anomalies/{id}")))?;
    Ok(a)
}

async fn dismiss(call: &Call, id: &str) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&[])?;
    let reason = if call.body.is_empty() {
        None
    } else {
        match call.json_body()?.get("reason") {
            None | Some(Value::Null) => None,
            Some(Value::String(r)) if r.chars().count() <= MAX_REASON => Some(r.clone()),
            Some(_) => {
                return Err(AdminError::invalid_parameter(
                    "reason must be a string of at most 500 characters",
                ));
            }
        }
    };
    let flag = visible(call, id, "keys:update").await?;
    let resource = format!("keys/{}", flag.key_name);
    match call
        .rt
        .dismiss_flag(id, &call.principal.id, reason.as_deref())
        .await
    {
        Some(true) => {}
        Some(false) => {
            return Err(AdminError::conflict(
                "already_exists",
                "The anomaly flag is already dismissed.",
                Vec::new(),
            ));
        }
        None => return Err(AdminError::store_unavailable()),
    }
    call.audit_with(
        "keys.anomaly_dismissed",
        &resource,
        antirion_core::audit::Outcome::Success,
        StatusCode::OK,
        json!({"anomaly": id, "reason": reason}),
    )
    .await;
    let after = visible(call, id, "keys:read").await?;
    Ok(call.json(StatusCode::OK, &flag_json(&after)))
}

/// Routes `/anomalies` and `/anomalies/{id}[:dismiss]`.
pub(crate) async fn handle(call: &Call, segment: Option<&str>) -> Result<Response, AdminError> {
    match segment {
        None => {
            if call.method != Method::GET {
                return Err(AdminError::method_not_allowed("GET"));
            }
            list(call).await
        }
        Some(s) => match s.strip_suffix(":dismiss") {
            Some(id) => dismiss(call, id).await,
            None => {
                if call.method != Method::GET {
                    return Err(AdminError::method_not_allowed("GET"));
                }
                call.allow_params(&[])?;
                let a = visible(call, s, "keys:read").await?;
                Ok(call.json(StatusCode::OK, &flag_json(&a)))
            }
        },
    }
}
