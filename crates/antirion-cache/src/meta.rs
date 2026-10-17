//! What an entry records about the request that wrote it, so the entry can be listed, filtered,
//! capped and invalidated by Model, route, deployment, tenancy, key, end user, tag or metadata
//! without its key. Prompt text is never part of it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How an entry can be found.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Only by its exact key.
    #[default]
    Exact,
    /// Also by the similarity of its request (it has a vector in the semantic store).
    Semantic,
}

impl Mode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Mode::Exact => "exact",
            Mode::Semantic => "semantic",
        }
    }
}

/// A per-scope entry cap the entry counts under: the level that declared `max_entries`
/// (`model/m`, `project/p`, `gateway`) and the cap in force when the entry was written.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Cap {
    pub scope: String,
    pub max: u64,
}

/// The facts of the request that wrote an entry.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EntryMeta {
    #[serde(default)]
    pub mode: Mode,
    /// The digest of the key's tenant scope, part of the backend key.
    pub digest: String,
    /// The Model the request resolved to.
    pub model: String,
    /// The named route whose policies applied, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    /// The policy's `scope` (`project`, `team`, `shared`…).
    pub scope: String,
    pub organization: String,
    pub team: String,
    pub project: String,
    /// The virtual key's name.
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_user: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// The request metadata of the keys invalidation rules name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    /// The similarity threshold of the semantic layer that indexed or served it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub similarity_threshold: Option<f64>,
    /// The configuration level whose semantic layer applied (`gateway`, `model/m`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_group: Option<String>,
    /// The entry a semantic hit answered with, when this one is the copy that hit wrote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caps: Vec<Cap>,
}

/// A dimension value, as dimension sets and invalidation cutoffs name it: `<dimension>/<value>`.
#[must_use]
pub fn dim(dimension: &str, value: &str) -> String {
    format!("{dimension}/{value}")
}

impl EntryMeta {
    /// Every dimension value the entry can be selected by; `deployment` is the entry's.
    #[must_use]
    pub fn dims(&self, deployment: &str) -> Vec<String> {
        let mut out = vec![
            dim("mode", self.mode.as_str()),
            dim("model", &self.model),
            dim("deployment", deployment),
            dim("organization", &self.organization),
            dim("team", &self.team),
            dim("project", &self.project),
            dim("key", &self.key),
        ];
        out.extend(self.route.iter().map(|r| dim("route", r)));
        out.extend(self.end_user.iter().map(|u| dim("end_user", u)));
        out.extend(self.tags.iter().map(|t| dim("tag", t)));
        out.extend(
            self.metadata
                .iter()
                .map(|(k, v)| dim("metadata", &format!("{k}={v}"))),
        );
        if self.mode == Mode::Semantic {
            out.extend(self.semantic_group.iter().map(|g| dim("semantic", g)));
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Whether the semantic layer wrote or served it.
    #[must_use]
    pub fn semantic(&self) -> bool {
        self.mode == Mode::Semantic || self.origin.is_some()
    }
}

/// The input and output token counts of a usage member, in any client dialect's names.
#[must_use]
pub fn tokens(usage: Option<&Value>) -> (u64, u64) {
    let Some(u) = usage else {
        return (0, 0);
    };
    let first = |names: &[&str]| names.iter().find_map(|n| u.get(*n).and_then(Value::as_u64));
    (
        first(&["prompt_tokens", "input_tokens"]).unwrap_or(0),
        first(&["completion_tokens", "output_tokens"]).unwrap_or(0),
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn dims_name_every_selector_once() {
        let m = EntryMeta {
            mode: Mode::Semantic,
            model: "m".into(),
            route: Some("r".into()),
            organization: "o".into(),
            team: "t".into(),
            project: "p".into(),
            key: "k".into(),
            end_user: Some("u1".into()),
            tags: vec!["a".into(), "a".into()],
            metadata: [("v".to_owned(), "2".to_owned())].into(),
            semantic_group: Some("gateway".into()),
            ..EntryMeta::default()
        };
        let d = m.dims("dep");
        for want in [
            "mode/semantic",
            "model/m",
            "route/r",
            "deployment/dep",
            "end_user/u1",
            "tag/a",
            "metadata/v=2",
            "semantic/gateway",
        ] {
            assert!(d.iter().any(|x| x == want), "{want} not in {d:?}");
        }
        assert_eq!(d.iter().filter(|x| *x == "tag/a").count(), 1);
        let text = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<EntryMeta>(&text).unwrap(), m);
    }

    #[test]
    fn tokens_read_every_dialect() {
        assert_eq!(
            tokens(Some(&json!({"prompt_tokens": 3, "completion_tokens": 4}))),
            (3, 4)
        );
        assert_eq!(
            tokens(Some(&json!({"input_tokens": 5, "output_tokens": 6}))),
            (5, 6)
        );
        assert_eq!(tokens(None), (0, 0));
    }
}
