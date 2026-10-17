//! Invitations: a disabled user with the bindings it will hold, and a single-use link mailed to it
//! (or returned once when mail is off). Accepting sets a local password and continues with the
//! second factor; a single sign-on user completes it by signing in.

use antirion_core::UtcMicros;
use antirion_core::audit::Outcome;
use antirion_identity::rbac::Scope;
use antirion_runtime::changes::Target;
use antirion_runtime::operators::Invitation;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Map, Value, json};

use crate::call::Call;
use crate::commit::{Proposal, api_write};
use crate::error::AdminError;
use crate::local::{Public, json_response};
use crate::perms::scope_chain;

fn invitation_json(i: &Invitation, now: UtcMicros) -> Value {
    json!({
        "id": i.id,
        "user": i.user,
        "email": i.email,
        "invited_by": i.invited_by,
        "created_at": i.created_at.to_rfc3339_seconds(),
        "expires_at": i.expires_at.to_rfc3339_seconds(),
        "expired": i.expired(now),
        "role_bindings": i.role_bindings,
    })
}

/// The user name an invited email gets: the address itself, as single sign-on names its users, so
/// the first sign-on with that email finds it.
fn user_name(email: &str) -> Option<String> {
    let name = email.trim().to_lowercase();
    let valid = name.len() <= 254
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._@+-".contains(c));
    valid.then_some(name)
}

/// The bindings of an invitation, each checked like a role-binding create.
fn bindings(call: &Call, user: &str, given: Option<&Value>) -> Result<Vec<Value>, AdminError> {
    let mut out = Vec::new();
    for b in given.and_then(Value::as_array).into_iter().flatten() {
        let role_name = b.get("role").and_then(Value::as_str).unwrap_or_default();
        let scope_text = b.get("scope").and_then(Value::as_str).unwrap_or_default();
        let role = call
            .snap
            .identity
            .roles
            .resolve(role_name)
            .ok_or_else(|| AdminError::invalid_parameter("role names no role"))?;
        let scope = Scope::parse(scope_text)
            .ok_or_else(|| AdminError::invalid_parameter("scope must be gateway or <kind>/<name>"))?;
        if !role
            .bindable_at()
            .contains(&scope_text.split('/').next().unwrap_or_default())
        {
            return Err(AdminError::invalid_parameter(
                "the role cannot be bound at this scope",
            ));
        }
        if let Some(p) = b.pointer("/principal/id").and_then(Value::as_str)
            && p != user
        {
            return Err(AdminError::invalid_parameter(
                "a binding of an invitation is for the invited user",
            ));
        }
        let chain = scope_chain(&call.snap, &scope);
        call.require("role_bindings:create", &chain)?;
        if !call.principal.may_grant(&role, &chain) {
            return Err(AdminError::forbidden("a role the caller holds itself"));
        }
        out.push(json!({"role": role_name, "scope": scope_text}));
    }
    Ok(out)
}

/// The link of an invitation, and whether it was mailed.
fn deliver(call: &Call, inv: &Invitation, token: &str) -> Option<String> {
    let base = format!(
        "{}{}",
        crate::oidc::own_origin(&call.rt, &call.headers),
        call.rt.statics.ui.path
    );
    let link = format!("{base}/accept-invitation?token={token}");
    let body = format!(
        "{} invited you to the gateway console.\n\nAccept the invitation here (the link works once, until {}):\n{link}\n",
        inv.invited_by,
        inv.expires_at.to_rfc3339_seconds()
    );
    if call
        .rt
        .send_mail(&inv.email, "You are invited to the console", body)
    {
        None
    } else {
        Some(link)
    }
}

fn created(inv: &Invitation, link: Option<String>, now: UtcMicros) -> Value {
    let mut v = invitation_json(inv, now);
    if let Some(o) = v.as_object_mut() {
        o.insert("link".into(), link.map_or(Value::Null, Value::from));
    }
    v
}

/// `GET` and `POST /invitations`.
pub(crate) async fn collection(call: &Call) -> Result<Response, AdminError> {
    match call.method {
        Method::GET => {
            call.allow_params(&["limit", "cursor"])?;
            call.require("users:read", &[Scope::Gateway])?;
            let now = call.rt.now();
            let items: Vec<Value> = call
                .rt
                .invitations()
                .await
                .map_err(AdminError::identity)?
                .iter()
                .map(|i| invitation_json(i, now))
                .collect();
            Ok(call.json(StatusCode::OK, &json!({"items": items, "next_cursor": null})))
        }
        Method::POST => create(call).await,
        _ => Err(AdminError::method_not_allowed("GET, POST")),
    }
}

async fn create(call: &Call) -> Result<Response, AdminError> {
    call.allow_params(&[])?;
    call.require("users:create", &[Scope::Gateway])
        .map_err(|e| e.with_resource("invitations"))?;
    let m: Map<String, Value> = call.json_body()?;
    let email = m
        .get("email")
        .and_then(Value::as_str)
        .filter(|e| {
            e.split_once('@')
                .is_some_and(|(l, d)| !l.is_empty() && !d.is_empty())
        })
        .ok_or_else(|| AdminError::invalid_parameter("email must be an email address"))?;
    let user = user_name(email).ok_or_else(|| AdminError::invalid_parameter("email cannot name a user"))?;
    crate::users_cap::check(&call.rt)
        .await
        .map_err(|e| e.with_resource("invitations"))?;
    let display_name = match m.get("display_name") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if (1..=200).contains(&s.chars().count()) => Some(s.clone()),
        Some(_) => {
            return Err(AdminError::invalid_parameter(
                "display_name must have 1 to 200 characters",
            ));
        }
    };
    let bindings = bindings(call, &user, m.get("role_bindings"))?;
    if call.snap.identity.users.contains_key(&user) || call.rt.user_by_email(email).is_some() {
        return Err(AdminError::conflict(
            "already_exists",
            "A user with this email exists.",
            Vec::new(),
        ));
    }
    let resource = format!("users/{user}");
    let spec_bindings = bindings.clone();
    let email_lower = email.to_lowercase();
    api_write(call, "invitations.create", &resource, StatusCode::CREATED, |state| {
        let mut doc = state.doc.clone();
        let users = doc
            .as_object_mut()
            .map(|o| o.entry("users").or_insert_with(|| json!({})))
            .and_then(Value::as_object_mut)
            .ok_or_else(AdminError::internal)?;
        let mut spec = json!({"kind": "human", "email": email_lower, "enabled": false, "role_bindings": spec_bindings});
        if let (Some(o), Some(d)) = (spec.as_object_mut(), &display_name) {
            o.insert("display_name".into(), Value::from(d.clone()));
        }
        users.insert(user.clone(), spec);
        Ok(Proposal {
            candidate: doc,
            written: vec![Target::Resource("users", user.clone())],
            credentials: Vec::new(),
            meta_edits: Vec::new(),
        })
    })
    .await?;
    let ttl = call.snap.identity.console.invitation_ttl;
    let inv = Invitation::new(
        &user,
        &email.to_lowercase(),
        call.principal.user.as_deref().unwrap_or(&call.principal.id),
        Value::Array(bindings),
        call.rt.now(),
        ttl,
    )
    .map_err(AdminError::identity)?;
    let token = call
        .rt
        .put_invitation(inv.clone())
        .await
        .map_err(AdminError::identity)?;
    let link = deliver(call, &inv, &token);
    let resp = call.json(StatusCode::CREATED, &created(&inv, link, call.rt.now()));
    Ok(call.created(resp, &format!("/admin/v1/invitations/{}", inv.id)))
}

/// `DELETE /invitations/{id}` and `POST /invitations/{id}:resend`.
pub(crate) async fn one(call: &Call, segment: &str) -> Result<Response, AdminError> {
    call.allow_params(&[])?;
    let (id, action) = match segment.split_once(':') {
        Some((id, a)) => (id, Some(a)),
        None => (segment, None),
    };
    let resource = format!("invitations/{id}");
    match (action, &call.method) {
        (None, &Method::DELETE) => {
            call.require("users:delete", &[Scope::Gateway])
                .map_err(|e| e.with_resource(resource.clone()))?;
            let inv = call
                .rt
                .invitation(id)
                .await
                .map_err(AdminError::identity)?
                .ok_or_else(AdminError::not_found)?;
            call.rt
                .delete_invitation(&inv)
                .await
                .map_err(AdminError::identity)?;
            let user = inv.user.clone();
            if call.snap.config.doc.pointer(&format!("/users/{user}")).is_some() {
                api_write(
                    call,
                    "invitations.delete",
                    &format!("users/{user}"),
                    StatusCode::NO_CONTENT,
                    |state| {
                        let mut doc = state.doc.clone();
                        if let Some(users) = doc.get_mut("users").and_then(Value::as_object_mut) {
                            users.remove(&user);
                        }
                        Ok(Proposal {
                            candidate: doc,
                            written: vec![Target::Resource("users", user.clone())],
                            credentials: Vec::new(),
                            meta_edits: Vec::new(),
                        })
                    },
                )
                .await?;
            }
            crate::users::forget_user(&call.rt, &inv.user).await;
            Ok(call.no_content())
        }
        (None, _) => Err(AdminError::method_not_allowed("DELETE")),
        (Some("resend"), &Method::POST) => {
            call.require("users:create", &[Scope::Gateway])
                .map_err(|e| e.with_resource(resource.clone()))?;
            let mut inv = call
                .rt
                .invitation(id)
                .await
                .map_err(AdminError::identity)?
                .ok_or_else(AdminError::not_found)?;
            let ttl = call.snap.identity.console.invitation_ttl;
            inv.expires_at = call.rt.now().saturating_add(ttl);
            let token = call
                .rt
                .put_invitation(inv.clone())
                .await
                .map_err(AdminError::identity)?;
            let link = deliver(call, &inv, &token);
            call.audit("invitations.resend", &resource, Outcome::Success, StatusCode::OK)
                .await;
            Ok(call.json(StatusCode::OK, &created(&inv, link, call.rt.now())))
        }
        (Some(_), &Method::POST) => Err(AdminError::not_found()),
        (Some(_), _) => Err(AdminError::method_not_allowed("POST")),
    }
}

/// Enables an invited user, outside any caller's permissions: accepting is the invitation's.
async fn enable(p: &Public, user: &str) -> Result<(), AdminError> {
    let user = user.to_owned();
    let result =
        p.rt.mutate(|state| {
            let mut doc = state.doc.clone();
            let Some(entry) = doc
                .pointer_mut(&format!("/users/{user}"))
                .and_then(Value::as_object_mut)
            else {
                return Err(AdminError::not_found());
            };
            entry.insert("enabled".into(), Value::Bool(true));
            Ok(antirion_runtime::Mutation {
                candidate: doc,
                writer: "api".to_owned(),
                written: vec![Target::Resource("users", user.clone())],
                credentials: Vec::new(),
                meta_edits: Vec::new(),
                dry_run: false,
                source: "api".to_owned(),
                audit: Some(antirion_runtime::AuditContext {
                    actor: antirion_core::audit::Actor {
                        kind: antirion_core::audit::ActorKind::Operator,
                        id: Some(user.clone()),
                    },
                    action: "invitations.accept".to_owned(),
                    resource: format!("users/{user}"),
                    request_id: p.request_id.as_str().to_owned(),
                    source_ip: p.peer.clone(),
                    status_code: 200,
                    reason: None,
                }),
            })
        })
        .await;
    match result {
        Ok(_) => Ok(()),
        Err(antirion_runtime::CommitError::Rejected(e)) => Err(e),
        Err(antirion_runtime::CommitError::Invalid(d)) => Err(AdminError::validation(d)),
        Err(_) => Err(AdminError::store_unavailable()),
    }
}

/// `POST /auth/invitations:accept` (public): `{token, password}`.
pub(crate) async fn accept(p: &Public) -> Result<Response, AdminError> {
    if p.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    let m = p.object()?;
    let token = m
        .get("token")
        .and_then(Value::as_str)
        .ok_or_else(|| AdminError::invalid_parameter("token is required"))?;
    p.rt.throttle_client(p.peer.as_deref())
        .map_err(AdminError::identity)?;
    let invalid = || AdminError::invalid_parameter("The invitation is unknown, used or expired.");
    let Some(inv) =
        p.rt.invitation_of_token(token)
            .await
            .map_err(AdminError::identity)?
    else {
        p.rt.count_failure(p.peer.as_deref());
        return Err(invalid());
    };
    let local = p.rt.gens.load().identity.console.local;
    let password = m.get("password").and_then(Value::as_str);
    if !local {
        return Err(AdminError::invalid_parameter(
            "local accounts are disabled: accept the invitation by signing in with single sign-on",
        ));
    }
    let Some(password) = password else {
        return Err(AdminError::invalid_parameter("password is required"));
    };
    let snap = p.rt.gens.load();
    let policy = &snap.identity.console.policy;
    antirion_identity::password::check(policy, password, Some(&inv.email))
        .map_err(|r| AdminError::identity(antirion_runtime::operators::IdentityError::Policy(r)))?;
    let Some(inv) = p.rt.take_invitation(token).await.map_err(AdminError::identity)? else {
        return Err(invalid());
    };
    enable(p, &inv.user).await?;
    p.rt.set_initial_password(&inv.user, password)
        .await
        .map_err(AdminError::identity)?;
    let step =
        p.rt.invitation_challenge(&inv.user)
            .await
            .map_err(AdminError::identity)?;
    p.audit(
        "invitations.accept",
        Some(&inv.user),
        Outcome::Success,
        StatusCode::OK,
    )
    .await;
    Ok(json_response(StatusCode::OK, &crate::local::step_json(&step)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invited_users_are_named_by_their_email() {
        assert_eq!(
            user_name(" Alice.M+x@Acme.example ").as_deref(),
            Some("alice.m+x@acme.example")
        );
        assert!(user_name("_bad@acme.example").is_none());
        assert!(user_name("a b@acme.example").is_none());
    }
}
