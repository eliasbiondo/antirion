//! Change requests: a change set (or a destructive action under the two-person rule) proposed by
//! one principal, approved by others holding the permissions it needs, and applied atomically
//! against the configuration it was based on, rebased when what it touches did not move.

use std::time::Duration;

use antirion_core::audit::Outcome;
use antirion_core::{RequestId, UtcMicros};
use antirion_identity::rbac::Scope;
use antirion_runtime::changes::{self, Target};
use antirion_runtime::commit::Mutation;
use antirion_runtime::snapshot::redacted;
use antirion_runtime::state::etag_of;
use antirion_store::{PendingRequest, RequestFilter};
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Map, Value, json};

use crate::call::Call;
use crate::change_control::{Staged, holds, stage};
use crate::change_needs::{covered, encode_needed, needed_from, stored_needed};
use crate::change_secrets;
use crate::error::AdminError;
use crate::pagination;

const KIND: &str = "change";
const STATES: [&str; 7] = [
    "open",
    "approved",
    "rejected",
    "applied",
    "cancelled",
    "expired",
    "conflicted",
];

fn invalid(field: &str, message: &str) -> AdminError {
    AdminError::validation(vec![antirion_config::ConfigError::new(
        &antirion_config::DocPath::root().key(field),
        antirion_config::DetailCode::InvalidFormat,
        message,
    )])
}

fn governance(doc: &Value, pointer: &str) -> Option<Value> {
    doc.pointer(&format!("/governance{pointer}")).cloned()
}

fn duration(doc: &Value, pointer: &str, default: Duration) -> Duration {
    governance(doc, pointer)
        .and_then(|v| v.as_str().and_then(antirion_config::scalar::parse_duration))
        .unwrap_or(default)
}

/// What a reader sees: the stored body without its internal members.
fn shown(body: &Value) -> Value {
    let mut v = body.clone();
    if let Some(o) = v.as_object_mut() {
        o.retain(|k, _| !k.starts_with('_'));
    }
    v
}

/// The stored state, `expired` once an open or approved request has passed its expiry.
fn effective(body: &Value, now: UtcMicros) -> String {
    let state = body.get("state").and_then(Value::as_str).unwrap_or("open");
    let expired = body
        .get("expires_at")
        .and_then(Value::as_str)
        .and_then(|t| UtcMicros::parse_rfc3339(t).ok())
        .is_some_and(|t| now >= t);
    if expired && matches!(state, "open" | "approved") {
        "expired".to_owned()
    } else {
        state.to_owned()
    }
}

/// The redacted diff of a staged change set against `base`, with the staged document's secret
/// paths.
fn diff(
    call: &Call,
    base: &Value,
    staged: &Staged,
) -> Result<(Vec<Value>, Vec<antirion_config::DocPath>), AdminError> {
    let checked = call
        .rt
        .check_unread(staged.candidate.clone(), base)
        .map_err(AdminError::validation)?;
    let old = redacted(base, &call.snap.secret_paths);
    let new = redacted(&staged.candidate, &checked.secret_paths);
    let pick = |doc: &Value, t: &Target| match t {
        Target::Resource(c, name) => antirion_config::model::collection(c)
            .and_then(|c| antirion_config::model::collection_map(doc, c))
            .and_then(|m| m.get(name))
            .cloned(),
        Target::Setting(s) => {
            antirion_config::model::singleton(s).and_then(|s| antirion_config::model::singleton_value(doc, s))
        }
    };
    let diff = changes::compute(base, &staged.candidate)
        .into_iter()
        .map(|(t, op, _, _)| {
            json!({"op": op, "path": t.path(), "before": pick(&old, &t), "after": pick(&new, &t)})
        })
        .collect();
    Ok((diff, checked.secret_paths))
}

/// The hash of each target's current value, which a later apply must still find.
fn fingerprints(doc: &Value, written: &[Target]) -> Map<String, Value> {
    written
        .iter()
        .map(|t| {
            let v = match t {
                Target::Resource(c, name) => antirion_config::model::collection(c)
                    .and_then(|c| antirion_config::model::collection_map(doc, c))
                    .and_then(|m| m.get(name)),
                Target::Setting(s) => doc.get(*s),
            };
            (t.path(), v.map_or(Value::Null, |v| Value::from(etag_of(v))))
        })
        .collect()
}

async fn store(call: &Call, body: &Value, author: &str, now: UtcMicros) -> Result<(), AdminError> {
    let id = body
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    call.rt
        .store
        .insert_request(PendingRequest {
            kind: KIND.to_owned(),
            id,
            subject: String::new(),
            state: "open".to_owned(),
            author: author.to_owned(),
            created_us: now.0,
            version: 0,
            body: body.to_string(),
        })
        .await
        .map_err(|_| AdminError::store_unavailable())
        .map(|_| ())
}

fn event(call: &Call, kind: &str, body: &Value) {
    call.rt.send_alert(
        &call.snap,
        None,
        kind,
        json!({"id": body.get("id"), "kind": body.get("kind"), "title": body.get("title"),
               "author": body.get("author"), "state": body.get("state")}),
    );
}

/// Opens a change request of kind `action` for a call the two-person rule held back.
pub(crate) async fn open_action(call: &Call, action: &str, change: Value) -> Result<Value, AdminError> {
    let now = call.rt.now();
    let ttl = duration(
        &call.snap.config.doc,
        "/dual_control/approval_ttl",
        Duration::from_secs(3600),
    );
    let id = format!("cr_{}", RequestId::generate().as_str());
    let raw = [change];
    let changes = change_secrets::redact_changes(&raw, &call.snap.secret_paths);
    let mut body = json!({
        "id": id,
        "kind": "action",
        "title": format!("Run {action}"),
        "description": "",
        "author": call.principal.person(),
        "_author_credential": call.principal.id,
        "created_at": now.to_rfc3339_seconds(),
        "base_generation": call.snap.generation,
        "changes": changes,
        "diff": [],
        "state": "open",
        "approvals": [],
        "approvals_required": 1,
        "expires_at": now.saturating_add(ttl).to_rfc3339_seconds(),
        "applied_generation": null,
        "_action": action,
    });
    change_secrets::keep(&call.rt.sealer, &mut body, &id, &raw, &changes)?;
    store(call, &body, call.principal.person(), now).await?;
    call.audit_with(
        "POST change_requests",
        &format!("change_requests/{}", body["id"].as_str().unwrap_or_default()),
        Outcome::Success,
        StatusCode::ACCEPTED,
        json!({"action": action}),
    )
    .await;
    event(call, "change_request.opened", &body);
    Ok(shown(&body))
}

/// `GET` and `POST` on `/change_requests`.
pub(crate) async fn collection(call: &Call) -> Result<Response, AdminError> {
    match call.method {
        Method::GET => list(call).await,
        Method::POST => create(call).await,
        _ => Err(AdminError::method_not_allowed("GET, POST")),
    }
}

async fn create(call: &Call) -> Result<Response, AdminError> {
    call.allow_params(&[])?;
    let body = call.json_body()?;
    if let Some(k) = body
        .keys()
        .find(|k| !matches!(k.as_str(), "title" | "description" | "changes"))
    {
        return Err(invalid(k, "unknown member"));
    }
    let title = body
        .get("title")
        .and_then(Value::as_str)
        .filter(|t| (1..=200).contains(&t.chars().count()))
        .ok_or_else(|| invalid("title", "title is 1 to 200 characters"))?;
    let description = match body.get("description") {
        None | Some(Value::Null) => "",
        Some(Value::String(d)) if d.chars().count() <= 5000 => d.as_str(),
        Some(_) => return Err(invalid("description", "description is at most 5000 characters")),
    };
    let list = body
        .get("changes")
        .and_then(Value::as_array)
        .filter(|l| !l.is_empty())
        .ok_or_else(|| invalid("changes", "changes is a non-empty list"))?;
    let base = &call.snap.config.doc;
    let staged = stage(call, base, list)?;
    if !holds(call, &staged.reads) {
        return Err(AdminError::forbidden(
            "read access to every resource the changes touch",
        ));
    }
    let (diff, secret_paths) = diff(call, base, &staged)?;
    let shown_changes = change_secrets::redact_changes(list, &secret_paths);
    let now = call.rt.now();
    let ttl = duration(
        base,
        "/change_control/request_ttl",
        Duration::from_secs(7 * 86_400),
    );
    let required = governance(base, "/change_control/approvals_required")
        .and_then(|v| v.as_u64())
        .unwrap_or(1);
    let id = format!("cr_{}", RequestId::generate().as_str());
    let mut record = json!({
        "id": id,
        "kind": "config",
        "title": title,
        "description": description,
        "author": call.principal.person(),
        "_author_credential": call.principal.id,
        "created_at": now.to_rfc3339_seconds(),
        "base_generation": call.snap.generation,
        "changes": shown_changes,
        "diff": diff,
        "state": "open",
        "approvals": [],
        "approvals_required": required,
        "expires_at": now.saturating_add(ttl).to_rfc3339_seconds(),
        "applied_generation": null,
        "_touched": fingerprints(base, &staged.written),
        "_needed": encode_needed(&staged.permissions),
    });
    change_secrets::keep(&call.rt.sealer, &mut record, &id, list, &shown_changes)?;
    store(call, &record, call.principal.person(), now).await?;
    call.audit_with(
        "POST change_requests",
        &format!("change_requests/{id}"),
        Outcome::Success,
        StatusCode::CREATED,
        json!({"title": title}),
    )
    .await;
    event(call, "change_request.opened", &record);
    let resp = call.json(StatusCode::CREATED, &shown(&record));
    Ok(call.created(resp, &format!("/admin/v1/change_requests/{id}")))
}

async fn list(call: &Call) -> Result<Response, AdminError> {
    call.allow_params(&["limit", "cursor", "state", "author"])?;
    call.require("config:read", &[Scope::Gateway])?;
    let states: Vec<String> = call
        .query
        .iter()
        .filter(|(k, _)| k == "state")
        .map(|(_, v)| v.clone())
        .collect();
    if let Some(bad) = states.iter().find(|s| !STATES.contains(&s.as_str())) {
        return Err(AdminError::invalid_parameter(format!(
            "state {bad:?} is not a change request state"
        )));
    }
    let (limit, after) = pagination::page_params(call, "change_requests")?;
    let now = call.rt.now();
    let mut items = Vec::new();
    let mut next = None;
    let mut cursor = after;
    'pages: loop {
        let rows = call
            .rt
            .store
            .requests(
                KIND,
                &RequestFilter {
                    author: call.param("author").map(str::to_owned),
                    after: cursor.clone(),
                    limit: 500,
                    ..RequestFilter::default()
                },
            )
            .await
            .map_err(|_| AdminError::store_unavailable())?;
        let full = rows.len() == 500;
        for r in &rows {
            cursor = Some(r.id.clone());
            let mut body: Value = serde_json::from_str(&r.body).unwrap_or(Value::Null);
            let state = effective(&body, now);
            if !states.is_empty() && !states.contains(&state) {
                continue;
            }
            if items.len() == limit {
                next = items
                    .last()
                    .and_then(|i: &Value| i.get("id"))
                    .and_then(Value::as_str)
                    .map(|id| pagination::encode(&call.rt.cursor_key, "change_requests", id));
                break 'pages;
            }
            body["state"] = json!(state);
            items.push(shown(&body));
        }
        if !full {
            break;
        }
    }
    Ok(call.json(StatusCode::OK, &json!({"items": items, "next_cursor": next})))
}

async fn load(call: &Call, id: &str) -> Result<(PendingRequest, Value), AdminError> {
    let row = call
        .rt
        .store
        .request(KIND, id)
        .await
        .map_err(|_| AdminError::store_unavailable())?
        .ok_or_else(AdminError::not_found)?;
    let body: Value = serde_json::from_str(&row.body).unwrap_or(Value::Null);
    Ok((row, body))
}

/// `GET /change_requests/{id}` and its actions.
pub(crate) async fn one(call: &Call, segment: &str) -> Result<Response, AdminError> {
    let (id, action) = match segment.split_once(':') {
        Some((id, a)) => (id, Some(a)),
        None => (segment, None),
    };
    let Some(action) = action else {
        if call.method != Method::GET {
            return Err(AdminError::method_not_allowed("GET"));
        }
        call.allow_params(&[])?;
        call.require("config:read", &[Scope::Gateway])?;
        let (_, mut body) = load(call, id).await?;
        body["state"] = json!(effective(&body, call.rt.now()));
        return Ok(call.json(StatusCode::OK, &shown(&body)));
    };
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&[])?;
    let (row, mut body) = load(call, id).await?;
    let now = call.rt.now();
    let state = effective(&body, now);
    let author = body
        .get("author")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    // Requests stored before authors were recorded as people carry the credential's id.
    let mine = author == call.principal.person() || author == call.principal.id;
    let reason = if call.body.is_empty() {
        None
    } else {
        call.json_body()?
            .get("reason")
            .and_then(Value::as_str)
            .map(|r| r.chars().take(500).collect::<String>())
    };
    let open = matches!(state.as_str(), "open" | "approved");
    if !open && change_secrets::forget(&mut body) {
        // A request that can no longer be applied keeps no sealed literals; a lost race here
        // leaves them to the next attempt.
        let _ = call
            .rt
            .store
            .update_request(KIND, &row.id, row.version, &state, &body.to_string())
            .await;
    }
    let closed = || {
        AdminError::conflict(
            "already_exists",
            format!("The change request is {state}."),
            Vec::new(),
        )
    };
    match action {
        "approve" | "reject" => {
            if mine {
                return Err(AdminError::new(
                    StatusCode::FORBIDDEN,
                    "forbidden",
                    "A change request needs a second person: its author cannot approve or reject it, with this \
                     credential or another of theirs.",
                ));
            }
            call.require("config:read", &[Scope::Gateway])?;
            // A change set that no longer stages can still be rejected by who could have
            // approved it when it was opened.
            let needed = match needed(call, &body) {
                Ok(n) => n,
                Err(e) => match (action, stored_needed(&body)) {
                    ("reject", Some(stored)) => stored,
                    _ => return Err(e),
                },
            };
            if !holds(call, &needed) {
                return Err(AdminError::forbidden("every write permission the changes need"));
            }
            if !open {
                return Err(closed());
            }
            if action == "approve" {
                let approvals = body["approvals"]
                    .as_array_mut()
                    .ok_or_else(AdminError::internal)?;
                let person = call.principal.person();
                if !approvals.iter().any(|a| a["by"] == json!(person)) {
                    approvals.push(json!({
                        "by": person,
                        "credential": call.principal.id,
                        "at": now.to_rfc3339_seconds(),
                    }));
                }
                let count = approvals.len() as u64;
                if count >= body["approvals_required"].as_u64().unwrap_or(1) {
                    body["state"] = json!("approved");
                }
            } else {
                body["state"] = json!("rejected");
                body["rejection_reason"] = json!(reason);
                change_secrets::forget(&mut body);
            }
        }
        "cancel" => {
            if !mine {
                return Err(AdminError::forbidden("the author"));
            }
            if !open {
                return Err(closed());
            }
            body["state"] = json!("cancelled");
            change_secrets::forget(&mut body);
        }
        "apply" => {
            // The author too: opening a request needs only read access to what it touches.
            if !holds(call, &needed(call, &body)?) {
                return Err(AdminError::forbidden("every write permission the changes need"));
            }
            if state != "approved" {
                if open {
                    return Err(AdminError::conflict(
                        "change_request_required",
                        "The change request does not have the approvals it needs.",
                        Vec::new(),
                    ));
                }
                return Err(closed());
            }
            return apply(call, row, body).await;
        }
        _ => return Err(AdminError::not_found()),
    }
    let state = body["state"].as_str().unwrap_or("open").to_owned();
    let swapped = call
        .rt
        .store
        .update_request(KIND, &row.id, row.version, &state, &body.to_string())
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    if !swapped {
        return Err(AdminError::conflict(
            "already_exists",
            "The change request changed concurrently.",
            Vec::new(),
        ));
    }
    call.audit_with(
        &format!("POST change_requests:{action}"),
        &format!("change_requests/{id}"),
        Outcome::Success,
        StatusCode::OK,
        json!({"state": state}),
    )
    .await;
    if action == "approve" && state == "approved" {
        event(call, "change_request.approved", &body);
    }
    Ok(call.json(StatusCode::OK, &shown(&body)))
}

/// The write permissions applying the request needs.
fn needed(call: &Call, body: &Value) -> Result<Vec<(String, Vec<Scope>)>, AdminError> {
    if let Some(action) = body.get("_action").and_then(Value::as_str) {
        let p = match action {
            "cache_flush_all" => "cache:write",
            "budget_reset" => "budgets:adjust",
            "audit_sink_delete" | "secrets_rotate" => "security:write",
            _ => "config:apply",
        };
        if let Some(needed) = crate::danger_routes::needed(action, body) {
            return Ok(needed);
        }
        return Ok(vec![(p.to_owned(), vec![Scope::Gateway])]);
    }
    let list = body["changes"].as_array().cloned().unwrap_or_default();
    let staged = stage(call, &call.snap.config.doc, &list).map(|s| s.permissions);
    needed_from(stored_needed(body), staged)
}

async fn finish(call: &Call, row: &PendingRequest, body: &Value) -> Result<(), AdminError> {
    let state = body["state"].as_str().unwrap_or("open");
    call.rt
        .store
        .update_request(KIND, &row.id, row.version, state, &body.to_string())
        .await
        .map_err(|_| AdminError::store_unavailable())?
        .then_some(())
        .ok_or_else(|| {
            AdminError::conflict(
                "already_exists",
                "The change request changed concurrently.",
                Vec::new(),
            )
        })
}

/// Applies an approved change request.
async fn apply(call: &Call, row: PendingRequest, mut body: Value) -> Result<Response, AdminError> {
    let id = row.id.clone();
    let approvers: Vec<String> = body["approvals"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["by"].as_str().map(str::to_owned))
        .collect();
    let author = body["author"].as_str().unwrap_or_default().to_owned();
    let note = format!(
        "change request {id} by {author}, approved by {}",
        approvers.join(", ")
    );
    let list = change_secrets::literal(&call.rt.sealer, &body)?;
    change_secrets::forget(&mut body);
    if body.get("_action").is_some() {
        body["state"] = json!("applied");
        finish(call, &row, &body).await?;
        let change = list.first().cloned().unwrap_or(Value::Null);
        let action = body["_action"].as_str().unwrap_or_default().to_owned();
        let resp = Box::pin(crate::change_control::run_approved(call, &action, &change, &note)).await?;
        event(call, "change_request.applied", &body);
        return Ok(resp);
    }
    let touched = body["_touched"].as_object().cloned().unwrap_or_default();
    let recorded = stored_needed(&body);
    let mut audit = call.audit_context(
        "POST change_requests:apply",
        &format!("change_requests/{id}"),
        StatusCode::OK,
    );
    audit.reason = Some(note);
    let result = call
        .rt
        .mutate(|state| {
            let staged = stage(call, &state.doc, &list)?;
            let now = fingerprints(&state.doc, &staged.written);
            if now != touched || !covered(recorded.as_deref(), &staged.permissions) {
                return Err(AdminError::precondition_failed());
            }
            Ok(Mutation {
                candidate: staged.candidate,
                writer: "api".to_owned(),
                written: staged.written,
                credentials: Vec::new(),
                meta_edits: Vec::new(),
                dry_run: false,
                source: "api".to_owned(),
                audit: Some(audit),
            })
        })
        .await;
    let committed = match crate::commit::finish(
        call,
        "POST change_requests:apply",
        &format!("change_requests/{id}"),
        result,
    )
    .await
    {
        Ok(c) => c,
        Err(e) if e.status == StatusCode::PRECONDITION_FAILED => {
            body["state"] = json!("conflicted");
            finish(call, &row, &body).await?;
            return Err(e);
        }
        Err(e) => return Err(e),
    };
    body["state"] = json!("applied");
    body["applied_generation"] = json!(committed.generation);
    finish(call, &row, &body).await?;
    event(call, "change_request.applied", &body);
    let resp = call.json(StatusCode::OK, &shown(&body));
    Ok(call.mutated(resp, committed.generation).await)
}
