//! The public sign-in endpoints of the console: the login options, local sign-in (password step,
//! second-factor step and first enrolment), password reset and invitation acceptance. Each answers
//! without credentials; every outcome is audited.

use std::sync::Arc;

use antirion_core::RequestId;
use antirion_core::audit::{Actor, ActorKind, AuditEntry, Outcome};
use antirion_runtime::Runtime;
use antirion_runtime::admin_auth::{Client, Session};
use antirion_runtime::operators::{IdentityError, LoginStep, Proof};
use axum::body::Body;
use axum::response::Response;
use bytes::Bytes;
use http::header::{CONTENT_TYPE, USER_AGENT};
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use serde_json::{Map, Value, json};

use crate::error::AdminError;
use crate::session;

/// One unauthenticated request.
pub(crate) struct Public {
    pub(crate) rt: Arc<Runtime>,
    pub(crate) method: Method,
    pub(crate) headers: HeaderMap,
    pub(crate) query: Vec<(String, String)>,
    pub(crate) body: Bytes,
    pub(crate) peer: Option<String>,
    pub(crate) request_id: RequestId,
}

/// The public paths: they are matched exactly, before authentication.
pub(crate) fn is_public(path: &str) -> bool {
    matches!(
        path,
        "/admin/v1/auth/login_options"
            | "/admin/v1/auth/local/login"
            | "/admin/v1/auth/local/mfa"
            | "/admin/v1/auth/local/password_reset"
            | "/admin/v1/auth/local/password_reset:confirm"
            | "/admin/v1/auth/invitations:accept"
            | "/admin/v1/auth/invitations:check"
            | "/admin/v1/auth/local/password_reset:check"
    )
}

/// The enrolment paths a sign-in challenge may call without a session.
pub(crate) fn is_enrolment(path: &str) -> bool {
    matches!(
        path,
        "/admin/v1/auth/me/mfa/totp"
            | "/admin/v1/auth/me/mfa/totp:confirm"
            | "/admin/v1/auth/me/mfa/webauthn"
            | "/admin/v1/auth/me/mfa/webauthn:confirm"
    )
}

pub(crate) fn json_response(status: StatusCode, body: &Value) -> Response {
    let mut resp = Response::new(Body::from(serde_json::to_vec(body).unwrap_or_default()));
    *resp.status_mut() = status;
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    resp
}

impl Public {
    pub(crate) fn new(
        rt: &Arc<Runtime>,
        parts: &http::request::Parts,
        query: Vec<(String, String)>,
        body: Bytes,
        request_id: &RequestId,
    ) -> Public {
        Public {
            rt: Arc::clone(rt),
            method: parts.method.clone(),
            headers: parts.headers.clone(),
            query,
            body,
            peer: crate::client_addr::client_addr(rt, parts),
            request_id: request_id.clone(),
        }
    }

    fn only(&self, method: Method, allow: &'static str) -> Result<(), AdminError> {
        if self.method == method {
            Ok(())
        } else {
            Err(AdminError::method_not_allowed(allow))
        }
    }

    pub(crate) fn object(&self) -> Result<Map<String, Value>, AdminError> {
        let v: Value = serde_json::from_slice(&self.body).map_err(|e| {
            AdminError::malformed(vec![antirion_config::ConfigError::new(
                &antirion_config::DocPath::root(),
                antirion_config::DetailCode::MalformedDocument,
                e.to_string(),
            )])
        })?;
        match v {
            Value::Object(m) => Ok(m),
            _ => Err(AdminError::invalid_parameter("the body must be a JSON object")),
        }
    }

    pub(crate) fn client(&self, organization: Option<String>) -> Client {
        Client {
            ip: self.peer.clone(),
            user_agent: self
                .headers
                .get(USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.chars().take(512).collect()),
            organization,
        }
    }

    /// Audits a public action, its actor being the user when one is known.
    pub(crate) async fn audit(&self, action: &str, user: Option<&str>, outcome: Outcome, status: StatusCode) {
        let generation = self.rt.gens.load().generation;
        let entry = AuditEntry {
            id: RequestId::generate().as_str().to_owned(),
            time: self.rt.now().to_rfc3339(),
            actor: Actor {
                kind: ActorKind::Operator,
                id: user.map(str::to_owned),
            },
            action: action.to_owned(),
            resource: user.map_or_else(|| "auth".to_owned(), |u| format!("users/{u}")),
            outcome,
            status_code: status.as_u16(),
            generation_before: Some(generation),
            generation_after: Some(generation),
            request_id: self.request_id.as_str().to_owned(),
            source_ip: self.peer.clone(),
            kind: antirion_core::audit::AuditKind::Write,
            reason: None,
            changes: Value::Null,
        };
        if let Err(e) = self.rt.store.append_audit(entry).await {
            tracing::warn!(error = %e, "an audit entry could not be written");
        }
    }

    /// Audits a failure and turns it into the Admin API's error.
    async fn failed(&self, action: &str, user: Option<&str>, e: IdentityError) -> AdminError {
        let err = AdminError::identity(e);
        let outcome = if err.status == StatusCode::FORBIDDEN || err.status == StatusCode::UNAUTHORIZED {
            Outcome::Denied
        } else {
            Outcome::Failed
        };
        self.audit(action, user, outcome, err.status).await;
        if let (Some(u), Outcome::Denied) = (user, outcome) {
            self.rt.login_failed(u, action);
        }
        err
    }
}

fn string(m: &Map<String, Value>, name: &'static str) -> Result<String, AdminError> {
    m.get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| AdminError::invalid_parameter(format!("{name} is required")))
}

fn optional(m: &Map<String, Value>, name: &str) -> Option<String> {
    m.get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// The second factor in a body: `totp`, `webauthn` or `recovery_code`, exactly one.
pub(crate) fn proof(m: &Map<String, Value>) -> Result<Option<Proof>, AdminError> {
    let given: Vec<Proof> = [
        m.get("totp")
            .and_then(Value::as_str)
            .map(|s| Proof::Totp(s.to_owned())),
        m.get("recovery_code")
            .and_then(Value::as_str)
            .map(|s| Proof::RecoveryCode(s.to_owned())),
        m.get("webauthn")
            .filter(|v| v.is_object())
            .map(|v| Proof::Webauthn(v.clone())),
    ]
    .into_iter()
    .flatten()
    .collect();
    match given.len() {
        0 => Ok(None),
        1 => Ok(given.into_iter().next()),
        _ => Err(AdminError::invalid_parameter(
            "give one of totp, webauthn or recovery_code",
        )),
    }
}

pub(crate) fn step_json(step: &LoginStep) -> Value {
    let mut v = json!({"next": step.next, "challenge": step.challenge});
    if let (Some(options), Some(o)) = (&step.webauthn, v.as_object_mut()) {
        o.insert("webauthn_options".into(), options.clone());
    }
    v
}

/// The origin and console path links in mails point to.
fn link_base(rt: &Runtime, headers: &HeaderMap) -> String {
    let ui = &rt.statics.ui.path;
    format!("{}{ui}", crate::oidc::own_origin(rt, headers))
}

pub(crate) async fn handle(p: &Public, path: &str) -> Result<Response, AdminError> {
    match path {
        "/admin/v1/auth/login_options" => login_options(p).await,
        "/admin/v1/auth/local/login" => login(p).await,
        "/admin/v1/auth/local/mfa" => mfa(p).await,
        "/admin/v1/auth/local/password_reset" => reset(p).await,
        "/admin/v1/auth/local/password_reset:confirm" => reset_confirm(p).await,
        "/admin/v1/auth/invitations:accept" => crate::invitations::accept(p).await,
        "/admin/v1/auth/invitations:check" => Box::pin(crate::token_checks::invitation(p)).await,
        "/admin/v1/auth/local/password_reset:check" => Box::pin(crate::token_checks::reset(p)).await,
        _ => Err(AdminError::not_found()),
    }
}

/// `GET /auth/login_options[?organization=]`: the sign-in methods, and nothing an unauthenticated
/// caller should not see.
async fn login_options(p: &Public) -> Result<Response, AdminError> {
    p.only(Method::GET, "GET")?;
    if let Some((k, _)) = p.query.iter().find(|(k, _)| k != "organization") {
        return Err(AdminError::invalid_parameter(format!(
            "unknown query parameter {k:?}"
        )));
    }
    let snap = p.rt.gens.load();
    let id = &snap.identity;
    let organization = p
        .query
        .iter()
        .find(|(k, _)| k == "organization")
        .map(|(_, v)| v.clone())
        .filter(|o| id.organizations.contains_key(o));
    let mut oidc: Vec<(&String, &antirion_runtime::jwt::OidcProviderRt)> = id.oidc.iter().collect();
    oidc.sort_by(|a, b| a.0.cmp(b.0));
    let mut methods: Vec<Value> = oidc
        .into_iter()
        .map(|(name, prov)| {
            json!({"type": "oidc", "id": name, "display_name": prov.display_name.clone().unwrap_or_else(|| name.clone())})
        })
        .collect();
    let mut oauth: Vec<(&String, &antirion_runtime::jwt::OauthProviderRt)> = id.oauth.iter().collect();
    oauth.sort_by(|a, b| a.0.cmp(b.0));
    methods.extend(oauth.into_iter().map(|(name, prov)| {
        json!({"type": "oauth", "id": name, "display_name": prov.display_name.clone().unwrap_or_else(|| name.clone())})
    }));
    methods.extend(id.saml.iter().map(|(name, prov)| {
        json!({"type": "saml", "id": name, "display_name": prov.display_name.clone().unwrap_or_else(|| name.clone())})
    }));
    let org_allows = organization
        .as_ref()
        .and_then(|o| id.organizations.get(o))
        .is_none_or(|o| o.local_login);
    if id.console.local && org_allows {
        methods.push(json!({"type": "local", "id": "local", "display_name": "Email and password"}));
    }
    methods.push(json!({"type": "token", "id": "token", "display_name": "Admin token"}));
    let mut body = json!({
        "methods": methods,
        "password_reset": p.rt.resets_enabled(),
        "organization": organization,
        "version": id.console.show_version.then(|| p.rt.node.version.clone()),
        "ticker": if id.console.ticker { p.rt.login_ticker().await } else { Value::Null },
    });
    if let Some(o) = body.as_object_mut() {
        o.extend(crate::signin_facts::facts(&p.rt, id));
    }
    Ok(json_response(StatusCode::OK, &body))
}

/// `POST /auth/local/login`: the password step.
async fn login(p: &Public) -> Result<Response, AdminError> {
    p.only(Method::POST, "POST")?;
    if !p.rt.gens.load().identity.console.local {
        return Err(AdminError::not_found());
    }
    let m = p.object()?;
    let email = string(&m, "email")?;
    let password = string(&m, "password")?;
    let organization = optional(&m, "organization");
    match p
        .rt
        .local_login(
            &email,
            &password,
            organization,
            p.peer.as_deref(),
            &crate::oidc::own_origin(&p.rt, &p.headers),
        )
        .await
    {
        Ok(step) => {
            let user = p.rt.user_by_email(&email);
            p.audit(
                "auth.local.password",
                user.as_deref(),
                Outcome::Success,
                StatusCode::OK,
            )
            .await;
            Ok(json_response(StatusCode::OK, &step_json(&step)))
        }
        Err(e) => {
            let user = match &e {
                IdentityError::LocalLoginRefused => p.rt.user_by_email(&email),
                _ => None,
            };
            if user.is_none()
                && let Some(known) = p.rt.user_by_email(&email)
            {
                p.rt.login_failed(&known, "auth.local.password");
            }
            Err(p.failed("auth.local.password", user.as_deref(), e).await)
        }
    }
}

/// The response that starts a session: the `Me` document and the cookie.
pub(crate) async fn session_started(
    rt: &Runtime,
    id: &str,
    session: &Session,
    body: Option<Value>,
) -> Response {
    let max_age = rt.session_max_age(&session.facts).as_secs();
    let body = match body {
        Some(b) => b,
        None => session::me(rt, &session.principal, Some(session)).await,
    };
    let mut resp = json_response(StatusCode::OK, &body);
    session::set_cookie(&mut resp, id, max_age);
    resp
}

/// `POST /auth/local/mfa`: the second-factor step, which starts the session.
async fn mfa(p: &Public) -> Result<Response, AdminError> {
    p.only(Method::POST, "POST")?;
    if !p.rt.gens.load().identity.console.local {
        return Err(AdminError::not_found());
    }
    let m = p.object()?;
    let challenge = string(&m, "challenge")?;
    let proof = proof(&m)?.ok_or_else(|| AdminError::invalid_parameter("a second factor is required"))?;
    if let Some(Value::String(code)) = m.get("totp")
        && (code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(AdminError::invalid_parameter("totp must be six digits"));
    }
    let trust = m.get("trust_device").and_then(Value::as_bool).unwrap_or(false);
    let client = p.client(None);
    let origin = crate::oidc::own_origin(&p.rt, &p.headers);
    match p.rt.local_mfa(&challenge, &proof, trust, client, &origin).await {
        Ok((id, session)) => {
            p.audit(
                "auth.local.mfa",
                session.principal.user.as_deref(),
                Outcome::Success,
                StatusCode::OK,
            )
            .await;
            Ok(session_started(&p.rt, &id, &session, None).await)
        }
        Err(e) => Err(p.failed("auth.local.mfa", None, e).await),
    }
}

/// `POST /auth/local/password_reset`: always the same 202 while resets are offered.
async fn reset(p: &Public) -> Result<Response, AdminError> {
    p.only(Method::POST, "POST")?;
    if !p.rt.resets_enabled() {
        return Err(AdminError::not_found());
    }
    let m = p.object()?;
    let email = string(&m, "email")?;
    let base = link_base(&p.rt, &p.headers);
    match p.rt.request_reset(&email, &base, p.peer.as_deref()).await {
        Ok(()) => {
            p.audit(
                "auth.local.password_reset",
                None,
                Outcome::Success,
                StatusCode::ACCEPTED,
            )
            .await;
            Ok(json_response(
                StatusCode::ACCEPTED,
                &json!({"status": "accepted", "message": "If the address belongs to an account, a reset link was sent to it."}),
            ))
        }
        Err(e) => Err(p.failed("auth.local.password_reset", None, e).await),
    }
}

/// `POST /auth/local/password_reset:confirm`.
async fn reset_confirm(p: &Public) -> Result<Response, AdminError> {
    p.only(Method::POST, "POST")?;
    if !p.rt.resets_enabled() {
        return Err(AdminError::not_found());
    }
    let m = p.object()?;
    let token = string(&m, "token")?;
    let new_password = string(&m, "new_password")?;
    match p.rt.confirm_reset(&token, &new_password).await {
        Ok(step) => {
            p.audit(
                "auth.local.password_reset.confirm",
                None,
                Outcome::Success,
                StatusCode::OK,
            )
            .await;
            Ok(json_response(StatusCode::OK, &step_json(&step)))
        }
        Err(e) => Err(p.failed("auth.local.password_reset.confirm", None, e).await),
    }
}

/// The enrolment calls made with a sign-in challenge instead of a session: the challenge is in the
/// body, and confirming the factor starts the session.
pub(crate) async fn enrol(p: &Public, path: &str) -> Result<Response, AdminError> {
    p.only(Method::POST, "POST")?;
    if !p.rt.gens.load().identity.console.local {
        return Err(AdminError::not_found());
    }
    let m = if p.body.is_empty() {
        Map::new()
    } else {
        p.object()?
    };
    let Some(challenge) = optional(&m, "challenge") else {
        return Err(AdminError::unauthenticated(
            "A valid admin credential is required.",
        ));
    };
    p.rt.throttle_client(p.peer.as_deref())
        .map_err(AdminError::identity)?;
    let user = match p.rt.enrolment_user(&challenge).await {
        Ok(u) => u,
        Err(e) => {
            p.rt.count_failure(p.peer.as_deref());
            return Err(p.failed("auth.local.enrol", None, e).await);
        }
    };
    let origin = crate::oidc::own_origin(&p.rt, &p.headers);
    match path {
        "/admin/v1/auth/me/mfa/totp" => crate::mfa::start_totp(&p.rt, &user).await,
        "/admin/v1/auth/me/mfa/webauthn" => crate::mfa::start_webauthn(&p.rt, &user, &origin).await,
        "/admin/v1/auth/me/mfa/totp:confirm" | "/admin/v1/auth/me/mfa/webauthn:confirm" => {
            let codes = if path.ends_with("totp:confirm") {
                crate::mfa::confirm_totp(&p.rt, &user, &m).await
            } else {
                crate::mfa::confirm_webauthn(&p.rt, &user, &m, &origin).await
            };
            let codes = match codes {
                Ok(c) => c,
                Err(e) => {
                    p.rt.count_failure(p.peer.as_deref());
                    p.audit("auth.local.enrol", Some(&user), Outcome::Denied, e.status)
                        .await;
                    return Err(e);
                }
            };
            let (id, session) =
                p.rt.finish_enrolment(&challenge, p.client(None))
                    .await
                    .map_err(AdminError::identity)?;
            p.audit("auth.local.enrol", Some(&user), Outcome::Success, StatusCode::OK)
                .await;
            Ok(session_started(&p.rt, &id, &session, Some(json!({"recovery_codes": codes}))).await)
        }
        _ => Err(AdminError::not_found()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_public_set_is_exact() {
        assert!(is_public("/admin/v1/auth/login_options"));
        assert!(is_public("/admin/v1/auth/local/password_reset:confirm"));
        assert!(!is_public("/admin/v1/auth/login_options/x"));
        assert!(!is_public("/admin/v1/auth/local"));
        assert!(!is_public("/admin/v1/auth/me"));
        assert!(is_enrolment("/admin/v1/auth/me/mfa/totp:confirm"));
        assert!(!is_enrolment("/admin/v1/auth/me/mfa"));
    }

    #[test]
    fn exactly_one_factor_is_taken_from_a_body() {
        let m = |v: Value| v.as_object().cloned().unwrap_or_default();
        assert!(matches!(
            proof(&m(json!({"totp": "123456"}))),
            Ok(Some(Proof::Totp(_)))
        ));
        assert!(matches!(
            proof(&m(json!({"recovery_code": "ABCDE12345"}))),
            Ok(Some(Proof::RecoveryCode(_)))
        ));
        assert!(matches!(
            proof(&m(json!({"webauthn": {}}))),
            Ok(Some(Proof::Webauthn(_)))
        ));
        assert!(matches!(proof(&m(json!({}))), Ok(None)));
        assert!(proof(&m(json!({"totp": "1", "recovery_code": "x"}))).is_err());
    }

    #[test]
    fn a_login_step_carries_webauthn_options_only_when_there_are_some() {
        let step = LoginStep {
            next: "mfa",
            challenge: "c".into(),
            webauthn: None,
        };
        assert_eq!(step_json(&step), json!({"next": "mfa", "challenge": "c"}));
        let with = LoginStep {
            webauthn: Some(json!({"challenge": "x"})),
            ..step
        };
        assert_eq!(step_json(&with)["webauthn_options"]["challenge"], "x");
    }
}
