//! Access decisions made once the model is known, after the key's own lists: a key's required end
//! user, `auth.access_rules`, and the external authorization hook, which can only restrict.

use std::time::Duration;

use antirion_core::Stage;
use antirion_error::{GatewayError, Message};
use antirion_identity::reasons;
use antirion_runtime::access::{AuthzKey, ExternalAuthz, RuleFacts, Verdict, denies};
use http::request::Parts;
use serde_json::{Map, Value, json};

use crate::auth::Authenticated;
use crate::ctx::Ctx;

/// The largest answer read from the hook.
const MAX_ANSWER: u64 = 64 * 1024;
/// The longest deny reason repeated to the client.
const MAX_REASON: usize = 200;

/// A key with `end_user: required` refuses a request that names no end user.
pub(crate) fn require_end_user(auth: &Authenticated, end_user: Option<&str>) -> Result<(), GatewayError> {
    if auth.key.end_user_required && end_user.is_none() {
        return Err(GatewayError::with_reason(
            reasons::MISSING_END_USER,
            Stage::Parse,
            Message::Fixed(
                "This API key requires an end user: send x-antirion-end-user or the body's user field.",
            ),
        )
        .param("user"));
    }
    Ok(())
}

/// What the access decisions read about a request.
pub(crate) struct Subject<'a> {
    pub(crate) parts: &'a Parts,
    pub(crate) auth: &'a Authenticated,
    pub(crate) end_user: Option<&'a str>,
    pub(crate) requested: &'a str,
    pub(crate) model: &'a str,
    /// The request's effective tags.
    pub(crate) tags: &'a [String],
}

/// The tags configured on the key and its tenancy levels.
fn key_tags(cx: &Ctx, s: &Subject<'_>) -> Vec<String> {
    let key = &s.auth.key;
    cx.snap
        .identity
        .chain(key)
        .iter()
        .flatten()
        .flat_map(|t| t.tags.iter().cloned())
        .chain(key.tags.iter().cloned())
        .collect()
}

/// `auth.access_rules`, then the external authorization hook.
pub(crate) async fn check(cx: &Ctx, s: &Subject<'_>) -> Result<(), GatewayError> {
    let settings = &cx.snap.identity.access;
    if !settings.rules.is_empty() {
        let tags = key_tags(cx, s);
        let facts = RuleFacts {
            requested: s.requested,
            model: s.model,
            claims: s.auth.claims.as_ref(),
            key_tags: &tags,
            headers: &s.parts.headers,
        };
        if denies(&settings.rules, &facts) {
            return Err(GatewayError::with_reason(
                reasons::ACCESS_RULE_DENIED,
                Stage::Resolve,
                Message::Fixed("An access rule for this model is not satisfied."),
            ));
        }
    }
    match &settings.external {
        Some(hook) => external(cx, s, hook).await,
        None => Ok(()),
    }
}

fn unavailable() -> GatewayError {
    let mut e = GatewayError::with_reason(
        reasons::EXTERNAL_AUTHZ_UNAVAILABLE,
        Stage::Resolve,
        Message::Fixed("The authorization service did not answer."),
    );
    e.retry_after = Some(Duration::from_secs(1));
    e
}

fn denied(reason: &str) -> GatewayError {
    let reason: String = reason
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_REASON)
        .collect();
    let message = if reason.is_empty() {
        "The authorization service denied this request.".to_owned()
    } else {
        format!("The authorization service denied this request: {reason}")
    };
    GatewayError::with_reason(
        reasons::EXTERNAL_AUTHZ_DENIED,
        Stage::Resolve,
        Message::built(message),
    )
}

/// The hook's request body: the caller, the request and the headers it may see, never the
/// client's credentials.
fn body(cx: &Ctx, s: &Subject<'_>, hook: &ExternalAuthz) -> Value {
    let key = &s.auth.key;
    let mut headers = Map::new();
    for name in &hook.send_headers {
        if let Some(v) = s.parts.headers.get(name.as_str()).and_then(|v| v.to_str().ok()) {
            headers.insert(name.clone(), Value::from(v));
        }
    }
    json!({
        "request_id": cx.request_id.as_str(),
        "organization": key.organization,
        "team": key.team,
        "project": key.project,
        "key": key.name,
        "end_user": s.end_user,
        "client_ip": cx.client_ip.map(|ip| ip.to_string()),
        "method": s.parts.method.as_str(),
        "path": s.parts.uri.path(),
        "model": s.model,
        "tags": s.tags,
        "claims": s.auth.claims.clone().map(Value::Object),
        "headers": headers,
    })
}

/// Reads the hook's answer; `None` for anything but `200 {"allow": bool, …}`.
fn verdict(status: u16, answer: &[u8]) -> Option<Verdict> {
    if status != 200 {
        return None;
    }
    let v: Value = serde_json::from_slice(answer).ok()?;
    if v.get("allow")?.as_bool()? {
        return Some(Verdict::Allow);
    }
    Some(Verdict::Deny(
        v.get("reason")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    ))
}

async fn external(cx: &Ctx, s: &Subject<'_>, hook: &ExternalAuthz) -> Result<(), GatewayError> {
    let cache_key = AuthzKey {
        url: hook.url.clone(),
        key: s.auth.key.uid,
        method: s.parts.method.as_str().to_owned(),
        path: s.parts.uri.path().to_owned(),
        model: s.model.to_owned(),
    };
    let cache = &cx.rt.state.access.authz;
    let found = match cache.get(&cache_key, cx.rt.now()) {
        Some(v) => Some(v),
        None => {
            let post = crate::external::Post {
                url: &hook.url,
                headers: http::HeaderMap::new(),
                body: serde_json::to_vec(&body(cx, s, hook)).unwrap_or_default(),
                timeout: hook.timeout,
                max: MAX_ANSWER,
            };
            let call = crate::external::post_json(&cx.rt, &cx.snap, &cx.request_id, post);
            let answered = match tokio::time::timeout(hook.timeout, call).await {
                Ok(Ok((status, answer))) => verdict(status, &answer),
                Ok(Err(e)) => {
                    tracing::warn!(error = %e.text(), "external authorization call failed");
                    None
                }
                Err(_) => None,
            };
            if let Some(v) = &answered {
                cache.put(cache_key, v.clone(), hook, cx.rt.now());
            }
            answered
        }
    };
    match found {
        Some(Verdict::Allow) => Ok(()),
        Some(Verdict::Deny(reason)) => Err(denied(&reason)),
        None if hook.fail_open => Ok(()),
        None => Err(unavailable()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers() {
        assert_eq!(verdict(200, br#"{"allow": true}"#), Some(Verdict::Allow));
        assert_eq!(
            verdict(200, br#"{"allow": false, "reason": "no"}"#),
            Some(Verdict::Deny("no".into()))
        );
        assert_eq!(verdict(500, br#"{"allow": true}"#), None);
        assert_eq!(verdict(200, b"allow"), None);
        assert_eq!(verdict(200, br#"{"allow": "yes"}"#), None);
        let e = denied("bad\nactor");
        assert_eq!(e.reason, Some(reasons::EXTERNAL_AUTHZ_DENIED));
    }
}
