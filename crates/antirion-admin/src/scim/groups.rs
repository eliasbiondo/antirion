//! SCIM groups: kept as records beside the configuration; membership gives the members' role
//! bindings through `auth.admin.scim.group_mappings` (a group matches a mapping by display name), and
//! every change of a group rewrites the bindings of the users it touched.

use std::collections::BTreeSet;

use antirion_store::IdentityRecord;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Map, Value, json};

use super::{Scim, ScimError, eq_filter, list, no_content, patch_ops, random_id, scim_json, users};

pub(crate) const KIND: &str = "scim_group";
const SCHEMA: &str = "urn:ietf:params:scim:schemas:core:2.0:Group";

struct Group {
    id: String,
    body: Value,
}

impl Group {
    fn name(&self) -> &str {
        self.body
            .get("displayName")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    fn members(&self) -> BTreeSet<String> {
        self.body
            .get("members")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    }

    fn set_members(&mut self, m: &BTreeSet<String>) {
        self.body["members"] = json!(m.iter().collect::<Vec<_>>());
    }
}

async fn all(s: &Scim) -> Result<Vec<Group>, ScimError> {
    Ok(s.rt
        .store
        .records(KIND, None)
        .await
        .map_err(|_| ScimError::unavailable())?
        .into_iter()
        .filter_map(|r| {
            Some(Group {
                id: r.id,
                body: serde_json::from_str(&r.body).ok()?,
            })
        })
        .collect())
}

async fn save(s: &Scim, g: &Group) -> Result<(), ScimError> {
    s.rt.store
        .put_record(IdentityRecord {
            kind: KIND.to_owned(),
            id: g.id.clone(),
            user: String::new(),
            expires_us: None,
            body: g.body.to_string(),
        })
        .await
        .map_err(|_| ScimError::unavailable())
}

/// The role bindings the groups containing the SCIM user `id` give it.
pub(crate) async fn bindings_for(s: &Scim, id: &str) -> Result<Vec<Value>, ScimError> {
    let mappings = s.rt.gens.load().identity.scim.group_mappings.clone();
    let mut out: Vec<Value> = Vec::new();
    for g in all(s).await?.iter().filter(|g| g.members().contains(id)) {
        for (group, role, scope) in &mappings {
            let b = json!({"role": role, "scope": scope});
            if group == g.name() && !out.contains(&b) {
                out.push(b);
            }
        }
    }
    Ok(out)
}

/// Rewrites the role bindings of the SCIM users `ids`.
async fn rebind(s: &Scim, ids: &BTreeSet<String>) -> Result<(), ScimError> {
    let users = users::all(s).await?;
    let mut changes = Vec::new();
    for u in users.iter().filter(|u| ids.contains(&u.id)) {
        let doc = s.rt.gens.load();
        let Some(mut entry) = doc.config.doc.get("users").and_then(|m| m.get(&u.user)).cloned() else {
            continue;
        };
        entry["role_bindings"] = Value::Array(bindings_for(s, &u.id).await?);
        changes.push((u.user.clone(), Some(entry)));
    }
    s.write_users(changes, "scim.groups.bind").await
}

/// Removes a deleted user from every group.
pub(crate) async fn forget_member(s: &Scim, id: &str) -> Result<(), ScimError> {
    for mut g in all(s).await? {
        let mut m = g.members();
        if m.remove(id) {
            g.set_members(&m);
            save(s, &g).await?;
        }
    }
    Ok(())
}

async fn resource(s: &Scim, g: &Group) -> Result<Value, ScimError> {
    let names: Vec<(String, Value)> = users::all(s)
        .await?
        .into_iter()
        .map(|u| (u.id, u.body.get("userName").cloned().unwrap_or(Value::Null)))
        .collect();
    let members: Vec<Value> = g
        .members()
        .iter()
        .map(|m| {
            let display = names
                .iter()
                .find(|(id, _)| id == m)
                .map_or(Value::Null, |(_, n)| n.clone());
            json!({"value": m, "display": display, "$ref": format!("/scim/v2/Users/{m}")})
        })
        .collect();
    Ok(json!({
        "schemas": [SCHEMA],
        "id": g.id,
        "displayName": g.name(),
        "externalId": g.body.get("externalId").cloned().unwrap_or(Value::Null),
        "members": members,
        "meta": {
            "resourceType": "Group",
            "created": g.body.get("created").cloned().unwrap_or(Value::Null),
            "lastModified": g.body.get("lastModified").cloned().unwrap_or(Value::Null),
            "location": format!("/scim/v2/Groups/{}", g.id),
        },
    }))
}

fn member_ids(v: &Value) -> BTreeSet<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m.get("value").and_then(Value::as_str).map(str::to_owned))
        .collect()
}

fn display_name(m: &Map<String, Value>) -> Result<String, ScimError> {
    m.get("displayName")
        .and_then(Value::as_str)
        .filter(|n| !n.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| ScimError::invalid("displayName is required"))
}

/// `GET` and `POST /scim/v2/Groups`.
pub(crate) async fn collection(s: &Scim) -> Result<Response, ScimError> {
    match s.method {
        Method::GET => {
            let filter = eq_filter(s)?;
            let mut out = Vec::new();
            for g in all(s).await? {
                let keep = match &filter {
                    None => true,
                    Some((attr, v)) if attr == "displayName" => g.name() == v,
                    Some((attr, v)) if attr == "externalId" => {
                        g.body.get("externalId").and_then(Value::as_str) == Some(v.as_str())
                    }
                    Some(_) => false,
                };
                if keep {
                    out.push(resource(s, &g).await?);
                }
            }
            Ok(list(s, out))
        }
        Method::POST => {
            let m = s.object()?;
            let name = display_name(&m)?;
            if all(s).await?.iter().any(|g| g.name() == name) {
                return Err(ScimError::new(
                    StatusCode::CONFLICT,
                    Some("uniqueness"),
                    "A group with this displayName exists.",
                ));
            }
            let now = s.now();
            let members = member_ids(m.get("members").unwrap_or(&Value::Null));
            let mut g = Group {
                id: random_id()?,
                body: json!({"displayName": name, "externalId": m.get("externalId").cloned(), "created": now, "lastModified": now}),
            };
            g.set_members(&members);
            save(s, &g).await?;
            rebind(s, &members).await?;
            Ok(scim_json(StatusCode::CREATED, &resource(s, &g).await?))
        }
        _ => Err(ScimError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            None,
            "GET or POST.",
        )),
    }
}

/// `GET`, `PUT`, `PATCH` and `DELETE /scim/v2/Groups/{id}`.
pub(crate) async fn one(s: &Scim, id: &str) -> Result<Response, ScimError> {
    let rec =
        s.rt.store
            .record(KIND, id)
            .await
            .map_err(|_| ScimError::unavailable())?
            .ok_or_else(ScimError::not_found)?;
    let mut g = Group {
        id: rec.id,
        body: serde_json::from_str(&rec.body).map_err(|_| ScimError::not_found())?,
    };
    let before = g.members();
    match s.method {
        Method::GET => Ok(scim_json(StatusCode::OK, &resource(s, &g).await?)),
        Method::PUT => {
            let m = s.object()?;
            g.body["displayName"] = json!(display_name(&m)?);
            g.set_members(&member_ids(m.get("members").unwrap_or(&Value::Null)));
            finish(s, g, before).await
        }
        Method::PATCH => {
            let mut members = before.clone();
            for (op, path, value) in patch_ops(&s.object()?)? {
                let p = path.as_deref().unwrap_or("");
                match (op.as_str(), p) {
                    ("add", "members") => members.extend(member_ids(&value)),
                    ("replace", "members") => members = member_ids(&value),
                    ("remove", "members") if value.is_null() => members.clear(),
                    ("remove", "members") => {
                        for id in member_ids(&value) {
                            members.remove(&id);
                        }
                    }
                    ("remove", p) if p.starts_with("members[") => {
                        if let Some(v) = p.split('"').nth(1) {
                            members.remove(v);
                        }
                    }
                    ("add" | "replace", "displayName") => g.body["displayName"] = value,
                    ("add" | "replace", "") => {
                        if let Some(n) = value.get("displayName") {
                            g.body["displayName"] = n.clone();
                        }
                        if let Some(ms) = value.get("members") {
                            if op == "add" {
                                members.extend(member_ids(ms));
                            } else {
                                members = member_ids(ms);
                            }
                        }
                    }
                    _ => {}
                }
            }
            g.set_members(&members);
            finish(s, g, before).await
        }
        Method::DELETE => {
            s.rt.store
                .delete_record(KIND, id)
                .await
                .map_err(|_| ScimError::unavailable())?;
            rebind(s, &before).await?;
            Ok(no_content())
        }
        _ => Err(ScimError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            None,
            "GET, PUT, PATCH or DELETE.",
        )),
    }
}

async fn finish(s: &Scim, mut g: Group, before: BTreeSet<String>) -> Result<Response, ScimError> {
    g.body["lastModified"] = json!(s.now());
    save(s, &g).await?;
    let touched: BTreeSet<String> = before.union(&g.members()).cloned().collect();
    rebind(s, &touched).await?;
    Ok(scim_json(StatusCode::OK, &resource(s, &g).await?))
}
