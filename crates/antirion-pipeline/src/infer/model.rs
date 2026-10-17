//! The model a request names, its default, and what that name resolves to for routing.

use std::sync::Arc;

use antirion_core::Stage;
use antirion_dialects::{Dialect, Endpoint, ModelField};
use antirion_error::reason::hygiene;
use antirion_error::{Code, GatewayError, Message};
use antirion_identity::enduser;
use antirion_providers::DeploymentRt;
use antirion_router::{Captures, Params, RouteRt};
use antirion_runtime::Snapshot;
use serde_json::Value;

use crate::authorize::{self, Target};
use crate::ctx::Ctx;
use crate::parse::Parsed;
use crate::reasons;

use super::facts::dialect_name;

/// The end user named by the body: OpenAI `user` or Anthropic `metadata.user_id`.
pub(super) fn body_end_user(parsed: &Parsed) -> Option<String> {
    if let Some(user) = parsed.member("user") {
        return enduser::from_body(user.as_str()?);
    }
    let metadata = parsed.member("metadata")?;
    enduser::from_body(metadata.get("user_id")?.as_str()?)
}

pub(super) fn missing_model() -> GatewayError {
    GatewayError::with_reason(
        hygiene::MISSING_MODEL,
        Stage::Resolve,
        Message::Fixed("A model is required."),
    )
    .param("model")
    .openai_code("missing_required_parameter")
}

/// The Model named by `routing.default_models` for this endpoint.
pub(super) fn default_model(cx: &Ctx, endpoint: &Endpoint) -> Option<String> {
    cx.snap
        .config
        .doc
        .get("routing")
        .and_then(|r| r.get("default_models"))
        .and_then(|d| d.get(endpoint.id))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// The model the request names, and whether the body had no `model` (a default was used).
pub(super) fn requested_model(
    cx: &Ctx,
    endpoint: &Endpoint,
    parsed: &Parsed,
) -> Result<(String, bool), GatewayError> {
    let native = matches!(
        endpoint.dialect,
        Dialect::Gemini | Dialect::BedrockConverse | Dialect::BedrockInvoke
    );
    let legacy = endpoint.kind == antirion_dialects::Kind::Inference
        && matches!(
            endpoint.dialect,
            Dialect::OpenAiChat | Dialect::OpenAiCompletions | Dialect::OpenAiEmbeddings
        );
    let path_model = endpoint.path_param.clone().filter(|_| native || legacy);
    match (path_model, parsed.model()) {
        (Some(m), ModelField::Missing) if legacy => Ok((m, true)),
        (Some(m), _) => Ok((m, false)),
        (_, ModelField::Name(m)) if m.trim().is_empty() => Err(GatewayError::with_reason(
            reasons::INVALID_VALUE,
            Stage::Parse,
            Message::Fixed("model must not be blank."),
        )
        .param("model")
        .openai_code("invalid_value")),
        (_, ModelField::Name(m)) => Ok((m.clone(), false)),
        (_, ModelField::Missing) => default_model(cx, endpoint)
            .map(|m| (m, true))
            .ok_or_else(missing_model),
        (_, ModelField::WrongType) => Err(GatewayError::with_reason(
            reasons::INVALID_TYPE,
            Stage::Parse,
            Message::Fixed("model must be a string."),
        )
        .param("model")
        .openai_code("invalid_type")),
    }
}

/// The error when no deployment the key may use serves the endpoint's dialect.
pub(super) fn unserved(endpoint: &Endpoint, requested: &str) -> GatewayError {
    let openai_ingress = !matches!(
        endpoint.dialect,
        Dialect::AnthropicMessages | Dialect::Gemini | Dialect::BedrockConverse | Dialect::BedrockInvoke
    );
    if openai_ingress && endpoint.id != "responses" {
        return GatewayError::with_reason(
            reasons::CAP_DIALECTS,
            Stage::Resolve,
            Message::built(format!(
                "No deployment of model `{requested}` serves the {} API.",
                dialect_name(endpoint)
            )),
        )
        .param("model");
    }
    GatewayError::with_reason(
        hygiene::TRANSLATION_UNSUPPORTED,
        Stage::Resolve,
        Message::built(format!(
            "No deployment of model `{}` serves the {} API: this build forwards {} requests only to a deployment of the same dialect.",
            requested,
            dialect_name(endpoint),
            dialect_name(endpoint),
        )),
    )
}

/// What the requested name resolved to, for routing.
pub(crate) struct Resolution {
    pub(crate) model_name: String,
    /// The named route whose policies apply (none for an inline target).
    pub(crate) policy_route: Option<String>,
    pub(crate) route: Arc<RouteRt>,
    pub(crate) alias: Option<Params>,
    pub(crate) captures: Option<Captures>,
    pub(crate) known: Vec<Arc<DeploymentRt>>,
}

pub(crate) fn resolution(
    snap: &Snapshot,
    requested: &str,
    target: &Target,
) -> Result<Resolution, GatewayError> {
    Ok(match target {
        Target::Model(resolved) => {
            let model = &resolved.model;
            let route = snap.routing.route_of(model).cloned().ok_or_else(|| {
                GatewayError::new(
                    Code::ModelNotFound,
                    Stage::Resolve,
                    Message::built(format!("The model `{requested}` has no route.")),
                )
            })?;
            Resolution {
                model_name: model.name.clone(),
                policy_route: model.named_route.then(|| model.route.clone()),
                route,
                alias: resolved.alias_params.clone(),
                captures: resolved.captures.clone(),
                known: authorize::candidates(&snap.providers, &snap.routing, model)
                    .into_iter()
                    .cloned()
                    .collect(),
            }
        }
        Target::Direct(d) => Resolution {
            model_name: requested.to_owned(),
            policy_route: None,
            route: Arc::new(RouteRt::direct(requested, &d.provider.name, &d.model)),
            alias: None,
            captures: None,
            known: vec![Arc::clone(d)],
        },
    })
}
