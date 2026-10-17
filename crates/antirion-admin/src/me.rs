//! The signed-in user's own account: profile edits, preferences, password change and personal
//! admin tokens. Service accounts and the bootstrap credential have none of these.

use antirion_core::UtcMicros;
use antirion_core::audit::Outcome;
use antirion_identity::{Credential, Plane};
use antirion_runtime::changes::Target;
use antirion_store::AdminTokenRecord;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Map, Value, json};

use crate::call::Call;
use crate::commit::{Proposal, api_write};
use crate::error::AdminError;
use crate::local::proof;
use crate::mfa::own_user;

/// The most a preferences document may weigh.
const PREFERENCES_MAX: usize = 16 * 1024;
/// Members of the preferences the console knows; anything else is kept under `ui`.
const KNOWN: &[&str] = &[
    "theme",
    "accent",
    "density",
    "sidebar_collapsed",
    "reduced_motion",
    "compact_numbers",
    "week_start",
    "timezone",
    "locale",
    "default_organization",
    "time_range",
    "notifications",
    "ui",
];

/// Changes `users.<user>` with `edit` as an API write.
pub(crate) async fn edit_user(
    call: &Call,
    action: &str,
    user: &str,
    edit: impl FnOnce(&mut Map<String, Value>),
) -> Result<u64, AdminError> {
    let resource = format!("users/{user}");
    let committed = api_write(call, action, &resource, StatusCode::OK, |state| {
        let mut doc = state.doc.clone();
        let entry = doc
            .as_object_mut()
            .map(|o| o.entry("users").or_insert_with(|| json!({})))
            .and_then(Value::as_object_mut)
            .map(|users| users.entry(user.to_owned()).or_insert_with(|| json!({})))
            .and_then(Value::as_object_mut)
            .ok_or_else(AdminError::internal)?;
        edit(entry);
        Ok(Proposal {
            candidate: doc,
            written: vec![Target::Resource("users", user.to_owned())],
            credentials: Vec::new(),
            meta_edits: Vec::new(),
        })
    })
    .await?;
    Ok(committed.generation)
}

/// `PATCH /auth/me`: display name, title, and email (a local user's new email waits for the link
/// mailed to it; following it is `PATCH /auth/me` with that `email_token`).
pub(crate) async fn patch(call: &Call) -> Result<Response, AdminError> {
    call.allow_params(&[])?;
    let user = own_user(call)?;
    let m = call.json_body()?;
    let text = |name: &'static str, max: usize| -> Result<Option<String>, AdminError> {
        match m.get(name) {
            None => Ok(None),
            Some(Value::String(s))
                if !s.is_empty() && s.chars().count() <= max && !s.contains(['\r', '\n']) =>
            {
                Ok(Some(s.clone()))
            }
            Some(_) => Err(AdminError::invalid_parameter(format!(
                "{name} must be a string of 1 to {max} characters"
            ))),
        }
    };
    let display_name = text("display_name", 200)?;
    let title = text("title", 100)?;
    let email = text("email", 254)?;
    if email.as_deref().is_some_and(|e| {
        e.split_once('@')
            .is_none_or(|(l, d)| l.is_empty() || d.is_empty() || e.contains(' '))
    }) {
        return Err(AdminError::invalid_parameter("email must be an email address"));
    }
    let confirmed = match m.get("email_token").and_then(Value::as_str) {
        Some(t) => Some(
            call.rt
                .take_email_change(&user, t)
                .await
                .map_err(AdminError::identity)?
                .ok_or_else(|| AdminError::invalid_parameter("the token is not valid"))?,
        ),
        None => None,
    };
    let local = call.rt.has_password(&user).await.map_err(AdminError::identity)?;
    let mut direct_email = confirmed;
    if let Some(e) = &email {
        if local {
            if !call.rt.resets_enabled() {
                return Err(AdminError::invalid_parameter(
                    "the email of a local account changes through a mailed link, and mail is not enabled",
                ));
            }
            let token = call
                .rt
                .email_change_token(&user, e)
                .await
                .map_err(AdminError::identity)?;
            let base = format!(
                "{}{}",
                crate::oidc::own_origin(&call.rt, &call.headers),
                call.rt.statics.ui.path
            );
            call.rt.send_mail(
                e,
                "Confirm your new email address",
                format!(
                    "Confirm {e} as the email address of your console account:\n{base}/confirm-email?token={token}\n"
                ),
            );
        } else {
            direct_email = Some(e.clone());
        }
    }
    if display_name.is_some() || title.is_some() || direct_email.is_some() {
        edit_user(call, "auth.me.update", &user, |u| {
            if let Some(v) = display_name {
                u.insert("display_name".into(), Value::from(v));
            }
            if let Some(v) = title {
                u.insert("title".into(), Value::from(v));
            }
            if let Some(v) = direct_email {
                u.insert("email".into(), Value::from(v.to_lowercase()));
            }
        })
        .await?;
    }
    let snap = call.rt.gens.load();
    let body = crate::session::me_with(
        &call.rt,
        &snap,
        &call.principal,
        call.session.as_ref().map(|(_, s)| s),
    )
    .await;
    Ok(call.json(StatusCode::OK, &body))
}

/// Checks a preferences document: an object of at most 16 KiB whose known members have their
/// types; unknown top-level members move under `ui`.
fn preferences(body: &[u8]) -> Result<Value, AdminError> {
    if body.len() > PREFERENCES_MAX {
        return Err(AdminError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "Preferences may weigh at most 16 KiB.",
        ));
    }
    let v: Value = serde_json::from_slice(body)
        .map_err(|_| AdminError::invalid_parameter("the preferences must be a JSON object"))?;
    let Value::Object(m) = v else {
        return Err(AdminError::invalid_parameter(
            "the preferences must be a JSON object",
        ));
    };
    let mut out = Map::new();
    let mut ui = match m.get("ui") {
        None => Map::new(),
        Some(Value::Object(u)) => u.clone(),
        Some(_) => return Err(AdminError::invalid_parameter("ui must be an object")),
    };
    for (k, v) in m {
        let ok = match k.as_str() {
            "theme" => matches!(v.as_str(), Some("light" | "dark" | "system")),
            "density" => matches!(v.as_str(), Some("compact" | "comfortable")),
            "week_start" => matches!(v.as_str(), Some("monday" | "sunday")),
            "sidebar_collapsed" | "reduced_motion" | "compact_numbers" => v.is_boolean(),
            "accent" | "timezone" | "locale" | "default_organization" | "time_range" => v.is_string(),
            "notifications" | "ui" => v.is_object(),
            _ => true,
        };
        if !ok {
            return Err(AdminError::invalid_parameter(format!(
                "{k} has the wrong type or value"
            )));
        }
        if k == "ui" {
            continue;
        }
        if KNOWN.contains(&k.as_str()) {
            out.insert(k, v);
        } else {
            ui.insert(k, v);
        }
    }
    if !ui.is_empty() {
        out.insert("ui".into(), Value::Object(ui));
    }
    Ok(Value::Object(out))
}

/// `GET` and `PUT /auth/me/preferences`.
pub(crate) async fn prefs(call: &Call) -> Result<Response, AdminError> {
    call.allow_params(&[])?;
    let user = own_user(call)?;
    match call.method {
        Method::GET => {
            let v = call.rt.preferences(&user).await.map_err(AdminError::identity)?;
            Ok(call.json(StatusCode::OK, &v))
        }
        Method::PUT => {
            let v = preferences(&call.body)?;
            call.rt
                .set_preferences(&user, &v)
                .await
                .map_err(AdminError::identity)?;
            Ok(call.json(StatusCode::OK, &v))
        }
        _ => Err(AdminError::method_not_allowed("GET, PUT")),
    }
}

/// `POST /auth/me:change_password`.
pub(crate) async fn change_password(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&[])?;
    let user = own_user(call)?;
    let m = call.json_body()?;
    let field = |name: &'static str| {
        m.get(name)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| AdminError::invalid_parameter(format!("{name} is required")))
    };
    let current = field("current_password")?;
    let new = field("new_password")?;
    let proof = proof(&m)?;
    let keep = call.session.as_ref().map(|(_, s)| s.digest.clone());
    let resource = format!("users/{user}/password");
    match call
        .rt
        .change_password(&user, &current, &new, proof.as_ref(), keep.as_deref())
        .await
    {
        Ok(()) => {
            call.audit(
                "auth.me.change_password",
                &resource,
                Outcome::Success,
                StatusCode::NO_CONTENT,
            )
            .await;
            Ok(call.no_content())
        }
        Err(e) => {
            let e = AdminError::identity(e);
            call.audit("auth.me.change_password", &resource, Outcome::Failed, e.status)
                .await;
            Err(e)
        }
    }
}

fn token_json(r: &AdminTokenRecord) -> Value {
    json!({
        "kid": r.kid,
        "description": r.description,
        "access": r.access.as_deref().unwrap_or("full"),
        "created_at": r.created_at,
        "expires_at": r.expires_at,
        "last_used_at": r.last_used_us.map(|t| UtcMicros(t).to_rfc3339_seconds()),
    })
}

fn personal(call: &Call) -> Result<String, AdminError> {
    if !call.snap.identity.console.personal_tokens {
        return Err(AdminError::not_found());
    }
    own_user(call)
}

/// `GET` and `POST /auth/me/tokens`.
pub(crate) async fn tokens(call: &Call) -> Result<Response, AdminError> {
    let user = personal(call)?;
    match call.method {
        Method::GET => {
            call.allow_params(&["limit", "cursor"])?;
            let records = call
                .rt
                .store
                .admin_tokens_of(user)
                .await
                .map_err(|_| AdminError::store_unavailable())?;
            let items: Vec<Value> = records.iter().map(token_json).collect();
            Ok(call.json(StatusCode::OK, &json!({"items": items, "next_cursor": null})))
        }
        Method::POST => {
            call.allow_params(&[])?;
            let m = call.json_body()?;
            let now = call.rt.now();
            let expires = m
                .get("expires_at")
                .and_then(Value::as_str)
                .and_then(|s| UtcMicros::parse_rfc3339(s).ok())
                .ok_or_else(|| AdminError::invalid_parameter("expires_at must be an RFC 3339 time"))?;
            let longest = now.saturating_add(call.snap.identity.console.personal_max_lifetime);
            if expires <= now || expires > longest {
                return Err(AdminError::invalid_parameter(
                    "expires_at must be in the future and within personal_tokens.max_lifetime",
                ));
            }
            let access = match m.get("access").and_then(Value::as_str) {
                None | Some("full") => None,
                Some("read_only") => Some("read_only".to_owned()),
                Some(_) => return Err(AdminError::invalid_parameter("access must be full or read_only")),
            };
            let description = match m.get("description") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) if s.chars().count() <= 500 => Some(s.clone()),
                Some(_) => {
                    return Err(AdminError::invalid_parameter(
                        "description must be at most 500 characters",
                    ));
                }
            };
            let cred = Credential::generate(Plane::Admin).map_err(|_| AdminError::internal())?;
            let record = AdminTokenRecord {
                kid: cred.kid().to_owned(),
                user: user.clone(),
                verifier: call.rt.pepper.verifier(cred.expose()),
                description,
                created_at: now.to_rfc3339_seconds(),
                expires_at: Some(expires.to_rfc3339_seconds()),
                access,
                last_used_us: None,
            };
            call.rt
                .store
                .put_admin_token(record.clone())
                .await
                .map_err(|_| AdminError::store_unavailable())?;
            call.audit(
                "auth.me.tokens.create",
                &format!("users/{user}/tokens/{}", record.kid),
                Outcome::Success,
                StatusCode::CREATED,
            )
            .await;
            call.audit_read(
                "auth.me.tokens.revealed",
                &format!("users/{user}/tokens/{}", record.kid),
            )
            .await;
            let mut out = token_json(&record);
            if let Some(o) = out.as_object_mut() {
                o.insert("token".into(), Value::from(cred.expose()));
            }
            Ok(call.json(StatusCode::CREATED, &out))
        }
        _ => Err(AdminError::method_not_allowed("GET, POST")),
    }
}

/// `DELETE /auth/me/tokens/{kid}`.
pub(crate) async fn token(call: &Call, kid: &str) -> Result<Response, AdminError> {
    if call.method != Method::DELETE {
        return Err(AdminError::method_not_allowed("DELETE"));
    }
    call.allow_params(&[])?;
    let user = personal(call)?;
    let record = call
        .rt
        .store
        .admin_token(kid.to_owned())
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    if record.is_none_or(|r| r.user != user) {
        return Err(AdminError::not_found());
    }
    let store = &call.rt.store;
    store
        .delete_admin_token(kid.to_owned())
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    store
        .forget_token(kid.to_owned())
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    call.audit(
        "auth.me.tokens.delete",
        &format!("users/{user}/tokens/{kid}"),
        Outcome::Success,
        StatusCode::NO_CONTENT,
    )
    .await;
    Ok(call.no_content())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_keep_known_members_and_move_others_under_ui() {
        let v =
            preferences(br#"{"theme":"dark","density":"compact","sidebar":{"w":3},"ui":{"x":1}}"#).unwrap();
        assert_eq!(
            v,
            json!({"theme": "dark", "density": "compact", "ui": {"x": 1, "sidebar": {"w": 3}}})
        );
        assert!(preferences(br#"{"theme":"pink"}"#).is_err());
        assert!(preferences(b"[]").is_err());
        let big = format!("{{\"ui\":{{\"x\":\"{}\"}}}}", "a".repeat(17 * 1024));
        assert_eq!(
            preferences(big.as_bytes()).unwrap_err().status,
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }
}
