//! The immutable snapshot of one config generation: every engine's compiled part, built from the
//! stored document with its secrets resolved.

use std::sync::Arc;

use antirion_config::secrets::{SecretSource, expand_document};
use antirion_config::{ConfigError, DocPath};
use antirion_identity::Pepper;
use antirion_limits::policy::Policies;
use antirion_providers::ProviderTable;
use antirion_resilience::HealthConfig;
use antirion_router::RoutingSnapshot;
use serde_json::Value;

use crate::identity::{self, IdentitySnapshot};
use crate::security::SecurityRt;
use crate::state::ConfigState;

/// Everything a request reads from configuration, for one generation.
#[derive(Debug)]
pub struct Snapshot {
    pub generation: u64,
    pub hash: String,
    /// The stored document and metadata, for the Admin API's reads.
    pub config: Arc<ConfigState>,
    /// Where the stored document's secret fields are, for redaction.
    pub secret_paths: Arc<Vec<DocPath>>,
    pub providers: Arc<ProviderTable>,
    pub routing: Arc<RoutingSnapshot>,
    /// `resilience.health` with its per-deployment overrides.
    pub health: Arc<HealthConfig>,
    pub identity: Arc<IdentitySnapshot>,
    /// Limit and budget declarations with the `limits`, `budgets` and `pricing` settings.
    pub limits: Arc<Policies>,
    pub security: Arc<SecurityRt>,
    pub mcp: Arc<antirion_mcp::McpSnapshot>,
    pub obs: Arc<antirion_obs::ObsSettings>,
    pub pricing: Arc<crate::pricing::PricingRt>,
    pub guardrails: Arc<antirion_guardrails::Guardrails>,
    pub plugins: Arc<antirion_ext::Plugins>,
    /// The response cache's storage settings, secrets resolved.
    pub cache: Arc<antirion_cache::Settings>,
    /// The alert webhooks, secrets resolved.
    pub alerts: Arc<Vec<crate::telemetry::alerts::AlertWebhook>>,
    /// The GeoIP database, when one is configured and readable.
    pub geoip: Option<Arc<crate::geoip::GeoDb>>,
    /// Alert rules and channels, objectives, audit sinks and OTLP metric export, secrets resolved.
    pub alerting: Arc<crate::alerting::AlertingConfig>,
}

/// Builds the snapshot of `state`: resolves every secret reference on this node, then compiles each
/// part. Any failure rejects the generation as a whole.
pub fn build(
    state: ConfigState,
    secret_paths: Vec<DocPath>,
    secrets: &dyn SecretSource,
    pepper: &Pepper,
) -> Result<Snapshot, Vec<ConfigError>> {
    let resolved = expand_document(&state.doc, secrets)?;
    let mut errors = Vec::new();
    let providers = antirion_providers::compile(&resolved).unwrap_or_else(|e| {
        errors.extend(e);
        ProviderTable::default()
    });
    for p in providers.providers.values().filter(|p| !p.tls.verify) {
        tracing::warn!(provider = %p.name, "upstream TLS verification is off for this provider");
    }
    let identity = identity::compile(&state, &resolved, pepper).unwrap_or_else(|e| {
        errors.extend(e);
        IdentitySnapshot::default()
    });
    let security = SecurityRt::compile(&resolved).unwrap_or_else(|e| {
        errors.extend(e);
        SecurityRt::default()
    });
    let mcp = antirion_mcp::compile(&resolved).unwrap_or_else(|e| {
        errors.extend(e);
        antirion_mcp::McpSnapshot::default()
    });
    let mut routing = antirion_router::compile(&resolved).unwrap_or_else(|e| {
        errors.extend(e);
        RoutingSnapshot::default()
    });
    if let Some(metas) = state.meta.resources.get("models") {
        for (name, m) in &mut routing.models {
            let since = metas
                .get(name)
                .and_then(|meta| meta.lifecycle_at.as_deref().or(Some(meta.updated_at.as_str())))
                .and_then(|t| antirion_core::UtcMicros::parse_rfc3339(t).ok());
            std::sync::Arc::make_mut(m).lifecycle.since = since;
        }
    }
    let limits = Policies::compile(&resolved, &|c, n| state.uid_of(c, n)).unwrap_or_else(|e| {
        errors.extend(e.into_iter().map(|e| ConfigError {
            path: e.path,
            code: antirion_config::DetailCode::InvalidFormat,
            message: e.message,
        }));
        Policies::default()
    });
    let guardrails = antirion_guardrails::compile(&resolved).unwrap_or_else(|e| {
        errors.extend(e);
        antirion_guardrails::Guardrails::default()
    });
    let plugins = antirion_ext::compile(&resolved).unwrap_or_else(|e| {
        errors.extend(e);
        antirion_ext::Plugins::default()
    });
    if !errors.is_empty() {
        return Err(errors);
    }
    let health = HealthConfig::from_doc(&resolved);
    let obs = antirion_obs::ObsSettings::compile(&resolved);
    let pricing = crate::pricing::PricingRt::compile(&resolved);
    let cache = antirion_cache::Settings::from_doc(&resolved);
    let alerts = crate::telemetry::alerts::compile(&resolved);
    let geoip = obs.geoip_database.as_deref().and_then(crate::geoip::GeoDb::load);
    Ok(Snapshot {
        generation: state.generation,
        hash: state.hash(),
        config: Arc::new(state),
        secret_paths: Arc::new(secret_paths),
        providers: Arc::new(providers),
        routing: Arc::new(routing),
        health: Arc::new(health),
        identity: Arc::new(identity),
        limits: Arc::new(limits),
        security: Arc::new(security),
        mcp: Arc::new(mcp),
        obs: Arc::new(obs),
        pricing: Arc::new(pricing),
        guardrails: Arc::new(guardrails),
        plugins: Arc::new(plugins),
        cache: Arc::new(cache),
        alerts: Arc::new(alerts),
        geoip,
        alerting: Arc::new(crate::alerting::AlertingConfig::compile(&resolved)),
    })
}

/// The document as every read path shows it: literal secrets replaced by `[REDACTED]`.
#[must_use]
pub fn redacted(doc: &Value, secret_paths: &[DocPath]) -> Value {
    antirion_config::secrets::redact(doc, secret_paths)
}
