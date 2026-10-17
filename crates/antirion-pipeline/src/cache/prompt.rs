//! Provider prompt-cache breakpoints on each attempt to a deployment that accepts them, and the
//! per-request switch.

use antirion_cache::PromptCaching;
use antirion_cache::policy::CachePolicy;
use antirion_core::Stage;
use antirion_egress::Payload;
use antirion_error::reason::hygiene;
use antirion_error::{GatewayError, Message};
use antirion_providers::DeploymentRt;
use http::HeaderMap;
use serde_json::Value;

use crate::attempt::Plan;
use crate::parse::Parsed;

/// The request header that turns injection on or off for one request.
pub(crate) const SWITCH: &str = "x-antirion-prompt-cache";

/// The breakpoints the request's attempts get: the effective `prompt_caching` block when injection
/// is on for it, `on` and `off` overriding `auto_inject`.
pub(crate) fn switch(headers: &HeaderMap, policy: &Value) -> Result<Option<PromptCaching>, GatewayError> {
    let on = match headers.get(SWITCH).map(|v| v.to_str().unwrap_or_default()) {
        None => None,
        Some("on") => Some(true),
        Some("off") => Some(false),
        Some(_) => {
            return Err(GatewayError::with_reason(
                hygiene::INVALID_HEADER,
                Stage::Parse,
                Message::Fixed("x-antirion-prompt-cache must be on or off."),
            )
            .param(SWITCH));
        }
    };
    let Some(pc) = CachePolicy::prompt_in(policy) else {
        return Ok(None);
    };
    Ok(on.unwrap_or(pc.auto_inject).then_some(pc))
}

/// The upstream body of an attempt with breakpoints placed, when the deployment accepts cache
/// markers and the client placed none.
pub(crate) fn breakpoints(plan: &Plan<'_>, d: &DeploymentRt, body: Payload) -> Payload {
    let Some(pc) = &plan.prompt_cache else {
        return body;
    };
    if !d.capabilities.flag("prompt_cache_markers") {
        return body;
    }
    let Parsed::Json { .. } = plan.parsed else {
        return body;
    };
    let caller_marked = plan
        .parsed
        .doc()
        .is_none_or(antirion_cache::prompt::has_caller_marker);
    if caller_marked {
        return body;
    }
    let Ok(mut v) = serde_json::from_slice::<Value>(&body.contiguous()) else {
        return body;
    };
    if v.get("messages").and_then(Value::as_array).is_none() {
        return body;
    }
    let estimate = |chars: usize| u64::try_from(chars.div_ceil(4)).unwrap_or(u64::MAX);
    if antirion_cache::prompt::inject(&mut v, pc, estimate) == 0 {
        return body;
    }
    serde_json::to_vec(&v).map_or(body, Payload::from)
}

/// Records the request's prompt prefix (system and developer text, tools and first user message)
/// when it uses provider prompt caching: the policy has a `prompt_caching` block or the client placed
/// a marker. Analytics group by it.
pub(crate) fn note_prefix(cx: &crate::ctx::Ctx, policy: &Value, client: Option<&Value>) {
    let Some(body) = client else {
        return;
    };
    if CachePolicy::prompt_in(policy).is_none() && !antirion_cache::prompt::has_caller_marker(body) {
        return;
    }
    if let Some(prefix) = antirion_router::prefix_key::value(body) {
        cx.log.update(|r| r.cache.prefix = Some(prefix));
    }
}

/// The `prompt_cache_key` of a request whose policy asks for one and whose client sent none: the
/// first 32 hex digits of SHA-256 over the project and the route.
pub(crate) fn openai_key(
    policy: &Value,
    client: Option<&Value>,
    project: &str,
    route: &str,
) -> Option<String> {
    if !CachePolicy::prompt_in(policy).is_some_and(|pc| pc.openai_cache_key) {
        return None;
    }
    if client.is_some_and(|d| d.get("prompt_cache_key").is_some()) {
        return None;
    }
    let mut digest = antirion_cache::key::sha256_hex(format!("{project}\n{route}").as_bytes());
    digest.truncate(32);
    Some(digest)
}

/// The upstream body of an attempt to an OpenAI deployment with the request's `prompt_cache_key`.
pub(crate) fn openai_cache_key(plan: &Plan<'_>, d: &DeploymentRt, body: Payload) -> Payload {
    let Some(value) = &plan.openai_cache_key else {
        return body;
    };
    if d.provider.kind != antirion_providers::ProviderType::OpenAi {
        return body;
    }
    let Ok(Value::Object(mut v)) = serde_json::from_slice::<Value>(&body.contiguous()) else {
        return body;
    };
    if v.contains_key("prompt_cache_key") {
        return body;
    }
    v.insert("prompt_cache_key".to_owned(), Value::from(value.as_str()));
    serde_json::to_vec(&v).map_or(body, Payload::from)
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;
    use serde_json::json;

    use super::*;

    #[test]
    fn the_openai_key_is_of_project_and_route() {
        let policy = json!({"prompt_caching": {"openai_cache_key": true}});
        let k = openai_key(&policy, Some(&json!({"model": "m"})), "p", "r").unwrap();
        assert_eq!(k.len(), 32);
        assert!(antirion_cache::key::sha256_hex(b"p\nr").starts_with(&k));
        assert!(openai_key(&policy, Some(&json!({"prompt_cache_key": "x"})), "p", "r").is_none());
        assert!(openai_key(&json!({"prompt_caching": {}}), Some(&json!({})), "p", "r").is_none());
    }

    #[test]
    fn the_switch_needs_a_block() {
        let mut h = HeaderMap::new();
        let block = json!({"prompt_caching": {"auto_inject": false}});
        assert!(switch(&h, &block).unwrap().is_none());
        h.insert(SWITCH, HeaderValue::from_static("on"));
        assert!(switch(&h, &block).unwrap().is_some());
        assert!(switch(&h, &json!({})).unwrap().is_none());
        h.insert(SWITCH, HeaderValue::from_static("off"));
        assert!(
            switch(&h, &json!({"prompt_caching": {"auto_inject": true}}))
                .unwrap()
                .is_none()
        );
        h.insert(SWITCH, HeaderValue::from_static("yes"));
        assert!(switch(&h, &block).is_err());
    }
}
