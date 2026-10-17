//! The config rules of limits, budgets and pricing that the schema cannot express: every
//! declaration compiles, prices parse with increasing tiers, and no budget exceeds a parent budget of
//! the same unit and period unless it says so.

use antirion_config::{ConfigError, DetailCode};
use antirion_core::ResourceUid;
use serde_json::Value;

use crate::policy::{CompileError, Policies, budgets_above_parents};
use crate::price::{Price, PriceError};

fn error(e: CompileError, code: DetailCode) -> ConfigError {
    ConfigError {
        path: e.path,
        code,
        message: e.message,
    }
}

fn prices(doc: &Value, out: &mut Vec<ConfigError>) {
    let mut check = |path: String, v: &Value| {
        if let Err(e) = Price::parse(v) {
            let path = match &e {
                PriceError::TierOrder => format!("{path}.tiers"),
                PriceError::Shape(field) if field == "price" => path,
                PriceError::Decimal { field, .. } | PriceError::Shape(field) => format!("{path}.{field}"),
            };
            out.push(ConfigError {
                path,
                code: DetailCode::InvalidFormat,
                message: e.to_string(),
            });
        }
    };
    for (name, d) in doc
        .get("deployments")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        if let Some(price) = d.get("price") {
            check(format!("deployments.{name}.price"), price);
        }
    }
    for (name, price) in doc
        .pointer("/pricing/models")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        check(format!("pricing.models.{name}"), price);
    }
    for (pattern, entry) in doc
        .pointer("/catalog/models")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        if let Some(price) = entry.get("price") {
            check(format!("catalog.models.{pattern}.price"), price);
        }
    }
}

/// The errors of a validated document's limits, budgets and prices.
#[must_use]
pub fn check_document(doc: &Value) -> Vec<ConfigError> {
    let mut out = Vec::new();
    if let Err(errors) = Policies::compile(doc, &|_, _| ResourceUid(0)) {
        out.extend(errors.into_iter().map(|e| error(e, DetailCode::InvalidFormat)));
    }
    out.extend(
        budgets_above_parents(doc)
            .into_iter()
            .map(|e| error(e, DetailCode::OutOfRange)),
    );
    prices(doc, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn errors_name_the_field() {
        let doc = json!({
            "policies": {"limits": {"x": {"unit": "requests", "limit": 1, "per": "500ms"}},
                         "budgets": {"b": {"unit": "usd", "amount": "1.00", "period": "daily", "timezone": "Mars/Olympus"}}},
            "deployments": {"p": {"price": {"input": "1", "tiers": [
                {"above_input_tokens": 200, "input": "2"}, {"above_input_tokens": 100, "input": "3"}]}}}
        });
        let paths: Vec<String> = check_document(&doc).into_iter().map(|e| e.path).collect();
        assert!(paths.contains(&"policies.limits.x.per".to_owned()), "{paths:?}");
        assert!(
            paths.contains(&"policies.budgets.b.timezone".to_owned()),
            "{paths:?}"
        );
        assert!(
            paths.contains(&"deployments.p.price.tiers".to_owned()),
            "{paths:?}"
        );
        assert!(check_document(&json!({})).is_empty());
    }
}
