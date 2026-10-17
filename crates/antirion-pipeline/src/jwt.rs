//! Authentication with a JWT bearer token: the issuer's provider, its key set, the signature and
//! claim checks, then the mapping to a `kind: jwt` key.

use std::sync::Arc;

use antirion_core::Stage;
use antirion_error::{GatewayError, Message};
use antirion_identity::jwt::{self, Failure};
use antirion_identity::reasons;
use antirion_runtime::jwt::FetchCx;
use http::HeaderValue;
use serde_json::Value;

use crate::auth::{Credential, unauthenticated};
use crate::ctx::Ctx;

fn invalid(f: Failure) -> GatewayError {
    unauthenticated(reasons::INVALID_TOKEN, f.message())
}

fn unavailable() -> GatewayError {
    GatewayError::with_reason(
        reasons::JWKS_UNAVAILABLE,
        Stage::Authenticate,
        Message::Fixed("The token issuer's signing keys are not available."),
    )
    .header(http::header::RETRY_AFTER, HeaderValue::from_static("5"))
}

/// The key a valid token maps to, the end user its `end_user_claim` names and its audiences.
pub(crate) async fn authenticate(cx: &Ctx, token: &str) -> Result<Credential, GatewayError> {
    let t = jwt::decode(token).map_err(invalid)?;
    let iss = t.claims.get("iss").and_then(Value::as_str);
    let provider = cx
        .snap
        .identity
        .jwt
        .iter()
        .find(|p| Some(p.rules.issuer.as_str()) == iss)
        .ok_or_else(|| invalid(Failure::Issuer))?;
    jwt::check_alg(&t, &provider.rules).map_err(invalid)?;
    let keys = if provider.rules.hmac_secret.is_some() {
        Arc::new(Vec::new())
    } else {
        let tls = cx.snap.security.egress_tls.clone().ok_or_else(unavailable)?;
        let fetch = FetchCx {
            client: &cx.rt.egress,
            policy: &cx.snap.security.egress,
            tls: &tls,
            now: cx.rt.now(),
            secure: cx.rt.test.is_none() && !provider.allow_insecure_jwks,
        };
        cx.rt
            .jwks
            .keys(provider, t.kid.as_deref(), &fetch)
            .await
            .map_err(|_| unavailable())?
    };
    jwt::verify_signature(&t, &provider.rules, &keys).map_err(invalid)?;
    jwt::verify_claims(&t, &provider.rules, cx.rt.now().0 / 1_000_000).map_err(invalid)?;
    let name = provider
        .mappings
        .iter()
        .find(|(want, _)| jwt::matches(&t.claims, want))
        .map(|(_, key)| key)
        .ok_or_else(|| {
            GatewayError::with_reason(
                reasons::JWT_NO_MAPPING,
                Stage::Authenticate,
                Message::Fixed("No mapping of the token's issuer matches its claims."),
            )
        })?;
    let key = cx
        .snap
        .identity
        .jwt_keys
        .get(name)
        .cloned()
        .ok_or_else(|| invalid(Failure::Issuer))?;
    crate::auth::expiry(&key, cx.rt.now())?;
    let end_user = provider
        .end_user_claim
        .as_deref()
        .and_then(|c| jwt::claim(&t.claims, c))
        .and_then(Value::as_str)
        .and_then(antirion_identity::enduser::from_body);
    let audiences = match t.claims.get("aud") {
        Some(Value::String(a)) => vec![a.clone()],
        Some(Value::Array(list)) => list.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
        _ => Vec::new(),
    };
    Ok(Credential {
        key,
        end_user,
        audiences: Some(audiences),
        claims: Some(t.claims),
    })
}
