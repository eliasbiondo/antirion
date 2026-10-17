//! Access requests for `preview` and `restricted` Models: a team, project, key or organization
//! asks to be added to a Model's access list, and a principal holding `models:write` approves
//! (adding it as one audited configuration change) or denies.

use antirion_config::model::DEFAULT_TENANT;
use antirion_core::RequestId;
use antirion_core::audit::Outcome;
use antirion_identity::rbac::Scope;
use antirion_runtime::changes::Target;
use antirion_store::{PendingRequest, RequestFilter};
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Map, Value, json};

use crate::call::Call;
use crate::commit::{Proposal, api_write};
use crate::error::AdminError;
use crate::pagination;
use crate::perms::{chain, owner};

const KIND: &str = "access";

/// `GET` and `POST` on `/models/{name}/access_requests`.
pub(crate) async fn collection(call: &Call, model: &str) -> Result<Response, AdminError> {
    match call.method {
        Method::GET => list(call, model).await,
        Method::POST => create(call, model).await,
        _ => Err(AdminError::method_not_allowed("GET, POST")),
    }
}

/// `POST /models/{name}/access_requests/{id}:approve` and `:deny`.
pub(crate) async fn action(call: &Call, model: &str, segment: &str) -> Result<Response, AdminError> {
    let Some((id, verb)) = segment.split_once(':') else {
        return Err(AdminError::not_found());
    };
    if !matches!(verb, "approve" | "deny") {
        return Err(AdminError::not_found());
    }
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&[])?;
    call.require("models:write", &[Scope::Gateway])?;
    model_spec(call, model)?;
    let reason = reason(call)?;
    let stored = call
        .rt
        .store
        .request(KIND, id)
        .await
        .map_err(|_| AdminError::store_unavailable())?
        .filter(|r| r.subject == model)
        .ok_or_else(AdminError::not_found)?;
    if stored.state != "open" {
        return Err(AdminError::conflict(
            "already_exists",
            "The access request was already decided.",
            Vec::new(),
        ));
    }
    let mut body: Map<String, Value> = serde_json::from_str(&stored.body).unwrap_or_default();
    let resource = format!("models/{model}/access_requests/{id}");
    let mut generation = None;
    if verb == "approve" {
        let scope = body
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let committed = grant(call, model, &scope, &resource).await?;
        generation = Some(committed.generation);
    }
    let state = if verb == "approve" { "approved" } else { "denied" };
    body.insert("state".into(), json!(state));
    body.insert("decided_by".into(), json!(call.principal.id));
    body.insert("decided_at".into(), json!(call.rt.now().to_rfc3339_seconds()));
    if let Some(r) = reason {
        body.insert("decision_reason".into(), json!(r));
    }
    let text = Value::Object(body.clone()).to_string();
    let swapped = call
        .rt
        .store
        .update_request(KIND, id, stored.version, state, &text)
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    if !swapped {
        return Err(AdminError::conflict(
            "already_exists",
            "The access request was decided concurrently.",
            Vec::new(),
        ));
    }
    if verb == "deny" {
        call.audit_with(
            "POST access_requests:deny",
            &resource,
            Outcome::Success,
            StatusCode::OK,
            json!({"state": "denied"}),
        )
        .await;
    }
    let resp = call.json(StatusCode::OK, &Value::Object(body));
    Ok(match generation {
        Some(g) => call.mutated(resp, g).await,
        None => resp,
    })
}

fn reason(call: &Call) -> Result<Option<String>, AdminError> {
    if call.body.is_empty() {
        return Ok(None);
    }
    let body = call.json_body()?;
    match body.get("reason") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.chars().count() <= 500 => Ok(Some(s.clone())),
        Some(_) => Err(invalid("reason", "reason is a string of at most 500 characters")),
    }
}

fn invalid(field: &str, message: &str) -> AdminError {
    AdminError::validation(vec![antirion_config::ConfigError::new(
        &antirion_config::DocPath::root().key(field),
        antirion_config::DetailCode::InvalidFormat,
        message,
    )])
}

fn model_spec<'a>(call: &'a Call, model: &str) -> Result<&'a Value, AdminError> {
    call.snap
        .config
        .doc
        .pointer("/models")
        .and_then(|m| m.get(model))
        .ok_or_else(AdminError::not_found)
}

/// Adds the scope to the Model's access list as one configuration change.
async fn grant(
    call: &Call,
    model: &str,
    scope: &str,
    resource: &str,
) -> Result<antirion_runtime::Committed, AdminError> {
    let (kind, name) = scope.split_once('/').unwrap_or_default();
    let list = match kind {
        "organization" => "organizations",
        "team" => "teams",
        "project" => "projects",
        _ => "keys",
    };
    let (model, name) = (model.to_owned(), name.to_owned());
    api_write(
        call,
        "POST access_requests:approve",
        resource,
        StatusCode::OK,
        |state| {
            let mut candidate = state.doc.clone();
            let entry = candidate
                .pointer_mut(&format!(
                    "/models/{}",
                    model.replace('~', "~0").replace('/', "~1")
                ))
                .and_then(Value::as_object_mut)
                .ok_or_else(AdminError::not_found)?;
            let access = entry
                .entry("access")
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .ok_or_else(AdminError::internal)?;
            let names = access
                .entry(list)
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .ok_or_else(AdminError::internal)?;
            if !names.iter().any(|n| n.as_str() == Some(name.as_str())) {
                names.push(json!(name));
            }
            Ok(Proposal {
                candidate,
                written: vec![Target::Resource("models", model.clone())],
                credentials: Vec::new(),
                meta_edits: Vec::new(),
            })
        },
    )
    .await
}

async fn list(call: &Call, model: &str) -> Result<Response, AdminError> {
    call.allow_params(&["limit", "cursor"])?;
    call.require("models:read", &[Scope::Gateway])?;
    model_spec(call, model)?;
    let scope = format!("models/{model}/access_requests");
    let (limit, after) = pagination::page_params(call, &scope)?;
    let rows = call
        .rt
        .store
        .requests(
            KIND,
            &RequestFilter {
                subject: Some(model.to_owned()),
                after,
                limit: u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1),
                ..RequestFilter::default()
            },
        )
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    let more = rows.len() > limit;
    let page: Vec<&PendingRequest> = rows.iter().take(limit).collect();
    let next = more
        .then(|| {
            page.last()
                .map(|r| pagination::encode(&call.rt.cursor_key, &scope, &r.id))
        })
        .flatten();
    let items: Vec<Value> = page
        .iter()
        .filter_map(|r| serde_json::from_str(&r.body).ok())
        .collect();
    Ok(call.json(StatusCode::OK, &json!({"items": items, "next_cursor": next})))
}

/// The scope chain of a requested scope and the resource's owner, after checking it exists.
fn scope_target(call: &Call, scope: &str) -> Result<(Vec<Scope>, Option<String>), AdminError> {
    let bad = || {
        invalid(
            "scope",
            "scope is organization/<name>, team/<name>, project/<name> or key/<name>",
        )
    };
    let (kind, name) = scope.split_once('/').ok_or_else(bad)?;
    let collection = match kind {
        "organization" => "organizations",
        "team" => "teams",
        "project" => "projects",
        "key" => "keys",
        _ => return Err(bad()),
    };
    let valid_name = !name.is_empty()
        && name.len() <= 128
        && name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !valid_name {
        return Err(bad());
    }
    let spec = call.snap.config.doc.get(collection).and_then(|c| c.get(name));
    if spec.is_none() && !(name == DEFAULT_TENANT && collection != "keys") {
        return Err(AdminError::validation(vec![antirion_config::ConfigError::new(
            &antirion_config::DocPath::root().key("scope"),
            antirion_config::DetailCode::UnresolvedReference,
            "the scope names nothing that exists",
        )]));
    }
    let scopes = chain(&call.snap, collection, name, spec);
    if !call.principal.sees(&scopes) {
        return Err(AdminError::not_found());
    }
    Ok((scopes, owner(collection, spec).map(str::to_owned)))
}

async fn create(call: &Call, model: &str) -> Result<Response, AdminError> {
    call.allow_params(&[])?;
    let body = call.json_body()?;
    if let Some(k) = body.keys().find(|k| !matches!(k.as_str(), "scope" | "reason")) {
        return Err(invalid(k, "unknown member"));
    }
    let scope = body
        .get("scope")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("scope", "scope is required"))?
        .to_owned();
    let reason = match body.get("reason") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.chars().count() <= 500 => Some(s.clone()),
        Some(_) => return Err(invalid("reason", "reason is a string of at most 500 characters")),
    };
    let (scopes, key_owner) = scope_target(call, &scope)?;
    let allowed = call
        .principal
        .allows_owned("keys:update", &scopes, key_owner.as_deref())
        || (!scope.starts_with("key/") && call.principal.allows("teams:update", &scopes));
    if !allowed {
        return Err(AdminError::forbidden("keys:update or teams:update"));
    }
    model_spec(call, model)?;
    let open = call
        .rt
        .store
        .requests(
            KIND,
            &RequestFilter {
                subject: Some(model.to_owned()),
                states: vec!["open".to_owned()],
                limit: 10_000,
                ..RequestFilter::default()
            },
        )
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    if open.iter().any(|r| {
        serde_json::from_str::<Value>(&r.body)
            .ok()
            .and_then(|b| b.get("scope").cloned())
            == Some(json!(scope))
    }) {
        return Err(AdminError::conflict(
            "already_exists",
            "An open access request for this scope exists.",
            Vec::new(),
        ));
    }
    let now = call.rt.now();
    let id = format!("ar_{}", RequestId::generate().as_str());
    let item = json!({
        "id": id,
        "model": model,
        "scope": scope,
        "reason": reason,
        "requested_by": call.principal.id,
        "requested_at": now.to_rfc3339_seconds(),
        "state": "open",
        "decided_by": null,
        "decided_at": null,
    });
    let row = PendingRequest {
        kind: KIND.to_owned(),
        id: id.clone(),
        subject: model.to_owned(),
        state: "open".to_owned(),
        author: call.principal.id.clone(),
        created_us: now.0,
        version: 0,
        body: item.to_string(),
    };
    call.rt
        .store
        .insert_request(row)
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    let resource = format!("models/{model}/access_requests/{id}");
    call.audit_with(
        "POST access_requests",
        &resource,
        Outcome::Success,
        StatusCode::CREATED,
        json!({"scope": scope}),
    )
    .await;
    let resp = call.json(StatusCode::CREATED, &item);
    Ok(call.created(resp, &format!("/admin/v1/{resource}")))
}
