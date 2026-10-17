//! OpenID Connect single sign-on for the admin plane: the authorization-code flow with PKCE, a
//! single-use `state` valid for ten minutes and a `nonce`; the ID token is checked like a data-plane
//! JWT with the client id as audience, then the user's bindings come from the role mappings.

use std::time::Duration;

use antirion_identity::jwt::{self, Alg, Rules};
use antirion_identity::verifier::random_id;
use antirion_runtime::admin_auth::{Client, SessionFacts};
use antirion_runtime::changes::Target;
use antirion_runtime::jwt::{FetchCx, OidcProviderRt, fetch, http_json};
use antirion_runtime::{Mutation, OidcLogin, Runtime};
use axum::body::Body;
use axum::response::Response;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use http::header::{COOKIE, LOCATION, SET_COOKIE};
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::error::AdminError;

const STATE_LIFETIME: Duration = Duration::from_secs(600);

pub(crate) fn form_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(char::from(b)),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub(crate) fn query(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", form_encode(k), form_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

pub(crate) fn redirect(location: &str) -> Response {
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::FOUND;
    if let Ok(v) = HeaderValue::from_str(location) {
        resp.headers_mut().insert(LOCATION, v);
    }
    resp
}

/// `return_to` when it is a path on this site: one `/` followed by anything but `/` or `\\`, and
/// no backslash, control character or whitespace anywhere (a browser drops tabs and newlines from
/// a URL, so `/\t/host` would become `//host`).
pub(crate) fn safe_return(r: &str) -> String {
    let mut chars = r.chars();
    let shape = chars.next() == Some('/') && chars.next().is_none_or(|c| c != '/' && c != '\\');
    let clean = !r
        .chars()
        .any(|c| c == '\\' || c.is_control() || c.is_whitespace());
    if shape && clean && !r.contains("://") {
        r.to_owned()
    } else {
        "/".to_owned()
    }
}

/// The cookie that binds a login's `state` to the browser that started it, so a callback link
/// made by someone else's login signs nobody in.
const BINDING: &str = "antirion_sso";

fn binding_of(state: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(state.as_bytes()))
}

/// Binds `state` to the browser `resp` answers.
pub(crate) fn bind(resp: &mut Response, state: &str) {
    let cookie = format!(
        "{BINDING}={}; HttpOnly; Secure; SameSite=Lax; Path=/admin/v1/auth; Max-Age={}",
        binding_of(state),
        STATE_LIFETIME.as_secs()
    );
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        resp.headers_mut().append(SET_COOKIE, v);
    }
}

/// Whether the browser that sent `headers` started the login of `state`.
pub(crate) fn bound(headers: &HeaderMap, state: &str) -> bool {
    let want = binding_of(state);
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|c| c.trim().split_once('='))
        .any(|(k, v)| k == BINDING && v == want)
}

/// Clears the binding once the callback ran.
pub(crate) fn unbind(resp: &mut Response) {
    resp.headers_mut().append(
        SET_COOKIE,
        HeaderValue::from_static(
            "antirion_sso=; HttpOnly; Secure; SameSite=Lax; Path=/admin/v1/auth; Max-Age=0",
        ),
    );
}

pub(crate) fn own_origin(rt: &Runtime, headers: &HeaderMap) -> String {
    if let Some(o) = rt.gens.load().identity.settings.public_origin.clone() {
        return o.trim_end_matches('/').to_owned();
    }
    let host = headers
        .get(http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    format!("http://{host}")
}

pub(crate) fn fetch_cx(
    rt: &Runtime,
) -> Option<(
    antirion_egress::TlsProfile,
    std::sync::Arc<antirion_egress::EgressPolicy>,
)> {
    let snap = rt.gens.load();
    Some((
        snap.security.egress_tls.clone()?,
        std::sync::Arc::clone(&snap.security.egress),
    ))
}

async fn discovery(rt: &Runtime, p: &OidcProviderRt) -> Option<Value> {
    let (tls, policy) = fetch_cx(rt)?;
    let cx = FetchCx {
        client: &rt.egress,
        policy: &policy,
        tls: &tls,
        now: rt.now(),
        secure: rt.test.is_none() && !p.allow_insecure_discovery,
    };
    let url = format!(
        "{}/.well-known/openid-configuration",
        p.issuer.trim_end_matches('/')
    );
    http_json(&cx, Method::GET, &url, None).await
}

fn provider(rt: &Runtime, name: &str) -> Result<OidcProviderRt, AdminError> {
    rt.gens
        .load()
        .identity
        .oidc
        .get(name)
        .cloned()
        .ok_or_else(AdminError::not_found)
}

pub(crate) fn unavailable() -> AdminError {
    AdminError::new(
        StatusCode::BAD_GATEWAY,
        "idp_unavailable",
        "The identity provider could not be reached.",
    )
}

/// `GET /admin/v1/auth/oidc/{provider}/login?test=true`: a test sign-in, for a caller with
/// `security:write`; its callback reports what a sign-in would do and starts no session.
pub(crate) async fn login_test(call: &crate::call::Call, name: &str) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&["test", "return_to"])?;
    call.require("security:write", &[antirion_identity::rbac::Scope::Gateway])?;
    login(&call.rt, name, &call.query, &call.headers, true).await
}

/// `GET /admin/v1/auth/oidc/{provider}/login?return_to=<path>`.
pub(crate) async fn login(
    rt: &Runtime,
    name: &str,
    params: &[(String, String)],
    headers: &HeaderMap,
    test: bool,
) -> Result<Response, AdminError> {
    let p = provider(rt, name)?;
    let meta = discovery(rt, &p).await.ok_or_else(unavailable)?;
    let authorize = meta
        .get("authorization_endpoint")
        .and_then(Value::as_str)
        .ok_or_else(unavailable)?;
    let (state, nonce, verifier) = match (random_id(), random_id(), random_id()) {
        (Ok(s), Ok(n), Ok(v)) => (s, n, v),
        _ => return Err(AdminError::internal()),
    };
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let redirect_uri = format!("{}/admin/v1/auth/oidc/{name}/callback", own_origin(rt, headers));
    let return_to = params
        .iter()
        .find(|(k, _)| k == "return_to")
        .map_or("/", |(_, v)| v.as_str());
    let scopes = p.scopes.join(" ");
    let q = query(&[
        ("response_type", "code"),
        ("client_id", &p.client_id),
        ("redirect_uri", &redirect_uri),
        ("scope", &scopes),
        ("state", &state),
        ("nonce", &nonce),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
    ]);
    let now = rt.now();
    let login = OidcLogin {
        provider: name.to_owned(),
        nonce,
        verifier,
        redirect_uri,
        return_to: safe_return(return_to),
        started: now,
        test,
        trust_device: !test && params.iter().any(|(k, v)| k == "trust_device" && v == "true"),
    };
    if !rt.start_login(state.clone(), login, STATE_LIFETIME).await {
        return Err(unavailable());
    }
    let sep = if authorize.contains('?') { '&' } else { '?' };
    let mut resp = redirect(&format!("{authorize}{sep}{q}"));
    bind(&mut resp, &state);
    Ok(resp)
}

pub(crate) fn bad_state() -> AdminError {
    AdminError::invalid_parameter("The login state is unknown, used or expired.")
}

pub(crate) fn refused(message: &str) -> AdminError {
    AdminError::new(StatusCode::FORBIDDEN, "forbidden", message.to_owned())
}

fn claim_values(claims: &serde_json::Map<String, Value>, name: &str) -> Vec<String> {
    match jwt::claim(claims, name) {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
        _ => Vec::new(),
    }
}

/// `GET /admin/v1/auth/oidc/{provider}/callback?code=…&state=…`.
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
    if login.provider != name || rt.now().since(login.started) > STATE_LIFETIME {
        return Err(bad_state());
    }
    let code = get("code").ok_or_else(|| AdminError::invalid_parameter("code is required"))?;
    let p = provider(rt, name)?;
    let meta = discovery(rt, &p).await.ok_or_else(unavailable)?;
    let token_endpoint = meta
        .get("token_endpoint")
        .and_then(Value::as_str)
        .ok_or_else(unavailable)?;
    let jwks_uri = meta
        .get("jwks_uri")
        .and_then(Value::as_str)
        .ok_or_else(unavailable)?;
    let (tls, policy) = fetch_cx(rt).ok_or_else(unavailable)?;
    let cx = FetchCx {
        client: &rt.egress,
        policy: &policy,
        tls: &tls,
        now: rt.now(),
        secure: rt.test.is_none() && !p.allow_insecure_discovery,
    };
    let form = query(&[
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("redirect_uri", &login.redirect_uri),
        ("client_id", &p.client_id),
        ("client_secret", &p.client_secret),
        ("code_verifier", &login.verifier),
    ]);
    let tokens = http_json(&cx, Method::POST, token_endpoint, Some(form))
        .await
        .ok_or_else(unavailable)?;
    let id_token = tokens
        .get("id_token")
        .and_then(Value::as_str)
        .ok_or_else(unavailable)?;
    let keys = fetch(&cx, jwks_uri).await.ok_or_else(unavailable)?;
    let rules = Rules {
        issuer: p.issuer.clone(),
        audiences: vec![p.client_id.clone()],
        algorithms: Alg::DEFAULT.to_vec(),
        hmac_secret: None,
        clock_skew: 60,
        max_lifetime: None,
    };
    let invalid = |_| AdminError::unauthenticated("The ID token is not valid.");
    let t = jwt::decode(id_token).map_err(invalid)?;
    jwt::verify_signature(&t, &rules, &keys).map_err(invalid)?;
    jwt::verify_claims(&t, &rules, rt.now().0 / 1_000_000).map_err(invalid)?;
    if t.claims.get("nonce").and_then(Value::as_str) != Some(login.nonce.as_str()) {
        return Err(AdminError::unauthenticated("The ID token is not valid."));
    }
    let email = t
        .claims
        .get("email")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| refused("The ID token names no email address."))?;
    if let Some(domains) = &p.allowed_email_domains {
        let domain = email.rsplit_once('@').map_or("", |(_, d)| d);
        if !domains.iter().any(|d| d.eq_ignore_ascii_case(domain)) {
            if login.test {
                return Ok(crate::local::json_response(
                    StatusCode::OK,
                    &json!({"claims": t.claims, "matched_role_mappings": [], "resulting_bindings": [],
                            "allowed": false, "reason": "This email domain may not sign in."}),
                ));
            }
            return Err(refused("This email domain may not sign in."));
        }
    }
    let matched: Vec<Value> = p
        .role_mappings
        .iter()
        .filter(|(claim, value, _, _)| claim_values(&t.claims, claim).contains(value))
        .map(|(claim, value, role, scope)| json!({"claim": claim, "value": value, "role": role, "scope": scope}))
        .collect();
    let mut bindings: Vec<Value> = matched
        .iter()
        .map(|m| json!({"role": m["role"], "scope": m["scope"]}))
        .collect();
    if bindings.is_empty() {
        bindings = p
            .default_role_bindings
            .iter()
            .map(|(role, scope)| json!({"role": role, "scope": scope}))
            .collect();
    }
    let user = email.clone();
    if login.test {
        let known = rt.gens.load().identity.users.contains_key(&user);
        let reason = if bindings.is_empty() {
            Some("No role mapping applies to this user, and the provider gives no default bindings.")
        } else if !p.jit_provisioning && !known {
            Some("This user is not provisioned.")
        } else {
            None
        };
        let mut claims = t.claims.clone();
        for secret in ["at_hash", "c_hash", "nonce"] {
            claims.remove(secret);
        }
        return Ok(crate::local::json_response(
            StatusCode::OK,
            &json!({
                "claims": claims,
                "matched_role_mappings": matched,
                "resulting_bindings": bindings,
                "allowed": reason.is_none(),
                "reason": reason,
            }),
        ));
    }
    let mfa = claim_values(&t.claims, "amr")
        .iter()
        .any(|m| matches!(m.as_str(), "mfa" | "otp" | "hwk" | "swk"));
    let signed = SignedIn {
        user: &user,
        provider: name,
        email: &email,
        bindings,
        jit: p.jit_provisioning,
        method: "oidc",
        mfa,
    };
    signed
        .session_trusted(rt, &login.return_to, login.trust_device && mfa)
        .await
}

/// A user an identity provider vouched for, with the bindings its mappings gave.
pub(crate) struct SignedIn<'a> {
    pub(crate) user: &'a str,
    /// The identity provider's name in the configuration.
    pub(crate) provider: &'a str,
    pub(crate) email: &'a str,
    pub(crate) bindings: Vec<Value>,
    pub(crate) jit: bool,
    pub(crate) method: &'static str,
    pub(crate) mfa: bool,
}

impl SignedIn<'_> {
    /// Completes an invitation or provisions the user, then starts its session and redirects to
    /// `return_to`.
    pub(crate) async fn session(self, rt: &Runtime, return_to: &str) -> Result<Response, AdminError> {
        self.session_trusted(rt, return_to, false).await
    }

    /// As `session`; with `trust`, the session is a trusted-device one while trusted devices are on.
    pub(crate) async fn session_trusted(
        self,
        rt: &Runtime,
        return_to: &str,
        trust: bool,
    ) -> Result<Response, AdminError> {
        let invited = rt
            .invitation_of_user(self.user)
            .await
            .map_err(|_| AdminError::store_unavailable())?;
        if let Some(inv) = &invited {
            accept_invitation(rt, inv).await?;
        } else {
            if self.bindings.is_empty() {
                return Err(refused("No role mapping applies to this user."));
            }
            let known = rt.gens.load().identity.users.contains_key(self.user);
            if self.jit {
                if !known && !crate::users_cap::room_for_one(rt).await {
                    return Err(refused("The gateway has as many active users as it allows."));
                }
                provision(rt, self.user, self.email, self.bindings).await?;
            } else if !known {
                return Err(refused("This user is not provisioned."));
            }
        }
        let trusted = trust && rt.gens.load().identity.console.trusted.is_some();
        let facts = SessionFacts::new(self.method, self.mfa, trusted, Client::default());
        let (id, session) = rt
            .start_user_session(self.user, facts.with_provider(self.provider))
            .await
            .map_err(|_| refused("No session can be started for this user."))?;
        let max_age = rt.session_max_age(&session.facts).as_secs();
        let mut resp = redirect(return_to);
        crate::session::set_cookie(&mut resp, &id, max_age);
        Ok(resp)
    }
}

/// Completes an invitation by the first single sign-on of its user: the user is enabled with the
/// invitation's bindings, and the invitation is gone.
async fn accept_invitation(
    rt: &Runtime,
    inv: &antirion_runtime::operators::Invitation,
) -> Result<(), AdminError> {
    let user = inv.user.clone();
    let result = rt
        .mutate(|state| {
            let mut doc = state.doc.clone();
            let Some(entry) = doc
                .pointer_mut(&format!("/users/{user}"))
                .and_then(Value::as_object_mut)
            else {
                return Err(());
            };
            entry.insert("enabled".into(), Value::Bool(true));
            Ok(Mutation {
                candidate: doc,
                writer: "api".to_owned(),
                written: vec![Target::Resource("users", user.clone())],
                credentials: Vec::new(),
                meta_edits: Vec::new(),
                dry_run: false,
                source: "sso".to_owned(),
                audit: None,
            })
        })
        .await;
    match result {
        Ok(_) => {
            let _ = rt.delete_invitation(inv).await;
            Ok(())
        }
        Err(antirion_runtime::CommitError::Rejected(())) => Err(refused("This user is not provisioned.")),
        Err(antirion_runtime::CommitError::Invalid(details)) => Err(AdminError::validation(details)),
        Err(_) => Err(AdminError::store_unavailable()),
    }
}

/// Creates or updates the user with the bindings its sign-in mapped to.
async fn provision(rt: &Runtime, user: &str, email: &str, bindings: Vec<Value>) -> Result<(), AdminError> {
    let result = rt
        .mutate(|state| {
            let mut doc = state.doc.clone();
            let users = doc
                .as_object_mut()
                .map(|o| o.entry("users").or_insert_with(|| json!({})));
            let Some(Value::Object(users)) = users else {
                return Err(());
            };
            let entry = users.entry(user.to_owned()).or_insert_with(|| json!({}));
            if let Some(o) = entry.as_object_mut() {
                o.insert("email".into(), Value::from(email));
                o.insert("role_bindings".into(), Value::Array(bindings));
            }
            Ok(Mutation {
                candidate: doc,
                writer: "api".to_owned(),
                written: vec![Target::Resource("users", user.to_owned())],
                credentials: Vec::new(),
                meta_edits: Vec::new(),
                dry_run: false,
                source: "sso".to_owned(),
                audit: None,
            })
        })
        .await;
    match result {
        Ok(_) => Ok(()),
        Err(antirion_runtime::CommitError::Invalid(details)) => Err(AdminError::validation(details)),
        Err(_) => Err(AdminError::store_unavailable()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_login_is_bound_to_the_browser_that_started_it() {
        let mut resp = redirect("/x");
        bind(&mut resp, "state-1");
        let set = resp
            .headers()
            .get(SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        assert!(set.contains("HttpOnly") && set.contains("SameSite=Lax"), "{set}");
        let cookie = set.split(';').next().unwrap_or_default().to_owned();
        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            HeaderValue::from_str(&format!("a=b; {cookie}")).unwrap_or_else(|_| unreachable!()),
        );
        assert!(bound(&headers, "state-1"));
        assert!(!bound(&headers, "state-2"));
        assert!(!bound(&HeaderMap::new(), "state-1"));
    }

    #[test]
    fn return_to_stays_on_site() {
        assert_eq!(safe_return("/ui/keys"), "/ui/keys");
        assert_eq!(safe_return("/"), "/");
        for bad in [
            "https://evil.example",
            "//evil.example/x",
            "evil",
            "/\\evil.example",
            "/\t/evil.example",
            "/\n/evil.example",
            "/ /evil.example",
            "/ui/a\u{7f}b",
        ] {
            assert_eq!(safe_return(bad), "/", "{bad}");
        }
        assert_eq!(query(&[("a", "b c/d")]), "a=b%20c%2Fd");
    }
}
