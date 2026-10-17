//! What crosses the egress boundary with a request: the client headers the policy forwards, the
//! headers the gateway owns, and the upstream response headers a client may see.

use antirion_core::headers as h;
use antirion_egress::headers::{self, HeaderRules, Outbound};
use antirion_runtime::header_rules::Levels;
use antirion_runtime::identity::KeyRt;
use http::header::CONTENT_TYPE;
use http::{HeaderMap, HeaderName, HeaderValue};

use crate::ctx::Ctx;

/// The header rules in effect for a request by `key` to `model` (through `route`, if any). A request
/// addressing a deployment directly has no route and names the deployment as its model.
pub(crate) fn rules(cx: &Ctx, key: &KeyRt, route: Option<&str>, model: &str) -> HeaderRules {
    cx.snap.security.headers.rules(&Levels {
        organization: &key.organization,
        team: &key.team,
        project: &key.project,
        key: &key.name,
        route,
        model,
    })
}

/// Completes an upstream request's headers from the client's and the gateway's own.
pub(crate) fn outbound(
    cx: &Ctx,
    client: &HeaderMap,
    rules: &HeaderRules,
    stream: bool,
    upstream: &mut HeaderMap,
) {
    let user_agent = HeaderValue::from_str(&format!("antirion/{}", cx.rt.node.version))
        .unwrap_or_else(|_| HeaderValue::from_static("antirion"));
    headers::outbound(
        upstream,
        client,
        &Outbound {
            rules,
            user_agent: &user_agent,
            stream,
        },
    );
}

/// Copies the upstream response headers a client may see, and maps the upstream's request id. The
/// content type is the caller's: a translated or streamed body has its own.
pub(crate) fn inbound(
    upstream: &HeaderMap,
    rules: &HeaderRules,
    client: &mut HeaderMap,
    scrub: impl Fn() -> crate::scrub::Scrub,
) {
    let mut built = None;
    for (name, value) in &headers::inbound(upstream, rules) {
        if name == CONTENT_TYPE {
            continue;
        }
        let value = match value.to_str() {
            Ok(text) => HeaderValue::from_str(&built.get_or_insert_with(&scrub).apply(text))
                .unwrap_or_else(|_| value.clone()),
            Err(_) => value.clone(),
        };
        client.append(name.clone(), value);
    }
    if let Some(id) = headers::upstream_request_id(upstream) {
        client.insert(HeaderName::from_static(h::UPSTREAM_REQUEST_ID), id.clone());
    }
}
