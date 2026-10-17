//! SCIM users: each is a `human` user of the configuration named by its `userName` (lowercased),
//! with its email, display name and `enabled` from `active`; the SCIM id, external id and times are
//! kept as an identity record of that user, so deleting the user removes them.

use antirion_runtime::MetaEdit;
use antirion_store::IdentityRecord;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Map, Value, json};

use super::{Scim, ScimError, audit, eq_filter, groups, list, no_content, patch_ops, random_id, scim_json};

pub(crate) const KIND: &str = "scim_user";
const SCHEMA: &str = "urn:ietf:params:scim:schemas:core:2.0:User";

/// A SCIM user as stored beside the configuration.
pub(crate) struct Stored {
    pub(crate) id: String,
    pub(crate) user: String,
    pub(crate) body: Value,
}

fn stored(r: &IdentityRecord) -> Option<Stored> {
    Some(Stored {
        id: r.id.clone(),
        user: r.user.clone(),
        body: serde_json::from_str(&r.body).ok()?,
    })
}

pub(crate) async fn all(s: &Scim) -> Result<Vec<Stored>, ScimError> {
    Ok(s.rt
        .store
        .records(KIND, None)
        .await
        .map_err(|_| ScimError::unavailable())?
        .iter()
        .filter_map(stored)
        .collect())
}

async fn find(s: &Scim, id: &str) -> Result<Stored, ScimError> {
    s.rt.store
        .record(KIND, id)
        .await
        .map_err(|_| ScimError::unavailable())?
        .as_ref()
        .and_then(stored)
        .ok_or_else(ScimError::not_found)
}

async fn save(s: &Scim, st: &Stored) -> Result<(), ScimError> {
    s.rt.store
        .put_record(IdentityRecord {
            kind: KIND.to_owned(),
            id: st.id.clone(),
            user: st.user.clone(),
            expires_us: None,
            body: st.body.to_string(),
        })
        .await
        .map_err(|_| ScimError::unavailable())
}

/// The user's entry in the serving configuration.
fn entry(s: &Scim, user: &str) -> Option<Value> {
    s.rt.gens
        .load()
        .config
        .doc
        .pointer(&format!("/users/{}", user.replace('~', "~0").replace('/', "~1")))
        .cloned()
}

fn resource(st: &Stored, entry: Option<&Value>) -> Value {
    let e = entry.cloned().unwrap_or_else(|| json!({}));
    let email = e.get("email").cloned().unwrap_or(Value::Null);
    json!({
        "schemas": [SCHEMA],
        "id": st.id,
        "externalId": st.body.get("externalId").cloned().unwrap_or(Value::Null),
        "userName": st.body.get("userName").cloned().unwrap_or_else(|| json!(st.user)),
        "displayName": e.get("display_name").cloned().unwrap_or(Value::Null),
        "emails": if email.is_null() { json!([]) } else { json!([{"value": email, "primary": true}]) },
        "active": e.get("enabled").and_then(Value::as_bool).unwrap_or(true),
        "groups": [],
        "meta": {
            "resourceType": "User",
            "created": st.body.get("created").cloned().unwrap_or(Value::Null),
            "lastModified": st.body.get("lastModified").cloned().unwrap_or(Value::Null),
            "location": format!("/scim/v2/Users/{}", st.id),
        },
    })
}

/// The facts a create or replace sets.
struct Fields {
    user_name: String,
    email: Option<String>,
    display_name: Option<String>,
    active: bool,
    external_id: Option<Value>,
}

fn fields(m: &Map<String, Value>) -> Result<Fields, ScimError> {
    let user_name = m
        .get("userName")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| ScimError::invalid("userName is required"))?;
    let email = m
        .get("emails")
        .and_then(Value::as_array)
        .and_then(|e| {
            e.iter()
                .find(|x| x.get("primary") == Some(&Value::Bool(true)))
                .or_else(|| e.first())
        })
        .and_then(|e| e.get("value"))
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
        .or_else(|| user_name.contains('@').then(|| user_name.clone()));
    let display_name = m
        .get("displayName")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            let n = m.get("name")?;
            let part = |k: &str| n.get(k).and_then(Value::as_str);
            part("formatted").map(str::to_owned).or_else(|| {
                let joined = [part("givenName"), part("familyName")]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" ");
                (!joined.is_empty()).then_some(joined)
            })
        });
    Ok(Fields {
        user_name,
        email,
        display_name,
        active: m.get("active").and_then(Value::as_bool).unwrap_or(true),
        external_id: m.get("externalId").cloned(),
    })
}

fn config_entry(f: &Fields, bindings: Vec<Value>) -> Value {
    let mut e = Map::new();
    e.insert("kind".into(), json!("human"));
    if let Some(email) = &f.email {
        e.insert("email".into(), json!(email));
    }
    if let Some(d) = &f.display_name {
        e.insert("display_name".into(), json!(d));
    }
    e.insert("enabled".into(), Value::Bool(f.active));
    e.insert("role_bindings".into(), Value::Array(bindings));
    Value::Object(e)
}

/// `GET` and `POST /scim/v2/Users`.
pub(crate) async fn collection(s: &Scim) -> Result<Response, ScimError> {
    match s.method {
        Method::GET => {
            let filter = eq_filter(s)?;
            let mut out = Vec::new();
            for st in all(s).await? {
                let r = resource(&st, entry(s, &st.user).as_ref());
                let keep = match &filter {
                    None => true,
                    Some((attr, v)) => r.get(attr.as_str()).and_then(Value::as_str).is_some_and(|x| {
                        if attr == "userName" {
                            x.eq_ignore_ascii_case(v)
                        } else {
                            x == v
                        }
                    }),
                };
                if keep {
                    out.push(r);
                }
            }
            Ok(list(s, out))
        }
        Method::POST => {
            let f = fields(&s.object()?)?;
            let taken = entry(s, &f.user_name).is_some();
            if taken {
                return Err(ScimError::new(
                    StatusCode::CONFLICT,
                    Some("uniqueness"),
                    "A user with this userName exists.",
                ));
            }
            crate::users_cap::check_scim(&s.rt, f.active).await?;
            let now = s.now();
            let st = Stored {
                id: random_id()?,
                user: f.user_name.clone(),
                body: json!({"userName": f.user_name, "externalId": f.external_id, "created": now, "lastModified": now}),
            };
            s.write_users(
                vec![(f.user_name.clone(), Some(config_entry(&f, Vec::new())))],
                "scim.users.create",
            )
            .await?;
            save(s, &st).await?;
            let r = resource(&st, entry(s, &st.user).as_ref());
            Ok(scim_json(StatusCode::CREATED, &r))
        }
        _ => Err(ScimError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            None,
            "GET or POST.",
        )),
    }
}

/// `GET`, `PUT`, `PATCH` and `DELETE /scim/v2/Users/{id}`.
pub(crate) async fn one(s: &Scim, id: &str) -> Result<Response, ScimError> {
    let mut st = find(s, id).await?;
    match s.method {
        Method::GET => Ok(scim_json(
            StatusCode::OK,
            &resource(&st, entry(s, &st.user).as_ref()),
        )),
        Method::PUT => {
            let f = fields(&s.object()?)?;
            if f.user_name != st.user {
                return Err(ScimError::new(
                    StatusCode::BAD_REQUEST,
                    Some("mutability"),
                    "userName cannot change.",
                ));
            }
            let current = entry(s, &st.user).unwrap_or_else(|| json!({}));
            if f.active && current.get("enabled") == Some(&Value::Bool(false)) {
                crate::users_cap::check_scim(&s.rt, true).await?;
            }
            let bindings = groups::bindings_for(s, &st.id).await?;
            s.write_users(
                vec![(st.user.clone(), Some(config_entry(&f, bindings)))],
                "scim.users.replace",
            )
            .await?;
            st.body["externalId"] = f.external_id.unwrap_or(Value::Null);
            st.body["lastModified"] = json!(s.now());
            save(s, &st).await?;
            Ok(scim_json(
                StatusCode::OK,
                &resource(&st, entry(s, &st.user).as_ref()),
            ))
        }
        Method::PATCH => {
            let ops = patch_ops(&s.object()?)?;
            let mut e = entry(s, &st.user).unwrap_or_else(|| json!({"kind": "human"}));
            let was_enabled = e.get("enabled") != Some(&Value::Bool(false));
            for (op, path, value) in ops {
                let updates: Vec<(String, Value)> = match path {
                    Some(p) => vec![(p, value)],
                    None => value
                        .as_object()
                        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                        .unwrap_or_default(),
                };
                for (p, v) in updates {
                    match (p.as_str(), op.as_str()) {
                        ("active", "add" | "replace") => {
                            let on = v
                                .as_bool()
                                .or_else(|| v.as_str().map(|s| s.eq_ignore_ascii_case("true")));
                            e["enabled"] = Value::Bool(
                                on.ok_or_else(|| ScimError::invalid("active must be a boolean"))?,
                            );
                        }
                        ("displayName", "add" | "replace") => e["display_name"] = v,
                        ("displayName", "remove") => {
                            if let Some(o) = e.as_object_mut() {
                                o.remove("display_name");
                            }
                        }
                        ("externalId", _) => st.body["externalId"] = v,
                        (p, "add" | "replace") if p.starts_with("emails") => {
                            if let Some(email) = v
                                .as_str()
                                .map(str::to_owned)
                                .or_else(|| v.as_array()?.first()?.get("value")?.as_str().map(str::to_owned))
                            {
                                e["email"] = json!(email.to_ascii_lowercase());
                            }
                        }
                        _ => {}
                    }
                }
            }
            let enabling = !was_enabled && e.get("enabled") != Some(&Value::Bool(false));
            if enabling {
                crate::users_cap::check_scim(&s.rt, true).await?;
            }
            s.write_users(vec![(st.user.clone(), Some(e))], "scim.users.update")
                .await?;
            st.body["lastModified"] = json!(s.now());
            save(s, &st).await?;
            Ok(scim_json(
                StatusCode::OK,
                &resource(&st, entry(s, &st.user).as_ref()),
            ))
        }
        Method::DELETE => {
            delete_user(s, &st.user).await?;
            s.rt.store
                .delete_record(KIND, &st.id)
                .await
                .map_err(|_| ScimError::unavailable())?;
            groups::forget_member(s, &st.id).await?;
            Ok(no_content())
        }
        _ => Err(ScimError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            None,
            "GET, PUT, PATCH or DELETE.",
        )),
    }
}

/// Deletes a user from the configuration, disables the keys it owns and forgets its tokens,
/// sessions and identity records.
async fn delete_user(s: &Scim, user: &str) -> Result<(), ScimError> {
    let result =
        s.rt.mutate(|state| {
            let mut doc = state.doc.clone();
            let (disabled, meta_edits): (Vec<String>, Vec<MetaEdit>) =
                crate::tenancy::disable_owned_keys(&mut doc, user);
            if let Some(users) = doc.get_mut("users").and_then(Value::as_object_mut) {
                users.remove(user);
            }
            Ok::<_, ()>(antirion_runtime::Mutation {
                candidate: doc,
                writer: "api".to_owned(),
                written: std::iter::once(antirion_runtime::changes::Target::Resource(
                    "users",
                    user.to_owned(),
                ))
                .chain(
                    disabled
                        .into_iter()
                        .map(|k| antirion_runtime::changes::Target::Resource("keys", k)),
                )
                .collect(),
                credentials: Vec::new(),
                meta_edits,
                dry_run: false,
                source: "scim".to_owned(),
                audit: Some(audit("scim.users.delete", &format!("users/{user}"), 204)),
            })
        })
        .await;
    if result.is_err() {
        return Err(ScimError::unavailable());
    }
    crate::users::forget_user(&s.rt, user).await;
    Ok(())
}
