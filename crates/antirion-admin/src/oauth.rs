//! GitHub sign-in for the admin plane: OAuth 2.0 authorization code with PKCE and a single-use
//! `state`, like the OpenID Connect flow, without an ID token. The user is the account's verified
//! primary email read from the API; membership of `allowed_organizations` is required when the list
//! is not empty, and `role_mappings` match organizations and optionally teams.

use std::time::Duration;

use antirion_identity::verifier::random_id;
use antirion_runtime::jwt::{FetchCx, OauthProviderRt, get_json, http_json};
use antirion_runtime::{OidcLogin, Runtime};
use axum::response::Response;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use http::{HeaderMap, Method};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::error::AdminError;
use crate::oidc::{
    SignedIn, bad_state, bind, bound, fetch_cx, own_origin, query, redirect, refused, safe_return,
    unavailable,
};

const STATE_LIFETIME: Duration = Duration::from_secs(600);
const SCOPES: &str = "read:user user:email read:org";
/// What marks a GitHub login among the logins in progress.
const KIND: &str = "oauth:";

fn provider(rt: &Runtime, name: &str) -> Result<OauthProviderRt, AdminError> {
    rt.gens
        .load()
        .identity
        .oauth
        .get(name)
        .cloned()
        .ok_or_else(AdminError::not_found)
}

/// `GET /admin/v1/auth/oauth/{provider}/login?return_to=<path>`.
pub(crate) async fn login(
    rt: &Runtime,
    name: &str,
    params: &[(String, String)],
    headers: &HeaderMap,
) -> Result<Response, AdminError> {
    let p = provider(rt, name)?;
    let (state, verifier) = match (random_id(), random_id()) {
        (Ok(s), Ok(v)) => (s, v),
        _ => return Err(AdminError::internal()),
    };
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let redirect_uri = format!("{}/admin/v1/auth/oauth/{name}/callback", own_origin(rt, headers));
    let return_to = params
        .iter()
        .find(|(k, _)| k == "return_to")
        .map_or("/", |(_, v)| v.as_str());
    let q = query(&[
        ("response_type", "code"),
        ("client_id", &p.client_id),
        ("redirect_uri", &redirect_uri),
        ("scope", SCOPES),
        ("state", &state),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
    ]);
    let login = OidcLogin {
        provider: format!("{KIND}{name}"),
        nonce: String::new(),
        verifier,
        redirect_uri,
        return_to: safe_return(return_to),
        started: rt.now(),
        test: false,
        trust_device: false,
    };
    if !rt.start_login(state.clone(), login, STATE_LIFETIME).await {
        return Err(unavailable());
    }
    let mut resp = redirect(&format!("{}/login/oauth/authorize?{q}", p.web_base_url));
    bind(&mut resp, &state);
    Ok(resp)
}

fn logins(v: &Value, field: &str) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| o.get(field).and_then(Value::as_str))
        .map(str::to_ascii_lowercase)
        .collect()
}

/// `(organization, team slug)` of every team the user is in.
fn teams(v: &Value) -> Vec<(String, String)> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            Some((
                t.pointer("/organization/login")?.as_str()?.to_ascii_lowercase(),
                t.get("slug")?.as_str()?.to_ascii_lowercase(),
            ))
        })
        .collect()
}

/// The verified primary email, else any verified one.
fn verified_email(v: &Value) -> Option<String> {
    let emails = v.as_array()?;
    let verified = |e: &&Value| e.get("verified") == Some(&Value::Bool(true));
    emails
        .iter()
        .find(|e| verified(e) && e.get("primary") == Some(&Value::Bool(true)))
        .or_else(|| emails.iter().find(verified))
        .and_then(|e| e.get("email"))
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
}

/// `GET /admin/v1/auth/oauth/{provider}/callback?code=…&state=…`.
pub(crate) async fn callback(
    rt: &Runtime,
    name: &str,
    params: &[(String, String)],
    headers: &HeaderMap,
) -> Result<Response, AdminError> {
    let get = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let state = get("state").ok_or_else(bad_state)?;
    if !bound(headers, &state) {
        return Err(bad_state());
    }
    let login = rt.take_login(&state).await.ok_or_else(bad_state)?;
    if login.provider != format!("{KIND}{name}") || rt.now().since(login.started) > STATE_LIFETIME {
        return Err(bad_state());
    }
    let code = get("code").ok_or_else(|| AdminError::invalid_parameter("code is required"))?;
    let p = provider(rt, name)?;
    let (tls, policy) = fetch_cx(rt).ok_or_else(unavailable)?;
    let cx = FetchCx {
        client: &rt.egress,
        policy: &policy,
        tls: &tls,
        now: rt.now(),
        secure: false,
    };
    let form = query(&[
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("redirect_uri", &login.redirect_uri),
        ("client_id", &p.client_id),
        ("client_secret", &p.client_secret),
        ("code_verifier", &login.verifier),
    ]);
    let tokens = http_json(
        &cx,
        Method::POST,
        &format!("{}/login/oauth/access_token", p.web_base_url),
        Some(form),
    )
    .await
    .ok_or_else(unavailable)?;
    let token = tokens
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| AdminError::unauthenticated("The authorization code was not accepted."))?
        .to_owned();
    let api = &p.api_base_url;
    let emails = get_json(&cx, &format!("{api}/user/emails"), &token)
        .await
        .ok_or_else(unavailable)?;
    let email =
        verified_email(&emails).ok_or_else(|| refused("The account has no verified email address."))?;
    let orgs = logins(
        &get_json(&cx, &format!("{api}/user/orgs"), &token)
            .await
            .ok_or_else(unavailable)?,
        "login",
    );
    let allowed = p
        .allowed_organizations
        .iter()
        .any(|o| orgs.contains(&o.to_ascii_lowercase()));
    if !p.allowed_organizations.is_empty() && !allowed {
        return Err(refused("The account is not a member of an allowed organization."));
    }
    let needs_teams = p.role_mappings.iter().any(|(_, t, _, _)| t.is_some());
    let team_list = if needs_teams {
        teams(
            &get_json(&cx, &format!("{api}/user/teams"), &token)
                .await
                .ok_or_else(unavailable)?,
        )
    } else {
        Vec::new()
    };
    let mut bindings: Vec<Value> = Vec::new();
    for (org, team, role, scope) in &p.role_mappings {
        let org = org.to_ascii_lowercase();
        let member = match team {
            None => orgs.contains(&org),
            Some(t) => team_list.contains(&(org.clone(), t.to_ascii_lowercase())),
        };
        let b = json!({"role": role, "scope": scope});
        if member && !bindings.contains(&b) {
            bindings.push(b);
        }
    }
    SignedIn {
        user: &email,
        provider: name,
        email: &email,
        bindings,
        jit: p.jit_provisioning,
        method: "oauth",
        mfa: false,
    }
    .session(rt, &login.return_to)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_verified_primary_email_wins() {
        let v = json!([
            {"email": "Old@Example.com", "verified": true, "primary": false},
            {"email": "Me@Example.com", "verified": true, "primary": true},
        ]);
        assert_eq!(verified_email(&v).as_deref(), Some("me@example.com"));
        let unverified = json!([{"email": "x@example.com", "verified": false, "primary": true}]);
        assert_eq!(verified_email(&unverified), None);
        let t = json!([{"slug": "Core", "organization": {"login": "Acme"}}]);
        assert_eq!(teams(&t), vec![("acme".to_owned(), "core".to_owned())]);
    }
}
