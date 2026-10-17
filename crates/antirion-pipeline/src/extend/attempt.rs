//! `attempt` hooks: before each upstream send, retries and fallbacks included, with the upstream
//! request in the deployment's dialect. They may patch its JSON body and set upstream headers,
//! under the request hook's restrictions; they never change where it goes.

use antirion_egress::Payload;
use antirion_error::GatewayError;
use antirion_ext::api::{AttemptTarget, Input};
use antirion_ext::{Hook, HookAction};
use antirion_providers::DeploymentRt;
use bytes::Bytes;
use http::{HeaderName, HeaderValue};
use serde_json::Value;

use super::{Hooks, header_pairs, merge_metadata, patch_failed, run_one};
use crate::ctx::Ctx;

/// What the attempt hooks made of one attempt's request.
pub(crate) struct Edited {
    pub(crate) body: Payload,
    pub(crate) headers: Vec<(HeaderName, HeaderValue)>,
}

/// Whether any extension of the request has an `attempt` hook.
pub(crate) fn any(hooks: &Hooks) -> bool {
    hooks.list.iter().any(|p| p.runs(Hook::Attempt))
}

/// The upstream body as JSON, when it is a JSON body held whole.
fn json_of(body: &Payload, content_type: Option<&HeaderValue>) -> Option<Value> {
    let json = content_type
        .and_then(|v| v.to_str().ok())
        .is_none_or(|t| t.starts_with("application/json"));
    if !json || body.tail().is_some() {
        return None;
    }
    serde_json::from_slice(&body.contiguous()).ok()
}

/// Runs the request's `attempt` hooks, in request-hook order, for attempt `index` on `d`.
pub(crate) async fn run(
    cx: &Ctx,
    hooks: &Hooks,
    index: u32,
    d: &DeploymentRt,
    body: Payload,
    content_type: Option<&HeaderValue>,
) -> Result<Edited, GatewayError> {
    let target = AttemptTarget::new(
        index,
        &d.name,
        &d.provider.name,
        d.provider.kind.as_str(),
        &d.model,
    );
    let mut json = json_of(&body, content_type);
    let mut changed = false;
    let mut headers = Vec::new();
    for p in hooks.list.iter().filter(|p| p.runs(Hook::Attempt)) {
        let input = Input::new(Hook::Attempt, &p.name, &p.config, &hooks.facts)
            .with_body(json.as_ref())
            .with_attempt(&target);
        let Some(action) = run_one(cx, p, &input).await? else {
            continue;
        };
        if let HookAction::Mutate {
            patch,
            set_headers,
            metadata,
        } = action
        {
            if !patch.is_empty() {
                match json.as_ref().map(|b| antirion_ext::patch::apply(b, &patch)) {
                    Some(Ok(patched)) => {
                        json = Some(patched);
                        changed = true;
                    }
                    _ => {
                        patch_failed(cx, p, Hook::Attempt)?;
                        continue;
                    }
                }
            }
            headers.extend(header_pairs(set_headers));
            merge_metadata(cx, metadata);
        }
    }
    let body = match json.filter(|_| changed) {
        Some(v) => Payload::from_parts(vec![Bytes::from(serde_json::to_vec(&v).unwrap_or_default())]),
        None => body,
    };
    Ok(Edited { body, headers })
}
