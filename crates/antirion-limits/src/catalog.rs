//! The catalog prices and the effective price of a deployment.

use std::sync::LazyLock;

use antirion_catalog::Catalog;

use crate::money::Dec;
use crate::price::Price;
use crate::settings::PricingSettings;

/// The prices of a model catalog, looked up like its capability values: `"<provider_type>/<model>"`,
/// then `"<model>"`, then the longest `*` prefix.
#[derive(Clone, Debug)]
pub struct PriceCatalog {
    catalog: Catalog,
}

static BUNDLED: LazyLock<PriceCatalog> = LazyLock::new(|| PriceCatalog {
    catalog: Catalog::bundled(),
});

impl PriceCatalog {
    /// The catalog shipped with this build.
    #[must_use]
    pub fn bundled() -> &'static PriceCatalog {
        &BUNDLED
    }

    /// The prices of an effective catalog.
    #[must_use]
    pub fn of(catalog: Catalog) -> PriceCatalog {
        PriceCatalog { catalog }
    }

    /// The version of the bundled prices.
    #[must_use]
    pub fn price_version(&self) -> &str {
        &self.catalog.bundled.price_version
    }

    fn lookup(&self, provider_type: &str, model: &str) -> Option<Price> {
        let (entry, _) = self.catalog.lookup(provider_type, model)?;
        Price::parse(entry.price.as_ref()?).ok()
    }
}

/// The effective price of a call on a deployment: every level merged field by field, the
/// deployment's own `price` first, then `pricing.models["<provider_type>/<model>"]`, then
/// `pricing.models["<model>"]`, then the bundled catalog. `None` when no level prices it.
#[must_use]
pub fn effective_price(
    pricing: &PricingSettings,
    catalog: Option<&PriceCatalog>,
    provider_type: &str,
    upstream_model: &str,
    deployment_price: Option<&Price>,
) -> Option<Price> {
    let from_catalog = catalog
        .filter(|_| pricing.bundled)
        .and_then(|c| c.lookup(provider_type, upstream_model));
    let levels = [
        deployment_price,
        pricing.models.get(&format!("{provider_type}/{upstream_model}")),
        pricing.models.get(upstream_model),
        from_catalog.as_ref(),
    ];
    let mut out: Option<Price> = None;
    for p in levels.into_iter().flatten().rev() {
        out = Some(match out {
            Some(lower) => p.over(&lower),
            None => p.clone(),
        });
    }
    out
}

/// The per-call price of an MCP tool: the exact `"<server>/<tool>"` key wins over `"<server>/*"`.
#[must_use]
pub fn tool_price(pricing: &PricingSettings, server: &str, tool: &str) -> Option<Dec> {
    pricing
        .tools
        .get(&format!("{server}/{tool}"))
        .or_else(|| pricing.tools.get(&format!("{server}/*")))
        .copied()
}

/// The narrowest price multiplier for a request: key, then project, team, organization; 1 when
/// none is set.
#[must_use]
pub fn multiplier(
    pricing: &PricingSettings,
    organization: &str,
    team: &str,
    project: &str,
    key: &str,
) -> Dec {
    [
        ("keys", key),
        ("projects", project),
        ("teams", team),
        ("organizations", organization),
    ]
    .into_iter()
    .find_map(|(level, name)| pricing.multipliers.get(level)?.get(name).copied())
    .unwrap_or(Dec::ONE)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::price::Rate;

    fn d(s: &str) -> Dec {
        Dec::parse(s).unwrap()
    }

    #[test]
    fn bundled_catalog_loads() {
        let c = PriceCatalog::bundled();
        assert!(!c.price_version().is_empty());
        assert!(c.lookup("openai", "gpt-4o").is_some());
        assert!(
            c.lookup("anthropic", "claude-sonnet-4-5")
                .is_some_and(|p| !p.tiers.is_empty())
        );
    }

    #[test]
    fn precedence_is_field_by_field() {
        let pricing = PricingSettings::from_value(Some(&json!({
            "models": {"gpt-4o": {"input": "2.50"}, "azure/gpt-4o": {"output": "9.00"}}
        })))
        .unwrap();
        let cat = PriceCatalog::bundled();
        let openai = effective_price(&pricing, Some(cat), "openai", "gpt-4o", None).unwrap();
        assert_eq!(openai.base.get(Rate::Input), Some(d("2.50")));
        assert_eq!(openai.base.get(Rate::Output), Some(d("10.00")));
        let own = Price::parse(&json!({"input": "2.00"})).unwrap();
        let azure = effective_price(&pricing, Some(cat), "azure", "gpt-4o", Some(&own)).unwrap();
        assert_eq!(azure.base.get(Rate::Input), Some(d("2.00")));
        assert_eq!(azure.base.get(Rate::Output), Some(d("9.00")));
        assert!(effective_price(&pricing, Some(cat), "openai", "unknown-model", None).is_none());
        let none = PricingSettings::from_value(Some(&json!({"catalog": "none"}))).unwrap();
        assert!(effective_price(&none, Some(cat), "openai", "gpt-4o", None).is_none());
    }

    #[test]
    fn tools_and_multipliers() {
        let pricing = PricingSettings::from_value(Some(&json!({
            "tools": {"search/*": {"per_call": "0.01"}, "search/web": {"per_call": "0.02"}},
            "multipliers": {"teams": {"t": "0.5"}, "keys": {"k": "2"}}
        })))
        .unwrap();
        assert_eq!(tool_price(&pricing, "search", "web"), Some(d("0.02")));
        assert_eq!(tool_price(&pricing, "search", "news"), Some(d("0.01")));
        assert_eq!(tool_price(&pricing, "other", "x"), None);
        assert_eq!(multiplier(&pricing, "o", "t", "p", "k"), d("2"));
        assert_eq!(multiplier(&pricing, "o", "t", "p", "x"), d("0.5"));
        assert_eq!(multiplier(&pricing, "o", "u", "p", "x"), Dec::ONE);
    }
}
