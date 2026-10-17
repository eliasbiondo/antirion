//! Stage `authenticate`: the credential is selected from headers only and checked, with the key's
//! state, its client IP and endpoint scopes, its request controls and the end-user header, before
//! any body byte is read.

use std::net::IpAddr;
use std::sync::Arc;

use antirion_core::Stage;
use antirion_dialects::Endpoint;
use antirion_error::{GatewayError, Message};
use antirion_identity::enduser;
use antirion_identity::reasons;
use antirion_identity::select::{Presented, Refusal, select};
use antirion_runtime::identity::{IdentitySnapshot, KeyRt};
use http::HeaderValue;
use http::header::{RETRY_AFTER, WWW_AUTHENTICATE};
use http::request::Parts;

use crate::controls::{self, Controls, END_USER};
use crate::ctx::Ctx;

/// What a request that passed `authenticate` carries to the later stages.
#[derive(Debug)]
pub(crate) struct Authenticated {
    pub(crate) key: Arc<KeyRt>,
    pub(crate) controls: Controls,
    /// The end user named by `x-antirion-end-user`.
    pub(crate) end_user: Option<String>,
    /// The `aud` values of the JWT the caller presented; `None` for a virtual key.
    pub(crate) token_audiences: Option<Vec<String>>,
    /// The claims of the JWT the caller presented; `None` for a virtual key.
    pub(crate) claims: Option<serde_json::Map<String, serde_json::Value>>,
}

pub(crate) fn unauthenticated(reason: antirion_error::Reason, message: &'static str) -> GatewayError {
    let challenge = if reason == reasons::MISSING_API_KEY {
        "Bearer realm=\"antirion\""
    } else {
        "Bearer realm=\"antirion\", error=\"invalid_token\""
    };
    GatewayError::with_reason(reason, Stage::Authenticate, Message::Fixed(message))
        .header(WWW_AUTHENTICATE, HeaderValue::from_static(challenge))
}

fn invalid_key() -> GatewayError {
    unauthenticated(reasons::INVALID_API_KEY, "The API key is not valid.")
}

/// While the identity store is unreachable, only credentials verified within the outage grace
/// authenticate: the snapshot cannot tell a revoked or newly created key from an unknown one.
fn store_unavailable() -> GatewayError {
    GatewayError::with_reason(
        reasons::IDENTITY_STORE_UNAVAILABLE,
        Stage::Authenticate,
        Message::Fixed("The identity store is unavailable; retry shortly."),
    )
    .header(RETRY_AFTER, HeaderValue::from_static("5"))
}

fn forbidden(reason: antirion_error::Reason, message: &'static str) -> GatewayError {
    GatewayError::with_reason(reason, Stage::Authenticate, Message::Fixed(message))
}

/// The subprotocol item that carries a key on a WebSocket upgrade.
pub(crate) const SUBPROTOCOL_KEY: &str = "openai-insecure-api-key.";

/// A credential outside the credential headers: the `key` query parameter on Gemini-native routes,
/// the `openai-insecure-api-key.<key>` item of `Sec-WebSocket-Protocol` (its items may be spread
/// over repeated headers) on the Realtime endpoint.
fn extra_key<'a>(parts: &'a Parts, endpoint: &Endpoint) -> Option<&'a str> {
    match endpoint.family {
        "gemini" => gemini_query_key(parts),
        _ if endpoint.kind == antirion_dialects::Kind::ResponsesSocket => subprotocol_key(parts),
        "realtime" => parts
            .headers
            .get_all(http::header::SEC_WEBSOCKET_PROTOCOL)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .find_map(|item| item.trim().strip_prefix(SUBPROTOCOL_KEY)),
        _ => None,
    }
}

/// The credential item of `Sec-WebSocket-Protocol`.
pub(crate) fn subprotocol_key(parts: &Parts) -> Option<&str> {
    parts
        .headers
        .get_all(http::header::SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .find_map(|item| item.trim().strip_prefix(SUBPROTOCOL_KEY))
}

fn gemini_query_key(parts: &Parts) -> Option<&str> {
    parts
        .uri
        .query()?
        .split('&')
        .find_map(|pair| pair.strip_prefix("key="))
}

/// What a valid credential establishes.
pub(crate) struct Credential {
    pub(crate) key: Arc<KeyRt>,
    /// The end user a JWT's `end_user_claim` names.
    pub(crate) end_user: Option<String>,
    /// The JWT's `aud` values.
    pub(crate) audiences: Option<Vec<String>>,
    /// The JWT's claims.
    pub(crate) claims: Option<serde_json::Map<String, serde_json::Value>>,
}

impl Credential {
    fn key(key: &Arc<KeyRt>) -> Self {
        Credential {
            key: Arc::clone(key),
            end_user: None,
            audiences: None,
            claims: None,
        }
    }
}

/// Recognises imported legacy keys, once any exists.
fn imported(identity: &IdentitySnapshot) -> Option<impl Fn(&str) -> bool + '_> {
    (!identity.imported.is_empty()).then_some(|d: &str| identity.imported.contains_key(d))
}

/// A key unknown to this node's generation may have been created on another node a moment ago:
/// in cluster mode the node reads the store's generation once before the key is checked.
pub(crate) async fn catch_up_for_unknown_key(cx: &mut Ctx, parts: &Parts, endpoint: &Endpoint) {
    if cx.rt.cluster.is_none() || cx.rt.identity_store_down() || cx.snap.identity.anonymous.is_some() {
        return;
    }
    let query_key = extra_key(parts, endpoint);
    let Ok(Presented::Key(cred)) = select(&parts.headers, query_key, None) else {
        return;
    };
    if cx.snap.identity.keys_by_kid.contains_key(cred.kid()) {
        return;
    }
    cx.rt.catch_up_after(cx.started).await;
    cx.snap = cx.rt.gens.load();
}

/// Checks 1–3: the credential, its verifier and its expiry.
async fn credential(cx: &Ctx, parts: &Parts, endpoint: &Endpoint) -> Result<Credential, GatewayError> {
    if let Some(anonymous) = &cx.snap.identity.anonymous {
        return Ok(Credential::key(anonymous));
    }
    let query_key = extra_key(parts, endpoint);
    let known = imported(&cx.snap.identity);
    let cred = match select(
        &parts.headers,
        query_key,
        known.as_ref().map(|f| f as &dyn Fn(&str) -> bool),
    ) {
        Ok(Presented::Key(c)) => c,
        Ok(Presented::Jwt(token)) => return crate::jwt::authenticate(cx, &token).await,
        Ok(Presented::Imported(digest)) => return legacy(cx, &digest),
        Err(Refusal::Missing) => {
            return Err(unauthenticated(
                reasons::MISSING_API_KEY,
                "No API key was provided.",
            ));
        }
        Err(Refusal::Invalid) => return Err(invalid_key()),
        Err(Refusal::Ambiguous) => {
            return Err(unauthenticated(
                reasons::AMBIGUOUS_CREDENTIALS,
                "More than one different credential was provided.",
            ));
        }
    };
    let pepper = &cx.rt.pepper;
    let now = cx.rt.now();
    let outage = cx.rt.identity_store_down();
    let Some(secret) = cx.snap.identity.keys_by_kid.get(cred.kid()) else {
        let _ = pepper.verifier(cred.expose());
        return Err(if outage {
            store_unavailable()
        } else {
            invalid_key()
        });
    };
    let verified =
        pepper.verify(cred.expose(), &secret.verifier) && secret.valid_until.is_none_or(|until| now < until);
    if outage {
        let grace = cx.snap.identity.settings.store_outage_grace;
        if !(verified && cx.rt.state.verified.within(cred.kid(), now, grace)) {
            return Err(store_unavailable());
        }
    } else if verified {
        cx.rt.state.verified.record(cred.kid(), now);
    } else {
        return Err(invalid_key());
    }
    expiry(&secret.key, now)?;
    Ok(Credential::key(&secret.key))
}

pub(crate) fn expiry(key: &KeyRt, now: antirion_core::UtcMicros) -> Result<(), GatewayError> {
    if key.expires_at.is_some_and(|exp| now >= exp) {
        return Err(unauthenticated(
            reasons::EXPIRED_API_KEY,
            "The API key has expired.",
        ));
    }
    Ok(())
}

/// An imported legacy key, found by the SHA-256 of the presented secret. A rotated-out one keeps
/// working until its grace ends.
fn legacy(cx: &Ctx, digest: &str) -> Result<Credential, GatewayError> {
    let now = cx.rt.now();
    let secret = cx
        .snap
        .identity
        .imported
        .get(digest)
        .filter(|s| s.valid_until.is_none_or(|until| now < until))
        .ok_or_else(invalid_key)?;
    let id = format!("sha256:{digest}");
    if cx.rt.identity_store_down() {
        let grace = cx.snap.identity.settings.store_outage_grace;
        if !cx.rt.state.verified.within(&id, now, grace) {
            return Err(store_unavailable());
        }
    } else {
        cx.rt.state.verified.record(&id, now);
    }
    expiry(&secret.key, now)?;
    Ok(Credential::key(&secret.key))
}

/// A failed authentication counts against the client address's throttle; past it, the failure is
/// answered as rate limited with the time until the window frees a place.
fn throttled(cx: &Ctx, e: GatewayError) -> GatewayError {
    if e.code != antirion_error::Code::Unauthenticated {
        return e;
    }
    let Some(ip) = cx.client_ip else {
        return e;
    };
    let settings = &cx.snap.identity.access.throttle;
    let Some(wait) = cx.rt.state.access.failed_auth.fail(ip, cx.rt.now(), settings) else {
        return e;
    };
    let mut refused = GatewayError::with_reason(
        reasons::AUTH_THROTTLED,
        Stage::Authenticate,
        Message::Fixed("Too many failed authentications from this address; retry later."),
    );
    refused.retry_after = Some(wait);
    refused
}

/// Checks 4–6: the key, its ancestors and its owner are enabled.
fn enabled(identity: &IdentitySnapshot, key: &KeyRt) -> Result<(), GatewayError> {
    if !key.enabled {
        return Err(forbidden(reasons::KEY_DISABLED, "The API key is disabled."));
    }
    let [org, team, project] = identity.chain(key);
    if org.is_some_and(|t| !t.enabled) {
        return Err(forbidden(
            reasons::ORGANIZATION_DISABLED,
            "The key's organization is disabled.",
        ));
    }
    if team.is_some_and(|t| !t.enabled) {
        return Err(forbidden(reasons::TEAM_DISABLED, "The key's team is disabled."));
    }
    if project.is_some_and(|t| !t.enabled) {
        return Err(forbidden(
            reasons::PROJECT_DISABLED,
            "The key's project is disabled.",
        ));
    }
    if key
        .owner
        .as_ref()
        .and_then(|o| identity.users.get(o))
        .is_some_and(|u| !u.enabled)
    {
        return Err(forbidden(reasons::USER_DISABLED, "The key's owner is disabled."));
    }
    Ok(())
}

/// Whether every endpoint scope defined over `key` allows `family`.
pub(crate) fn family_allowed(identity: &IdentitySnapshot, key: &KeyRt, family: &str) -> bool {
    identity.access_levels(key).iter().all(|a| {
        a.endpoints
            .as_ref()
            .is_none_or(|e| e.iter().any(|f| f == "*" || f == family))
    })
}

/// Checks 7 and 8: the client IP is in every defined allow-list and the endpoint family in every
/// defined scope.
fn scopes(
    identity: &IdentitySnapshot,
    key: &KeyRt,
    ip: Option<IpAddr>,
    endpoint: &Endpoint,
) -> Result<(), GatewayError> {
    let levels = identity.access_levels(key);
    let ip_ok = levels.iter().all(|a| {
        a.allowed_ips
            .as_ref()
            .is_none_or(|list| ip.is_some_and(|ip| list.iter().any(|c| c.contains(ip))))
    });
    if !ip_ok {
        let seen = ip.map_or_else(|| "unknown".to_owned(), |ip| ip.to_string());
        return Err(GatewayError::with_reason(
            reasons::IP_NOT_ALLOWED,
            Stage::Authenticate,
            Message::built(format!("The client IP {seen} may not use this API key.")),
        ));
    }
    let endpoint_ok =
        endpoint.kind == antirion_dialects::Kind::Feedback || family_allowed(identity, key, endpoint.family);
    if !endpoint_ok {
        return Err(forbidden(
            reasons::ENDPOINT_NOT_ALLOWED,
            "The API key may not use this endpoint.",
        ));
    }
    Ok(())
}

pub(crate) async fn run(cx: &Ctx, parts: &Parts, endpoint: &Endpoint) -> Result<Authenticated, GatewayError> {
    let Credential {
        key,
        end_user: claimed_user,
        audiences: token_audiences,
        claims,
    } = credential(cx, parts, endpoint)
        .await
        .and_then(|c| {
            if crate::certbind::satisfied(cx, parts, &c.key) {
                Ok(c)
            } else {
                Err(invalid_key())
            }
        })
        .map_err(|e| throttled(cx, e))?;
    let identity = &cx.snap.identity;
    enabled(identity, &key)?;
    if let Some(wait) = cx
        .rt
        .state
        .operations
        .pauses
        .refusal(cx.started_at, Some(&key.organization))
    {
        return Err(crate::accept::paused(Stage::Authenticate, wait));
    }
    scopes(identity, &key, cx.client_ip, endpoint)?;
    let controls = controls::check(cx, &parts.headers, &key)?;
    let end_user = match parts.headers.get(END_USER) {
        None => claimed_user,
        Some(v) => Some(enduser::from_header(v.as_bytes()).ok_or_else(|| {
            controls::invalid_header(
                END_USER,
                "x-antirion-end-user must be 1 to 256 visible ASCII characters.",
            )
        })?),
    };
    let now = cx.rt.now();
    cx.rt.state.key_usage.record(key.uid, now);
    if identity.anomaly.enabled {
        let ua = parts
            .headers
            .get(http::header::USER_AGENT)
            .map(http::HeaderValue::as_bytes);
        cx.rt
            .state
            .anomalies
            .observe(key.uid, &key.name, now, cx.client_ip, ua);
    }
    Ok(Authenticated {
        key,
        controls,
        end_user,
        token_audiences,
        claims,
    })
}
