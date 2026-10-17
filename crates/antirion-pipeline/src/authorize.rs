//! Stage `resolve` for a key: the requested name is checked against the key's model lists before
//! anything is looked up, then resolved to a Model or to direct provider addressing, and at least
//! one candidate deployment must pass the key's provider and deployment lists.

use std::sync::Arc;

use antirion_core::Stage;
use antirion_core::UtcMicros;
use antirion_error::reason::hygiene;
use antirion_error::{Code, GatewayError, Message};
use antirion_identity::KeyPermission;
use antirion_identity::access::{ListPair, allowed, region_allowed};
use antirion_identity::reasons;
use antirion_providers::{DeploymentRt, ProviderRt, ProviderTable};
use antirion_router::{
    Caller, Lifecycle, LifecycleState, ModelRt, Resolved, RoutingSnapshot, deployments_of,
};
use antirion_runtime::Snapshot;
use antirion_runtime::identity::{Access, KeyRt};
use http::{HeaderName, HeaderValue};
use serde_json::Value;

/// What a requested name resolved to.
#[derive(Clone, Debug)]
pub(crate) enum Target {
    Model(Box<Resolved>),
    /// `provider/upstream-model`: an implicit single-deployment route.
    Direct(Arc<DeploymentRt>),
}

/// Whether `name` passes every level's model lists.
pub(crate) fn model_allowed(levels: &[&Access], name: &str) -> bool {
    let lists: Vec<&ListPair> = levels.iter().map(|a| &a.models).collect();
    allowed(name, &lists)
}

/// Whether a deployment passes every level's provider and deployment lists.
pub(crate) fn deployment_allowed(levels: &[&Access], d: &DeploymentRt) -> bool {
    let providers: Vec<&ListPair> = levels.iter().map(|a| &a.providers).collect();
    let deployments: Vec<&ListPair> = levels.iter().map(|a| &a.deployments).collect();
    let regions: Vec<&ListPair> = levels.iter().map(|a| &a.regions).collect();
    allowed(&d.provider.name, &providers)
        && allowed(&d.name, &deployments)
        && region_allowed(d.effective_region(), &regions)
}

/// The deployments a Model's route can reach that exist.
pub(crate) fn candidates<'a>(
    table: &'a ProviderTable,
    routing: &RoutingSnapshot,
    model: &ModelRt,
) -> Vec<&'a Arc<DeploymentRt>> {
    let Some(route) = routing.route_of(model) else {
        return Vec::new();
    };
    let mut names = deployments_of(&route.tree);
    names.sort_unstable();
    names.dedup();
    names.iter().filter_map(|n| table.deployments.get(*n)).collect()
}

pub(crate) fn model_not_allowed(requested: &str) -> GatewayError {
    GatewayError::with_reason(
        reasons::MODEL_NOT_ALLOWED,
        Stage::Resolve,
        Message::built(format!("The API key may not use the model `{requested}`.")),
    )
    .param("model")
}

pub(crate) fn no_allowed_deployment() -> GatewayError {
    GatewayError::with_reason(
        reasons::NO_ALLOWED_DEPLOYMENT,
        Stage::Resolve,
        Message::Fixed("The API key may not use any deployment of this model."),
    )
}

/// The provider a `prefix/rest` name addresses: a provider of that name, one mapped to it by
/// `routing.provider_addressing.types`, or the only provider of that type.
fn addressed_provider<'a>(snap: &'a Snapshot, prefix: &str) -> Option<&'a Arc<ProviderRt>> {
    let routing = snap.config.doc.pointer("/routing/provider_addressing");
    if routing.and_then(|r| r.get("enabled")).and_then(Value::as_bool) == Some(false) {
        return None;
    }
    let table = &snap.providers;
    if let Some(p) = table.providers.get(prefix) {
        return Some(p);
    }
    if let Some(name) = routing
        .and_then(|r| r.pointer(&format!("/types/{prefix}")))
        .and_then(Value::as_str)
    {
        return table.providers.get(name);
    }
    let mut of_type = table.providers.values().filter(|p| {
        snap.config
            .doc
            .pointer(&format!("/providers/{}/type", p.name))
            .and_then(Value::as_str)
            == Some(prefix)
    });
    match (of_type.next(), of_type.next()) {
        (Some(p), None) => Some(p),
        _ => None,
    }
}

/// The lifecycle of the resolved Model: past its end of life it is not found; `preview` and
/// `restricted` Models serve only the principals of their access list. Returns the notices a
/// deprecated Model's responses carry.
pub(crate) fn lifecycle(
    target: &Target,
    key: &KeyRt,
    requested: &str,
    now: UtcMicros,
) -> Result<Vec<(HeaderName, HeaderValue)>, GatewayError> {
    let Target::Model(resolved) = target else {
        return Ok(Vec::new());
    };
    let model = &resolved.model;
    if model.lifecycle.retired(now) {
        return Err(GatewayError::with_reason(
            crate::reasons::MODEL_RETIRED,
            Stage::Resolve,
            Message::built(format!("The model `{requested}` has reached its end of life.")),
        )
        .param("model"));
    }
    if !model.admits(caller(key)) {
        return Err(model_not_allowed(requested));
    }
    Ok(deprecation_headers(&model.lifecycle))
}

/// The key's place in the tenancy, as access lists read it.
pub(crate) fn caller(key: &KeyRt) -> Caller<'_> {
    Caller {
        organization: &key.organization,
        team: &key.team,
        project: &key.project,
        key: &key.name,
    }
}

/// `Deprecation` (RFC 9745), `Sunset` (RFC 8594) and the successor `Link` of a deprecated Model.
fn deprecation_headers(l: &Lifecycle) -> Vec<(HeaderName, HeaderValue)> {
    if l.state != LifecycleState::Deprecated {
        return Vec::new();
    }
    let mut out = Vec::new();
    let since = l.since.map_or(0, |t| t.0.div_euclid(1_000_000));
    if let Ok(v) = HeaderValue::from_str(&format!("@{since}")) {
        out.push((HeaderName::from_static("deprecation"), v));
    }
    if let Some(eol) = l.eol_at {
        if let Ok(v) = HeaderValue::from_str(&eol.to_http_date()) {
            out.push((HeaderName::from_static("sunset"), v));
        }
        if let Some(r) = &l.replacement
            && let Ok(v) = HeaderValue::from_str(&format!("</v1/models/{r}>; rel=\"successor-version\""))
        {
            out.push((http::header::LINK, v));
        }
    }
    out
}

/// Checks 10–13 of the key's access for the requested name.
pub(crate) fn resolve(snap: &Snapshot, key: &KeyRt, requested: &str) -> Result<Target, GatewayError> {
    let levels = snap.identity.access_levels(key);
    if !model_allowed(&levels, requested) {
        return Err(model_not_allowed(requested));
    }
    if let Some(resolved) = snap.routing.resolve(requested) {
        let known = candidates(&snap.providers, &snap.routing, &resolved.model);
        if !known.is_empty() && !known.iter().any(|d| deployment_allowed(&levels, d)) {
            return Err(no_allowed_deployment());
        }
        return Ok(Target::Model(Box::new(resolved)));
    }
    if let Some((prefix, rest)) = requested.split_once('/')
        && let Some(provider) = addressed_provider(snap, prefix)
    {
        if rest.is_empty() {
            return Err(GatewayError::with_reason(
                hygiene::INVALID_PARAMETER,
                Stage::Resolve,
                Message::Fixed("A provider-addressed model needs an upstream model after the `/`."),
            )
            .param("model"));
        }
        if !key.permissions.has(KeyPermission::ModelsDirectProvider) {
            return Err(GatewayError::with_reason(
                reasons::PERMISSION_REQUIRED,
                Stage::Resolve,
                Message::Fixed(
                    "Addressing a provider directly needs the `models:direct_provider` permission.",
                ),
            )
            .param("model"));
        }
        let provider_caps = snap
            .config
            .doc
            .pointer(&format!("/providers/{}/capabilities", provider.name));
        let name = format!("{}/{rest}", provider.name);
        let deployment = DeploymentRt::direct(provider, &name, rest, provider_caps);
        if !deployment_allowed(&levels, &deployment) {
            return Err(no_allowed_deployment());
        }
        return Ok(Target::Direct(Arc::new(deployment)));
    }
    Err(GatewayError::new(
        Code::ModelNotFound,
        Stage::Resolve,
        Message::built(format!(
            "The model `{}` does not exist or you do not have access to it.",
            requested.chars().take(256).collect::<String>()
        )),
    )
    .param("model"))
}
