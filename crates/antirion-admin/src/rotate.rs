//! `POST /settings/security:rotate_secrets`: the incident rotation of everything the gateway
//! generated itself. Every operator session ends (the caller's too), the generated SCIM token and
//! every literal webhook signing secret are replaced with no overlap, and the new secrets are
//! returned once. Secrets configured by reference, and a configured SCIM token, are their owners' to
//! rotate and are listed as not rotated.

use antirion_config::DetailCode;
use antirion_config::model;
use antirion_config::secrets::has_reference;
use antirion_core::audit::Outcome;
use antirion_identity::rbac::Scope;
use antirion_runtime::alerting;
use antirion_runtime::changes::Target;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Map, Value, json};

use crate::call::Call;
use crate::commit::{Proposal, api_write};
use crate::danger_routes::{body, body_error, only};
use crate::error::AdminError;

const RESOURCE: &str = "settings/security";
const SCOPES: [&str; 3] = ["sessions", "scim_token", "webhook_secrets"];
/// Where a configured SCIM token lives, and the name the generated one is returned under.
const SCIM_PATH: &str = "auth.admin.scim.token";

/// The rotation's scopes, in their request order.
fn scopes(m: &Map<String, Value>) -> Result<Vec<String>, AdminError> {
    let list = match m.get("scope") {
        None | Some(Value::Null) => {
            return Err(body_error("scope", DetailCode::Required, "scope is required"));
        }
        Some(Value::Array(list)) => list,
        Some(_) => {
            return Err(body_error(
                "scope",
                DetailCode::TypeMismatch,
                "scope must be a list",
            ));
        }
    };
    if list.is_empty() {
        return Err(body_error(
            "scope",
            DetailCode::OutOfRange,
            "scope must name at least one",
        ));
    }
    let mut out = Vec::new();
    for v in list {
        let Some(s) = v.as_str().filter(|s| SCOPES.contains(s)) else {
            return Err(body_error(
                "scope",
                DetailCode::InvalidEnum,
                "scope items are sessions, scim_token or webhook_secrets",
            ));
        };
        if out.iter().any(|x| x == s) {
            return Err(body_error(
                "scope",
                DetailCode::DuplicateKey,
                "scope items are unique",
            ));
        }
        out.push(s.to_owned());
    }
    Ok(out)
}

/// The webhooks whose signing secret the gateway holds as a literal, and the paths of those whose
/// secret is a reference.
fn webhooks(doc: &Value) -> (Vec<String>, Vec<String>) {
    let mut literal = Vec::new();
    let mut referenced = Vec::new();
    for (name, spec) in doc
        .pointer("/observability/alerts/webhooks")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        match spec.get("signing_secret").and_then(Value::as_str) {
            Some(s) if has_reference(s) => referenced.push(path_of(name)),
            Some(_) => literal.push(name.clone()),
            None => {}
        }
    }
    (literal, referenced)
}

fn path_of(webhook: &str) -> String {
    format!("observability.alerts.webhooks.{webhook}.signing_secret")
}

/// Replaces the literal webhook signing secrets in one generation and ends their overlaps.
async fn rotate_webhooks(call: &Call, names: &[String]) -> Result<Map<String, Value>, AdminError> {
    let mut fresh = Vec::with_capacity(names.len());
    for n in names {
        fresh.push((
            n.clone(),
            alerting::generate_secret().map_err(|_| AdminError::internal())?,
        ));
    }
    let written = fresh.clone();
    Box::pin(api_write(
        call,
        "settings.security.rotate_webhook_secrets",
        RESOURCE,
        StatusCode::OK,
        |state| {
            let mut doc = state.doc.clone();
            for (name, secret) in &written {
                if let Some(entry) = doc
                    .pointer_mut(&format!("/observability/alerts/webhooks/{name}"))
                    .and_then(Value::as_object_mut)
                {
                    entry.insert("signing_secret".into(), Value::String(secret.clone()));
                }
            }
            Ok(Proposal {
                candidate: doc,
                written: vec![Target::Setting(
                    model::singleton("observability").map_or("observability", |s| s.name),
                )],
                credentials: Vec::new(),
                meta_edits: Vec::new(),
            })
        },
    ))
    .await?;
    let mut out = Map::new();
    for (name, secret) in fresh {
        alerting::keep_previous_secret(&call.rt, &name, None)
            .await
            .map_err(|_| AdminError::store_unavailable())?;
        out.insert(path_of(&name), Value::String(secret));
    }
    Ok(out)
}

pub(crate) async fn rotate(call: &Call) -> Result<Response, AdminError> {
    only(call, Method::POST, "POST")?;
    call.allow_params(&[])?;
    call.require("security:write", &[Scope::Gateway])
        .map_err(|e| e.with_resource(RESOURCE))?;
    let m = body(call, &["scope"])?;
    let scope = scopes(&m)?;
    if let Some(held) = Box::pin(crate::change_control::second_person(call, "secrets_rotate")).await? {
        return Ok(held);
    }
    let has = |s: &str| scope.iter().any(|x| x == s);
    let mut secrets = Map::new();
    let mut not_rotated = Vec::new();
    if has("scim_token") {
        if call.snap.identity.scim.token.is_some() {
            not_rotated.push(SCIM_PATH.to_owned());
        } else {
            let (secret, _) = crate::scim::regenerate(&call.rt, 0).await?;
            secrets.insert(SCIM_PATH.to_owned(), Value::String(secret));
        }
    }
    if has("webhook_secrets") {
        let (literal, referenced) = webhooks(&call.snap.config.doc);
        not_rotated.extend(referenced);
        if !literal.is_empty() {
            secrets.extend(Box::pin(rotate_webhooks(call, &literal)).await?);
        }
    }
    let sessions_revoked = if has("sessions") {
        Some(
            call.rt
                .store
                .delete_all_sessions()
                .await
                .map_err(|_| AdminError::store_unavailable())?,
        )
    } else {
        None
    };
    let rotated: Vec<&String> = secrets.keys().collect();
    call.audit_with(
        "settings.security.rotate_secrets",
        RESOURCE,
        Outcome::Success,
        StatusCode::OK,
        json!({"scope": scope, "rotated": rotated, "not_rotated": not_rotated,
               "sessions_revoked": sessions_revoked}),
    )
    .await;
    if !secrets.is_empty() {
        call.audit_read("settings.security.secrets_revealed", RESOURCE)
            .await;
    }
    let mut out = json!({"secrets": secrets, "not_rotated": not_rotated});
    if let Some(n) = sessions_revoked {
        out["sessions_revoked"] = json!(n);
    }
    Ok(call.json(StatusCode::OK, &out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_are_checked() {
        let ok = json!({"scope": ["webhook_secrets", "sessions"]});
        assert_eq!(
            scopes(ok.as_object().unwrap()).unwrap(),
            vec!["webhook_secrets", "sessions"]
        );
        for bad in [
            json!({}),
            json!({"scope": []}),
            json!({"scope": ["keys"]}),
            json!({"scope": ["sessions", "sessions"]}),
            json!({"scope": "sessions"}),
        ] {
            let e = scopes(bad.as_object().unwrap()).unwrap_err();
            assert_eq!(e.status, StatusCode::BAD_REQUEST);
        }
    }

    #[test]
    fn referenced_secrets_are_not_rotated() {
        let doc = json!({"observability": {"alerts": {"webhooks": {
            "a": {"url": "https://a", "signing_secret": "literal-secret-0123456789"},
            "b": {"url": "https://b", "signing_secret": "${env:HOOK_SECRET}"},
            "c": {"url": "https://c"}
        }}}});
        let (literal, referenced) = webhooks(&doc);
        assert_eq!(literal, vec!["a"]);
        assert_eq!(referenced, vec!["observability.alerts.webhooks.b.signing_secret"]);
    }
}
