//! Operator-configured labels: tenant labels on the families that accept them, and custom labels
//! read from request metadata through an allow-list, so a family's series stay bounded.

use std::sync::Arc;

use serde_json::{Map, Value};

use super::catalog::Fam;

/// The tenant labels, in the order they follow a family's own labels.
pub const TENANT_LABELS: [&str; 3] = ["organization", "team", "project"];

/// Families that take tenant labels, and that a custom label may name.
pub const LABELLED_FAMILIES: [Fam; 4] = [Fam::Requests, Fam::RequestDuration, Fam::Tokens, Fam::Cost];

/// Families a custom label goes on when it names none.
pub const DEFAULT_CUSTOM_FAMILIES: [Fam; 2] = [Fam::Requests, Fam::Tokens];

/// One entry of `observability.metrics.custom_labels`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomLabel {
    pub name: String,
    /// The top-level `x-antirion-metadata` member the value comes from.
    pub from_metadata: String,
    pub allowed_values: Vec<String>,
    /// The value for anything outside the allow-list, or no value at all.
    pub other_value: String,
    /// Family names; empty means [`DEFAULT_CUSTOM_FAMILIES`].
    pub families: Vec<String>,
}

impl CustomLabel {
    fn labels(&self, fam: Fam) -> bool {
        let name = fam.def().name;
        if self.families.is_empty() {
            DEFAULT_CUSTOM_FAMILIES.contains(&fam)
        } else {
            self.families.iter().any(|f| f == name) && LABELLED_FAMILIES.contains(&fam)
        }
    }

    fn value<'a>(&'a self, metadata: Option<&'a Map<String, Value>>) -> &'a str {
        metadata
            .and_then(|m| m.get(&self.from_metadata))
            .and_then(Value::as_str)
            .and_then(|v| self.allowed_values.iter().find(|a| *a == v))
            .map_or(self.other_value.as_str(), String::as_str)
    }
}

/// A label a family carries beyond the catalogue's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Extra {
    /// Index into [`TENANT_LABELS`].
    Tenant(usize),
    Custom(Arc<CustomLabel>),
}

impl Extra {
    pub(super) fn name(&self) -> &str {
        match self {
            Extra::Tenant(i) => TENANT_LABELS.get(*i).copied().unwrap_or_default(),
            Extra::Custom(c) => &c.name,
        }
    }

    pub(super) fn value<'a>(&'a self, src: Option<&LabelSource<'a>>) -> &'a str {
        match self {
            Extra::Tenant(i) => src.and_then(|s| s.tenant.get(*i).copied()).unwrap_or_default(),
            Extra::Custom(c) => c.value(src.and_then(|s| s.metadata)),
        }
    }
}

/// What one request offers the configured labels.
#[derive(Clone, Copy, Debug, Default)]
pub struct LabelSource<'a> {
    /// Organization, team and project.
    pub tenant: [&'a str; 3],
    pub metadata: Option<&'a Map<String, Value>>,
}

/// The extra labels of `fam`: the configured tenant labels, then the custom labels naming it, by name.
pub(super) fn extras(fam: Fam, tenant: &[String], custom: &[Arc<CustomLabel>]) -> Vec<Extra> {
    if !LABELLED_FAMILIES.contains(&fam) {
        return Vec::new();
    }
    let mut out: Vec<Extra> = TENANT_LABELS
        .iter()
        .enumerate()
        .filter(|(_, t)| tenant.iter().any(|c| c == *t))
        .map(|(i, _)| Extra::Tenant(i))
        .collect();
    let mut named: Vec<&Arc<CustomLabel>> = custom.iter().filter(|c| c.labels(fam)).collect();
    named.sort_by(|a, b| a.name.cmp(&b.name));
    out.extend(named.into_iter().map(|c| Extra::Custom(Arc::clone(c))));
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn env() -> Arc<CustomLabel> {
        Arc::new(CustomLabel {
            name: "environment".into(),
            from_metadata: "env".into(),
            allowed_values: vec!["prod".into(), "dev".into()],
            other_value: "_other".into(),
            families: Vec::new(),
        })
    }

    #[test]
    fn custom_values_are_allow_listed() {
        let label = env();
        let m = json!({"env": "prod"});
        assert_eq!(label.value(m.as_object()), "prod");
        let m = json!({"env": "weird"});
        assert_eq!(label.value(m.as_object()), "_other");
        let m = json!({"env": 3});
        assert_eq!(label.value(m.as_object()), "_other");
        assert_eq!(label.value(None), "_other");
    }

    #[test]
    fn extras_follow_the_families_and_order() {
        let tenant = vec!["project".to_owned(), "organization".to_owned()];
        let custom = vec![env()];
        let names = |f| {
            extras(f, &tenant, &custom)
                .iter()
                .map(|e| e.name().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(Fam::Requests), ["organization", "project", "environment"]);
        assert_eq!(names(Fam::RequestDuration), ["organization", "project"]);
        assert!(names(Fam::Overhead).is_empty());
        let src = LabelSource {
            tenant: ["acme", "t", "p"],
            metadata: None,
        };
        let e = extras(Fam::Cost, &tenant, &custom);
        assert_eq!(e[1].value(Some(&src)), "p");
    }
}
