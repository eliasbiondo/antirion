//! SAML 2.0 single sign-on for the admin plane: the service-provider-initiated sign-in (an
//! HTTP-Redirect `AuthnRequest` whose id is kept like an OIDC `state`, single use, ten minutes), the
//! assertion consumer service (HTTP-POST; signed assertion, audience, recipient, time window and
//! request checked; an assertion id is accepted once across nodes), the service provider's metadata,
//! and a provider's status and metadata resync. Sign-ins end like OIDC ones (`oidc::SignedIn`).

use std::sync::Arc;
use std::time::Duration;

use antirion_core::UtcMicros;
use antirion_core::audit::Outcome;
use antirion_identity::rbac::Scope;
use antirion_identity::saml::{self, Assertion, AuthnRequest, Expected, IdpSource, Provider, SamlError};
use antirion_identity::verifier::random_id;
use antirion_runtime::{OidcLogin, Runtime};
use axum::body::Body;
use axum::response::Response;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use http::header::CONTENT_TYPE;
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;
use crate::oidc::{SignedIn, own_origin, redirect, refused, safe_return, unavailable};

const REQUEST_LIFETIME: Duration = Duration::from_secs(600);
/// What a pending request is kept under, apart from OIDC states.
const LOGIN_PREFIX: &str = "saml:";
/// Authentication context classes that mean a second factor was used.
const MFA_CONTEXTS: [&str; 5] = [
    "mfa",
    "multipleauthn",
    "multifactor",
    "twofactor",
    "timesynctoken",
];

fn provider(rt: &Runtime, name: &str) -> Result<Arc<Provider>, AdminError> {
    rt.gens
        .load()
        .identity
        .saml
        .get(name)
        .cloned()
        .ok_or_else(AdminError::not_found)
}

fn base(origin: &str, name: &str) -> String {
    format!("{origin}/admin/v1/auth/saml/{name}")
}

fn sp_entity_id(p: &Provider, origin: &str, name: &str) -> String {
    p.sp_entity_id
        .clone()
        .unwrap_or_else(|| format!("{}/metadata", base(origin, name)))
}

/// `GET /admin/v1/auth/saml/{provider}/login?return_to=<path>`.
pub(crate) async fn login(
    rt: &Runtime,
    name: &str,
    params: &[(String, String)],
    headers: &HeaderMap,
    test: bool,
) -> Result<Response, AdminError> {
    let p = provider(rt, name)?;
    let idp = rt.saml_idp(name, &p).await.map_err(|_| unavailable())?;
    let origin = own_origin(rt, headers);
    let acs_url = format!("{}/acs", base(&origin, name));
    let id = random_id()
        .map(|r| format!("_{r}"))
        .map_err(|_| AdminError::internal())?;
    let now = rt.now();
    let return_to = params
        .iter()
        .find(|(k, _)| k == "return_to")
        .map_or("/", |(_, v)| v.as_str());
    let request = AuthnRequest {
        id: &id,
        issue_instant: &now.to_rfc3339_seconds(),
        destination: &idp.sso_url,
        sp_entity_id: &sp_entity_id(&p, &origin, name),
        acs_url: &acs_url,
        name_id_format: p.name_id_format,
    };
    let location = request.redirect_url(None);
    let pending = OidcLogin {
        provider: name.to_owned(),
        nonce: String::new(),
        verifier: String::new(),
        redirect_uri: acs_url,
        return_to: safe_return(return_to),
        started: now,
        test,
        trust_device: false,
    };
    if !rt
        .start_login(format!("{LOGIN_PREFIX}{id}"), pending, REQUEST_LIFETIME)
        .await
    {
        return Err(unavailable());
    }
    Ok(redirect(&location))
}

/// The public steps: `login` and `metadata` (GET) and `acs` (POST), before any credential.
pub(crate) async fn public(
    rt: &Runtime,
    name: &str,
    step: &str,
    method: &Method,
    params: &[(String, String)],
    headers: &HeaderMap,
    body: &[u8],
) -> Result<Response, AdminError> {
    match step {
        "acs" if *method == Method::POST => Box::pin(acs(rt, name, headers, body)).await,
        "acs" => Err(AdminError::method_not_allowed("POST")),
        _ if *method != Method::GET => Err(AdminError::method_not_allowed("GET")),
        "login" => Box::pin(login(rt, name, params, headers, false)).await,
        _ => metadata(rt, name, headers),
    }
}

/// `GET /admin/v1/auth/saml/{provider}/login?test=true`, for a caller with `security:write`.
pub(crate) async fn login_test(call: &Call, name: &str) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&["test", "return_to"])?;
    call.require("security:write", &[Scope::Gateway])?;
    Box::pin(login(&call.rt, name, &call.query, &call.headers, true)).await
}

/// `GET /admin/v1/auth/saml/{provider}/metadata`: the service provider's metadata.
pub(crate) fn metadata(rt: &Runtime, name: &str, headers: &HeaderMap) -> Result<Response, AdminError> {
    let p = provider(rt, name)?;
    let origin = own_origin(rt, headers);
    let xml = saml::sp_metadata(
        &sp_entity_id(&p, &origin, name),
        &format!("{}/acs", base(&origin, name)),
        p.name_id_format,
    );
    let mut resp = Response::new(Body::from(xml));
    resp.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/samlmetadata+xml"),
    );
    Ok(resp)
}

/// The members of an `application/x-www-form-urlencoded` body.
fn form(body: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(body);
    crate::query_pairs(Some(&text))
}

fn malformed() -> AdminError {
    AdminError::invalid_parameter("SAMLResponse must be the base64 of a SAML response.")
}

fn refusal(e: &SamlError) -> AdminError {
    match e {
        SamlError::Malformed => malformed(),
        other => refused(&other.to_string()),
    }
}

fn email_of(p: &Provider, a: &Assertion) -> Option<String> {
    let raw = match &p.email_attribute {
        Some(attr) => a.values(attr).first().map(|v| (*v).to_owned()),
        None => a.name_id.clone(),
    }?;
    let email = raw.trim().to_ascii_lowercase();
    is_address(&email).then_some(email)
}

/// One `@` between a non-empty local part and a non-empty domain.
fn is_address(email: &str) -> bool {
    email
        .split_once('@')
        .is_some_and(|(local, domain)| !local.is_empty() && !domain.is_empty() && !domain.contains('@'))
}

/// What a test sign-in reports of the assertion: everything but key material and ids.
fn claims(a: &Assertion) -> Value {
    let mut attributes = serde_json::Map::new();
    for (name, values) in &a.attributes {
        let entry = attributes
            .entry(name.clone())
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Value::Array(list) = entry {
            list.extend(values.iter().cloned().map(Value::String));
        }
    }
    json!({
        "name_id": a.name_id,
        "name_id_format": a.name_id_format,
        "authn_context": a.authn_context,
        "attributes": attributes,
    })
}

/// A pending request this response answers, or the unsolicited path when the provider allows it.
struct Answering {
    request_id: Option<String>,
    acs_url: String,
    return_to: String,
    test: bool,
}

async fn answering(
    rt: &Runtime,
    name: &str,
    p: &Provider,
    claimed: Option<String>,
    relay_state: Option<&str>,
    headers: &HeaderMap,
) -> Result<Answering, AdminError> {
    match claimed {
        Some(id) => {
            let login = rt
                .pending_login(&format!("{LOGIN_PREFIX}{id}"))
                .await
                .filter(|l| l.provider == name && rt.now().since(l.started) <= REQUEST_LIFETIME)
                .ok_or_else(|| refused(&SamlError::InResponseTo.to_string()))?;
            Ok(Answering {
                request_id: Some(id),
                acs_url: login.redirect_uri,
                return_to: login.return_to,
                test: login.test,
            })
        }
        None if p.allow_idp_initiated => Ok(Answering {
            request_id: None,
            acs_url: format!("{}/acs", base(&own_origin(rt, headers), name)),
            return_to: safe_return(relay_state.unwrap_or("/")),
            test: false,
        }),
        None => Err(refused(&SamlError::Unsolicited.to_string())),
    }
}

/// `POST /admin/v1/auth/saml/{provider}/acs`: the identity provider's HTTP-POST response.
pub(crate) async fn acs(
    rt: &Runtime,
    name: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<Response, AdminError> {
    let p = provider(rt, name)?;
    let fields = form(body);
    let get = |k: &str| fields.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    let encoded: String = get("SAMLResponse")
        .ok_or_else(malformed)?
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    let raw = STANDARD.decode(encoded).map_err(|_| malformed())?;
    if raw.len() > saml::MAX_RESPONSE_BYTES {
        return Err(malformed());
    }
    let xml = String::from_utf8(raw).map_err(|_| malformed())?;
    let claimed = saml::in_response_to(&xml).map_err(|_| malformed())?;
    let on = answering(rt, name, &p, claimed, get("RelayState"), headers).await?;
    let idp = rt.saml_idp(name, &p).await.map_err(|_| unavailable())?;
    let origin = own_origin(rt, headers);
    let sp = sp_entity_id(&p, &origin, name);
    let now = rt.now();
    let assertion = saml::validate(
        &xml,
        &Expected {
            idp_entity_id: &idp.entity_id,
            sp_entity_id: &sp,
            acs_url: &on.acs_url,
            request_id: on.request_id.as_deref(),
            certificates: &idp.certificates,
            now: now.0.div_euclid(1_000_000),
            skew: p.clock_skew,
        },
    )
    .map_err(|e| {
        tracing::info!(provider = name, reason = %e, "a SAML response was refused");
        refusal(&e)
    })?;
    if let Some(id) = &on.request_id
        && rt.take_login(&format!("{LOGIN_PREFIX}{id}")).await.is_none()
    {
        return Err(refused(&SamlError::InResponseTo.to_string()));
    }
    let first_use = rt
        .saml_assertion_once(
            name,
            &assertion.id,
            assertion.valid_until.saturating_mul(1_000_000),
        )
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    if !first_use {
        tracing::info!(provider = name, "a replayed SAML assertion was refused");
        return Err(refused(&SamlError::Replayed.to_string()));
    }
    Box::pin(sign_in(rt, name, &p, &assertion, on)).await
}

async fn sign_in(
    rt: &Runtime,
    name: &str,
    p: &Provider,
    a: &Assertion,
    on: Answering,
) -> Result<Response, AdminError> {
    let report = |allowed: bool, reason: Option<&str>, matched: &[Value], bindings: &[Value]| {
        crate::local::json_response(
            StatusCode::OK,
            &json!({"claims": claims(a), "matched_role_mappings": matched, "resulting_bindings": bindings,
                    "allowed": allowed, "reason": reason}),
        )
    };
    let Some(email) = email_of(p, a) else {
        if on.test {
            return Ok(report(
                false,
                Some("The assertion names no email address."),
                &[],
                &[],
            ));
        }
        return Err(refused("The assertion names no email address."));
    };
    if let Some(domains) = &p.allowed_email_domains {
        let domain = email.rsplit_once('@').map_or("", |(_, d)| d);
        if !domains.iter().any(|d| d.eq_ignore_ascii_case(domain)) {
            if on.test {
                return Ok(report(
                    false,
                    Some("This email domain may not sign in."),
                    &[],
                    &[],
                ));
            }
            return Err(refused("This email domain may not sign in."));
        }
    }
    let matched: Vec<Value> = p
        .role_mappings
        .iter()
        .filter(|(attribute, value, _, _)| a.values(attribute).contains(&value.as_str()))
        .map(|(attribute, value, role, scope)| json!({"attribute": attribute, "value": value, "role": role, "scope": scope}))
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
    if on.test {
        let known = rt.gens.load().identity.users.contains_key(&email);
        let reason = if bindings.is_empty() {
            Some("No role mapping applies to this user, and the provider gives no default bindings.")
        } else if !p.jit_provisioning && !known {
            Some("This user is not provisioned.")
        } else {
            None
        };
        return Ok(report(reason.is_none(), reason, &matched, &bindings));
    }
    let mfa = a.authn_context.as_deref().is_some_and(|c| {
        let c = c.to_ascii_lowercase();
        MFA_CONTEXTS.iter().any(|m| c.contains(m))
    });
    let signed = SignedIn {
        user: &email,
        provider: name,
        email: &email,
        bindings,
        jit: p.jit_provisioning,
        method: "saml",
        mfa,
    };
    signed.session(rt, &on.return_to).await
}

fn certificate_facts(c: &saml::Certificate) -> Value {
    json!({
        "subject": c.subject,
        "not_after": c.not_after.map(|s| UtcMicros(s.saturating_mul(1_000_000)).to_rfc3339_seconds()),
        "sha256": c.sha256,
    })
}

/// A provider's status: what an identity provider needs to know of the service provider, and what
/// the gateway knows of the identity provider.
async fn status_body(call: &Call, name: &str, p: &Provider) -> Result<Value, AdminError> {
    let origin = own_origin(&call.rt, &call.headers);
    let at = base(&origin, name);
    let inline = call
        .snap
        .config
        .doc
        .pointer(&format!("/auth/admin/sso/saml/{name}/idp/metadata"))
        .is_some();
    let idp = match &p.idp {
        IdpSource::Fixed(d) => json!({
            "source": if inline { "metadata" } else { "fields" },
            "metadata_url": null, "metadata_refresh": null,
            "entity_id": d.entity_id, "sso_url": d.sso_url,
            "certificates": d.certificates.iter().map(certificate_facts).collect::<Vec<_>>(),
            "last_sync_at": null, "last_attempt_at": null, "last_error": null,
        }),
        IdpSource::MetadataUrl { url, refresh, .. } => {
            let stored = call
                .rt
                .saml_stored(name)
                .await
                .map_err(|_| AdminError::store_unavailable())?
                .filter(|s| s.url == *url);
            let parsed = stored
                .as_ref()
                .filter(|s| s.fetched_us > 0)
                .and_then(|s| saml::parse_idp_metadata(&s.xml).ok());
            let at_us = |us: i64| (us > 0).then(|| UtcMicros(us).to_rfc3339_seconds());
            json!({
                "source": "metadata_url",
                "metadata_url": url,
                "metadata_refresh": antirion_config::scalar::format_duration(*refresh),
                "entity_id": parsed.as_ref().map(|d| d.entity_id.clone()),
                "sso_url": parsed.as_ref().map(|d| d.sso_url.clone()),
                "certificates": parsed.as_ref().map(|d| d.certificates.iter().map(certificate_facts).collect::<Vec<_>>()).unwrap_or_default(),
                "last_sync_at": stored.as_ref().and_then(|s| at_us(s.fetched_us)),
                "last_attempt_at": stored.as_ref().and_then(|s| at_us(s.attempted_us)),
                "last_error": stored.and_then(|s| s.last_error),
            })
        }
    };
    Ok(json!({
        "name": name,
        "display_name": p.display_name.clone().unwrap_or_else(|| name.to_owned()),
        "sp_entity_id": sp_entity_id(p, &origin, name),
        "acs_url": format!("{at}/acs"),
        "sp_metadata_url": format!("{at}/metadata"),
        "login_url": format!("{at}/login"),
        "name_id_format": p.name_id_format,
        "allow_idp_initiated": p.allow_idp_initiated,
        "idp": idp,
    }))
}

/// `GET /admin/v1/auth/saml/{provider}`.
pub(crate) async fn status(call: &Call, name: &str) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&[])?;
    call.require("security:read", &[Scope::Gateway])?;
    let p = provider(&call.rt, name)?;
    let body = Box::pin(status_body(call, name, &p)).await?;
    Ok(call.json(StatusCode::OK, &body))
}

/// `POST /admin/v1/auth/saml/{provider}:resync`: fetches the identity provider's metadata now.
pub(crate) async fn resync(call: &Call, name: &str) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&[])?;
    let resource = format!("auth/saml/{name}");
    call.require("security:write", &[Scope::Gateway])
        .map_err(|e| e.with_resource(&resource))?;
    let p = provider(&call.rt, name)?;
    let IdpSource::MetadataUrl { url, insecure, .. } = &p.idp else {
        return Err(AdminError::conflict(
            "conflict",
            "This provider's identity provider is configured inline; there is no metadata URL to fetch.",
            Vec::new(),
        ));
    };
    let stored = Box::pin(call.rt.saml_resync(name, url, *insecure))
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    let ok = stored.last_error.is_none();
    let (outcome, code) = if ok {
        (Outcome::Success, StatusCode::OK)
    } else {
        (Outcome::Failed, StatusCode::BAD_GATEWAY)
    };
    call.audit("auth.saml.resync", &resource, outcome, code).await;
    if !ok {
        return Err(unavailable());
    }
    let body = Box::pin(status_body(call, name, &p)).await?;
    Ok(call.json(StatusCode::OK, &body))
}

#[cfg(test)]
mod tests {
    use super::is_address;

    #[test]
    fn an_address_has_one_at_between_two_parts() {
        assert!(is_address("ada@corp.example"));
        for bad in [
            "ada",
            "@corp.example",
            "ada@",
            "a@b@corp.example",
            "ada@@corp.example",
            "@",
        ] {
            assert!(!is_address(bad), "{bad}");
        }
    }
}
