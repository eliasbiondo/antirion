//! The prices of one generation, for the cost a request's record and metrics report: the `pricing`
//! settings, each deployment's own `price`, and the bundled catalog.

use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;

use antirion_limits::catalog::{PriceCatalog, effective_price, multiplier};
use antirion_limits::money::Dec;
use antirion_limits::price::Price;
use antirion_limits::settings::PricingSettings;
use parking_lot::RwLock;
use serde_json::Value;

/// How many effective prices a generation remembers; calls past it compute theirs each time.
const MEMO: usize = 4096;

/// An effective price with its content hash.
#[derive(Debug)]
pub struct Priced {
    price: Price,
    version: String,
}

impl Priced {
    /// [`Price::version`], computed once.
    #[must_use]
    pub fn version(&self) -> String {
        self.version.clone()
    }
}

impl Deref for Priced {
    type Target = Price;

    fn deref(&self) -> &Price {
        &self.price
    }
}

/// The prices of one generation.
#[derive(Debug)]
pub struct PricingRt {
    pub settings: PricingSettings,
    deployments: HashMap<String, Price>,
    catalog: PriceCatalog,
    memo: RwLock<Memo>,
}

/// The effective prices of one deployment and upstream model, by provider type.
type ByType = HashMap<String, Option<Arc<Priced>>>;

/// Effective prices by deployment, upstream model and provider type, looked up without allocating.
#[derive(Debug, Default)]
struct Memo {
    prices: HashMap<String, HashMap<String, ByType>>,
    len: usize,
}

impl Memo {
    fn get(
        &self,
        provider_type: &str,
        upstream_model: &str,
        deployment: &str,
    ) -> Option<&Option<Arc<Priced>>> {
        self.prices
            .get(deployment)?
            .get(upstream_model)?
            .get(provider_type)
    }

    fn insert(
        &mut self,
        provider_type: &str,
        upstream_model: &str,
        deployment: &str,
        p: Option<Arc<Priced>>,
    ) {
        if self.len >= MEMO {
            return;
        }
        self.len += 1;
        self.prices
            .entry(deployment.to_owned())
            .or_default()
            .entry(upstream_model.to_owned())
            .or_default()
            .insert(provider_type.to_owned(), p);
    }
}

impl Default for PricingRt {
    fn default() -> PricingRt {
        PricingRt::compile(&Value::Null)
    }
}

impl PricingRt {
    /// The prices of a resolved document; entries that do not parse are left out (validation has
    /// already refused them).
    #[must_use]
    pub fn compile(doc: &Value) -> PricingRt {
        let settings = PricingSettings::from_value(doc.get("pricing"))
            .or_else(|_| PricingSettings::from_value(None))
            .unwrap_or_else(|_| PricingSettings {
                bundled: true,
                on_unpriced: antirion_limits::settings::OnUnpriced::RejectIfBudgeted,
                report_in_body: false,
                multipliers: Default::default(),
                tools: Default::default(),
                agents: Default::default(),
                models: Default::default(),
            });
        let deployments = doc
            .get("deployments")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(name, d)| Some((name.clone(), Price::parse(d.get("price")?).ok()?)))
            .collect();
        PricingRt {
            settings,
            deployments,
            catalog: PriceCatalog::of(antirion_catalog::Catalog::from_doc(doc)),
            memo: RwLock::new(Memo::default()),
        }
    }

    /// The price of a call to `upstream_model` on `deployment` of a provider of `provider_type`.
    #[must_use]
    pub fn price(&self, provider_type: &str, upstream_model: &str, deployment: &str) -> Option<Arc<Priced>> {
        if let Some(p) = self.memo.read().get(provider_type, upstream_model, deployment) {
            return p.clone();
        }
        let priced = effective_price(
            &self.settings,
            Some(&self.catalog),
            provider_type,
            upstream_model,
            self.deployments.get(deployment),
        )
        .map(|price| {
            Arc::new(Priced {
                version: price.version(),
                price,
            })
        });
        self.memo
            .write()
            .insert(provider_type, upstream_model, deployment, priced.clone());
        priced
    }

    /// The price multiplier of a request by `key`.
    #[must_use]
    pub fn multiplier(&self, organization: &str, team: &str, project: &str, key: &str) -> Dec {
        multiplier(&self.settings, organization, team, project, key)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn prices_are_remembered_with_their_version() {
        let p = PricingRt::compile(&json!({
            "deployments": {"d": {"provider": "p", "model": "m", "price": {"input": "1.00", "output": "2.00"}}}
        }));
        let a = p.price("openai", "m", "d").unwrap();
        let b = p.price("openai", "m", "d").unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(a.version(), a.price.version());
        assert!(p.price("openai", "no-such-model", "none").is_none());
        assert!(p.price("openai", "no-such-model", "none").is_none());
    }
}
