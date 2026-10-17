//! Saved log views (`/logs/views`): named filters, sort and columns a principal keeps for itself or
//! shares with the organization it was working in. They are per-user state in the store, not
//! configuration, and are deleted with their creator. A shared view is visible to every principal
//! whose scope contains its organization; only its creator, an `org_admin` of that organization or a
//! gateway administrator may change it.
//!
//! `starred` is the caller's own mark: the creator's is kept in the view, anyone else's in a star
//! record of its own, so a reader may star a view it cannot change.

use antirion_core::audit::Outcome;
use antirion_identity::rbac::{Role, Scope};
use antirion_identity::verifier::random_id;
use antirion_store::IdentityRecord;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Map, Value, json};

use crate::call::Call;
use crate::error::AdminError;
use crate::logs::require_some;
use crate::pagination;

const KIND: &str = "log_view";
const STAR_KIND: &str = "log_view_star";
const NAME_MAX: usize = 100;
const BODY_MAX: usize = 32 * 1024;

/// The owner a view is stored under: the caller's user, or its credential.
fn owner(call: &Call) -> String {
    call.principal
        .user
        .clone()
        .unwrap_or_else(|| call.principal.id.clone())
}

/// The organization the caller works in: its session's, else the one its bindings name.
fn active_organization(call: &Call) -> Option<String> {
    if let Some(o) = call
        .session
        .as_ref()
        .and_then(|(_, s)| s.facts.organization.clone())
    {
        return Some(o);
    }
    let id = &call.snap.identity;
    let org_of = |scope: &Scope| -> Option<String> {
        match scope {
            Scope::Gateway => None,
            Scope::Organization(o) => Some(o.clone()),
            Scope::Team(t) => id.teams.get(t).and_then(|t| t.parent.clone()),
            Scope::Project(p) => id
                .projects
                .get(p)
                .and_then(|p| p.parent.as_ref())
                .and_then(|t| id.teams.get(t))
                .and_then(|t| t.parent.clone()),
        }
    };
    if call.principal.bindings.iter().any(|b| b.scope == Scope::Gateway) {
        return None;
    }
    let mut orgs: Vec<String> = call
        .principal
        .bindings
        .iter()
        .filter_map(|b| org_of(&b.scope))
        .collect();
    orgs.sort();
    orgs.dedup();
    match orgs.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

fn org_chain(org: Option<&str>) -> Vec<Scope> {
    let mut chain = vec![Scope::Gateway];
    chain.extend(org.map(|o| Scope::Organization(o.to_owned())));
    chain
}

fn body_of(rec: &IdentityRecord) -> Option<Value> {
    serde_json::from_str(&rec.body).ok()
}

fn organization(view: &Value) -> Option<&str> {
    view.get("organization").and_then(Value::as_str)
}

fn visible(call: &Call, rec: &IdentityRecord, view: &Value) -> bool {
    if rec.user == owner(call) {
        return true;
    }
    let chain = org_chain(organization(view));
    view.get("shared") == Some(&Value::Bool(true))
        && call.principal.bindings.iter().any(|b| chain.contains(&b.scope))
}

fn may_change(call: &Call, rec: &IdentityRecord, view: &Value) -> bool {
    if rec.user == owner(call) || call.principal.is_gateway_admin() {
        return true;
    }
    !call.principal.read_only
        && organization(view).is_some_and(|o| {
            call.principal
                .bindings
                .iter()
                .any(|b| b.role == Role::OrgAdmin && b.scope == Scope::Organization(o.to_owned()))
        })
}

fn star_id(view: &str, owner: &str) -> String {
    format!("{view}:{owner}")
}

/// The ids of the views the caller starred without being their creator.
async fn stars_of(call: &Call) -> Result<Vec<String>, AdminError> {
    let me = owner(call);
    Ok(call
        .rt
        .store
        .records(STAR_KIND, Some(&me))
        .await
        .map_err(|_| AdminError::store_unavailable())?
        .into_iter()
        .filter_map(|r| r.id.strip_suffix(&format!(":{me}")).map(str::to_owned))
        .collect())
}

/// The view as the caller sees it: `starred` is the caller's own mark.
fn as_seen(call: &Call, rec: &IdentityRecord, mut view: Value, stars: &[String]) -> Value {
    if rec.user != owner(call) {
        let id = view.get("id").and_then(Value::as_str).unwrap_or_default();
        let starred = stars.iter().any(|s| s == id);
        view["starred"] = Value::Bool(starred);
    }
    view
}

/// Records or clears the caller's star on a view it did not create.
async fn set_star(call: &Call, id: &str, starred: bool) -> Result<(), AdminError> {
    let me = owner(call);
    let store = &call.rt.store;
    if starred {
        store
            .put_record(IdentityRecord {
                kind: STAR_KIND.to_owned(),
                id: star_id(id, &me),
                user: me,
                expires_us: None,
                body: "{}".to_owned(),
            })
            .await
    } else {
        store
            .delete_record(STAR_KIND, &star_id(id, &me))
            .await
            .map(|_| ())
    }
    .map_err(|_| AdminError::store_unavailable())
}

/// Drops every principal's star on a deleted view.
async fn drop_stars(call: &Call, id: &str) -> Result<(), AdminError> {
    let prefix = format!("{id}:");
    let store = &call.rt.store;
    let stars = store
        .records(STAR_KIND, None)
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    for star in stars.iter().filter(|r| r.id.starts_with(&prefix)) {
        store
            .delete_record(STAR_KIND, &star.id)
            .await
            .map_err(|_| AdminError::store_unavailable())?;
    }
    Ok(())
}

/// Checks and normalises the members of a create or patch body; `create` requires `name` and
/// `filters`.
fn members(call: &Call, create: bool) -> Result<Map<String, Value>, AdminError> {
    if call.body.len() > BODY_MAX {
        return Err(AdminError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "A view may weigh at most 32 KiB.",
        ));
    }
    let m = call.json_body()?;
    let mut out = Map::new();
    for (k, v) in &m {
        let ok = match k.as_str() {
            "name" => v
                .as_str()
                .is_some_and(|s| !s.trim().is_empty() && s.chars().count() <= NAME_MAX),
            "filters" => v.is_object(),
            "sort" => v.is_string(),
            "columns" => v.as_array().is_some_and(|a| a.iter().all(Value::is_string)),
            "starred" | "shared" => v.is_boolean(),
            _ => {
                return Err(AdminError::invalid_parameter(format!(
                    "a view has no member {k:?}"
                )));
            }
        };
        if !ok {
            return Err(AdminError::invalid_parameter(format!(
                "{k} has the wrong type or length"
            )));
        }
        out.insert(k.clone(), v.clone());
    }
    if create && (!out.contains_key("name") || !out.contains_key("filters")) {
        return Err(AdminError::invalid_parameter("a view needs name and filters"));
    }
    Ok(out)
}

async fn records(call: &Call) -> Result<Vec<IdentityRecord>, AdminError> {
    call.rt
        .store
        .records(KIND, None)
        .await
        .map_err(|_| AdminError::store_unavailable())
}

async fn find(call: &Call, id: &str) -> Result<(IdentityRecord, Value), AdminError> {
    let rec = call
        .rt
        .store
        .record(KIND, id)
        .await
        .map_err(|_| AdminError::store_unavailable())?
        .ok_or_else(AdminError::not_found)?;
    let view = body_of(&rec).ok_or_else(AdminError::not_found)?;
    if !visible(call, &rec, &view) {
        return Err(AdminError::not_found());
    }
    Ok((rec, view))
}

async fn save(call: &Call, user: String, view: &Value) -> Result<(), AdminError> {
    let id = view
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    call.rt
        .store
        .put_record(IdentityRecord {
            kind: KIND.to_owned(),
            id,
            user,
            expires_us: None,
            body: view.to_string(),
        })
        .await
        .map_err(|_| AdminError::store_unavailable())
}

/// `GET` and `POST /logs/views`.
pub(crate) async fn collection(call: &Call) -> Result<Response, AdminError> {
    require_some(call, "logs:read")?;
    match call.method {
        Method::GET => {
            call.allow_params(&["limit", "cursor"])?;
            let (limit, after) = pagination::page_params(call, "log_views")?;
            let stars = stars_of(call).await?;
            let mut views: Vec<Value> = records(call)
                .await?
                .iter()
                .filter_map(|r| {
                    body_of(r)
                        .filter(|v| visible(call, r, v))
                        .map(|v| as_seen(call, r, v, &stars))
                })
                .filter(|v| {
                    after
                        .as_deref()
                        .is_none_or(|a| v.get("id").and_then(Value::as_str).is_some_and(|id| id > a))
                })
                .collect();
            views.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
            let more = views.len() > limit;
            views.truncate(limit);
            let next = views
                .last()
                .filter(|_| more)
                .and_then(|v| v["id"].as_str())
                .map(|id| pagination::encode(&call.rt.cursor_key, "log_views", id));
            Ok(call.json(StatusCode::OK, &json!({"items": views, "next_cursor": next})))
        }
        Method::POST => {
            call.allow_params(&[])?;
            let m = members(call, true)?;
            let id = format!(
                "view-{}",
                random_id()
                    .map_err(|_| AdminError::internal())?
                    .get(..16)
                    .unwrap_or_default()
            );
            let view = json!({
                "id": id,
                "name": m["name"],
                "filters": m["filters"],
                "sort": m.get("sort").cloned().unwrap_or(Value::Null),
                "columns": m.get("columns").cloned().unwrap_or_else(|| json!([])),
                "starred": m.get("starred").cloned().unwrap_or(Value::Bool(false)),
                "shared": m.get("shared").cloned().unwrap_or(Value::Bool(false)),
                "organization": active_organization(call),
                "created_by": owner(call),
                "updated_at": call.rt.now().to_rfc3339_seconds(),
            });
            save(call, owner(call), &view).await?;
            call.audit(
                "logs.views.create",
                &format!("logs/views/{id}"),
                Outcome::Success,
                StatusCode::CREATED,
            )
            .await;
            let resp = call.json(StatusCode::CREATED, &view);
            Ok(call.created(resp, &format!("/admin/v1/logs/views/{id}")))
        }
        _ => Err(AdminError::method_not_allowed("GET, POST")),
    }
}

/// `GET`, `PATCH` and `DELETE /logs/views/{id}`.
pub(crate) async fn one(call: &Call, id: &str) -> Result<Response, AdminError> {
    require_some(call, "logs:read")?;
    call.allow_params(&[])?;
    let resource = format!("logs/views/{id}");
    match call.method {
        Method::GET => {
            let (rec, view) = find(call, id).await?;
            let stars = stars_of(call).await?;
            Ok(call.json(StatusCode::OK, &as_seen(call, &rec, view, &stars)))
        }
        Method::PATCH => {
            let (rec, mut view) = find(call, id).await?;
            let mut m = members(call, false)?;
            let creator = rec.user == owner(call);
            let star = if creator { None } else { m.remove("starred") };
            if !m.is_empty() {
                if !may_change(call, &rec, &view) {
                    return Err(AdminError::new(
                        StatusCode::FORBIDDEN,
                        "forbidden",
                        "Only the view's creator or an org_admin of its organization may change it.",
                    )
                    .with_resource(resource));
                }
                if let Some(o) = view.as_object_mut() {
                    o.extend(m);
                    o.insert("updated_at".into(), json!(call.rt.now().to_rfc3339_seconds()));
                }
                save(call, rec.user.clone(), &view).await?;
            }
            if let Some(star) = star {
                set_star(call, id, star == Value::Bool(true)).await?;
            }
            call.audit("logs.views.update", &resource, Outcome::Success, StatusCode::OK)
                .await;
            let stars = stars_of(call).await?;
            Ok(call.json(StatusCode::OK, &as_seen(call, &rec, view, &stars)))
        }
        Method::DELETE => {
            let (rec, view) = find(call, id).await?;
            if !may_change(call, &rec, &view) {
                return Err(AdminError::new(
                    StatusCode::FORBIDDEN,
                    "forbidden",
                    "Only the view's creator or an org_admin of its organization may delete it.",
                )
                .with_resource(resource));
            }
            call.rt
                .store
                .delete_record(KIND, id)
                .await
                .map_err(|_| AdminError::store_unavailable())?;
            drop_stars(call, id).await?;
            call.audit(
                "logs.views.delete",
                &resource,
                Outcome::Success,
                StatusCode::NO_CONTENT,
            )
            .await;
            Ok(call.no_content())
        }
        _ => Err(AdminError::method_not_allowed("GET, PATCH, DELETE")),
    }
}
